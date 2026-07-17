//! Дифф файла через git для таба «Изменения» вьюера документов (спека
//! 2026-07-18 §3.2). Никакого diff-алгоритма у себя: unified-вывод git
//! парсится в структурные ханки, UI только красит.
//!
//! Безопасность (§4): путь файла приходит из транскрипта (недоверенный),
//! поэтому ВСЕ вызовы — строго `git -C <cwd> … -- <файл>`: файл только после
//! `--`, чтобы он не интерпретировался как ревизия или флаг. Спавн без шелла;
//! зависший git убиваем по таймауту.

use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Строка ханка: t — " " (контекст) | "-" (удалена) | "+" (добавлена).
#[derive(Debug, PartialEq, Serialize)]
pub struct Line {
    pub t: String,
    pub s: String,
}

/// Ханк unified-диффа в структурном виде — без заголовков
/// diff --git/index/---/+++ (UI они не нужны).
#[derive(Debug, PartialEq, Serialize)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<Line>,
}

/// Итог диффа: mode — "worktree" (незакоммиченные правки) | "commit"
/// (последний коммит, тронувший файл) | "none" (не в git / бинарь / git
/// недоступен — таба «Изменения» просто нет).
pub struct FileDiff {
    pub mode: &'static str,
    pub label: String,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn none() -> Self {
        FileDiff {
            mode: "none",
            label: String::new(),
            hunks: Vec::new(),
        }
    }
}

const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// stdout git-команды `git -C <cwd> <args> -- <file>`. Err — git отсутствует,
/// упал (не репо, файл вне репо, нет HEAD…) или завис дольше таймаута; для
/// вьюера все эти случаи одинаковы — «диффа нет».
fn git_out(cwd: &str, args: &[&str], file: &Path) -> Result<String, ()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .arg("--")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    // stdout читаем отдельным потоком: заполненный пайп заблокировал бы git,
    // и опрос try_wait ниже никогда не дождался бы завершения
    let mut pipe = child.stdout.take().ok_or(())?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        pipe.read_to_end(&mut buf).ok();
        buf
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().unwrap_or_default();
                return if status.success() {
                    Ok(String::from_utf8_lossy(&buf).into_owned())
                } else {
                    Err(())
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

/// Дифф файла относительно git-репозитория, найденного от cwd сессии (сам
/// `git -C` поднимается к корню, как обычный git из подкаталога). Логика §3.2:
/// есть незакоммиченные правки файла → `diff HEAD` (главный кейс: агент
/// наделал, человек ревьюит до коммита); дерево по файлу чистое → дифф
/// последнего коммита, тронувшего файл; не в git / бинарь → none.
pub fn diff_for_file(cwd: &str, file: &Path) -> FileDiff {
    let Ok(status) = git_out(cwd, &["status", "--porcelain"], file) else {
        return FileDiff::none(); // не репо / файл вне репо / git недоступен
    };
    let status = status.trim();
    if status.starts_with("??") {
        return FileDiff::none(); // неотслеживаемый файл — git его не знает
    }
    if !status.is_empty() {
        let Ok(out) = git_out(cwd, &["diff", "HEAD"], file) else {
            return FileDiff::none();
        };
        let hunks = parse_unified(&out);
        if hunks.is_empty() {
            return FileDiff::none(); // бинарь («Binary files … differ») или только права
        }
        return FileDiff {
            mode: "worktree",
            label: "незакоммиченные изменения".into(),
            hunks,
        };
    }
    // дерево по файлу чистое → последний коммит, тронувший файл
    let Ok(last) = git_out(cwd, &["log", "-1", "--format=%h%x09%s"], file) else {
        return FileDiff::none();
    };
    let last = last.trim();
    let Some((sha, subject)) = last.split_once('\t') else {
        return FileDiff::none(); // файла нет в истории
    };
    let Ok(out) = git_out(cwd, &["show", sha], file) else {
        return FileDiff::none();
    };
    let hunks = parse_unified(&out);
    if hunks.is_empty() {
        return FileDiff::none();
    }
    FileDiff {
        mode: "commit",
        label: format!("коммит {sha} · {subject}"),
        hunks,
    }
}

/// Парс unified-вывода git в ханки. Заголовки (diff --git/index/---/+++ и
/// шапка коммита у `git show`) не отдаются; «Binary files … differ» ханков не
/// образует. Счётчики @@-заголовка задают точную длину ханка, поэтому строка
/// коммит-сообщения, начинающаяся с пробела или минуса, в ханк не просочится.
fn parse_unified(out: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_left, mut new_left) = (0u32, 0u32); // сколько строк ханка ещё ждём
    for line in out.lines() {
        if let Some((old_start, old_count, new_start, new_count)) = hunk_header(line) {
            old_left = old_count;
            new_left = new_count;
            hunks.push(Hunk {
                old_start,
                new_start,
                lines: Vec::new(),
            });
            continue;
        }
        if old_left == 0 && new_left == 0 {
            continue; // между ханками: заголовки диффа, шапка коммита
        }
        if line.starts_with('\\') {
            continue; // «\ No newline at end of file» — служебная пометка
        }
        let (t, s) = match line.as_bytes().first() {
            Some(b' ') | None => {
                // пустой line — контекстная пустая строка (git шлёт " ", но не режем)
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                (" ", line.get(1..).unwrap_or(""))
            }
            Some(b'-') => {
                old_left = old_left.saturating_sub(1);
                ("-", &line[1..])
            }
            Some(b'+') => {
                new_left = new_left.saturating_sub(1);
                ("+", &line[1..])
            }
            _ => {
                // повреждённый вывод — ханк обрываем, дальше ждём заголовок
                old_left = 0;
                new_left = 0;
                continue;
            }
        };
        if let Some(h) = hunks.last_mut() {
            h.lines.push(Line {
                t: t.into(),
                s: s.into(),
            });
        }
    }
    hunks
}

/// «@@ -l[,c] +l[,c] @@ …» → (old_start, old_count, new_start, new_count).
fn hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (os, oc) = start_count(old)?;
    let (ns, nc) = start_count(new)?;
    Some((os, oc, ns, nc))
}

/// «l» или «l,c» — счётчик по умолчанию 1 (unified так сокращает).
fn start_count(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- парсер unified (чистые фикстуры, git не нужен) ---

    #[test]
    fn parses_two_hunks_and_skips_headers() {
        let out = "diff --git a/a.md b/a.md\n\
                   index 111..222 100644\n\
                   --- a/a.md\n\
                   +++ b/a.md\n\
                   @@ -1,3 +1,3 @@\n \
                   один\n-два\n+ДВА\n\
                   @@ -10,2 +10,3 @@\n \
                   десять\n+вставка\n \
                   одиннадцать\n";
        let hunks = parse_unified(out);
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        assert_eq!(
            hunks[0].lines,
            vec![
                Line { t: " ".into(), s: "один".into() },
                Line { t: "-".into(), s: "два".into() },
                Line { t: "+".into(), s: "ДВА".into() },
            ]
        );
        assert_eq!((hunks[1].old_start, hunks[1].new_start), (10, 10));
        assert_eq!(hunks[1].lines.len(), 3);
    }

    #[test]
    fn skips_commit_header_of_git_show() {
        // шапка `git show`: строки сообщения с отступом — вне счётчиков ханка
        let out = "commit abcdef\nAuthor: t <t@t>\nDate: now\n\n    \
                   тема коммита\n\n    - пункт с минусом\n\n\
                   diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n\
                   @@ -1 +1 @@\n-старое\n+новое\n";
        let hunks = parse_unified(out);
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunks[0].lines,
            vec![
                Line { t: "-".into(), s: "старое".into() },
                Line { t: "+".into(), s: "новое".into() },
            ]
        );
    }

    #[test]
    fn handles_no_newline_marker_and_new_file() {
        // новый файл: @@ -0,0 +1,2 @@, счётчик без запятой = 1
        let out = "@@ -0,0 +1,2 @@\n+раз\n+два\n\\ No newline at end of file\n";
        let hunks = parse_unified(out);
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (0, 1));
        assert_eq!(hunks[0].lines.len(), 2, "пометка \\ не строка");
    }

    #[test]
    fn binary_diff_yields_no_hunks() {
        let out = "diff --git a/b.bin b/b.bin\nindex 111..222 100644\n\
                   Binary files a/b.bin and b/b.bin differ\n";
        assert!(parse_unified(out).is_empty());
    }

    // --- diff_for_file: настоящий временный git-репозиторий ---

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-gitdiff-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // канонизируем сразу: file_diff сверяет канонические пути, а temp_dir
        // на macOS может проходить через симлинк
        d.canonicalize().unwrap()
    }

    // git в тестовом репо; -c подменяет автора — глобальный конфиг не нужен
    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn repo_with_commit(name: &str, file: &str, content: &str, msg: &str) -> PathBuf {
        let d = tmp_dir(name);
        git(&d, &["init", "-q"]);
        std::fs::write(d.join(file), content).unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-q", "-m", msg]);
        d
    }

    #[test]
    fn worktree_mode_for_uncommitted_changes() {
        let d = repo_with_commit("wt", "a.md", "один\nдва\nтри\n", "база");
        std::fs::write(d.join("a.md"), "один\nдва!\nтри\n").unwrap();
        let res = diff_for_file(d.to_str().unwrap(), &d.join("a.md"));
        assert_eq!(res.mode, "worktree");
        assert_eq!(res.label, "незакоммиченные изменения");
        let lines: Vec<_> = res.hunks.iter().flat_map(|h| h.lines.iter()).collect();
        assert!(lines.iter().any(|l| l.t == "-" && l.s == "два"), "{lines:?}");
        assert!(lines.iter().any(|l| l.t == "+" && l.s == "два!"), "{lines:?}");
    }

    #[test]
    fn commit_mode_for_clean_tree() {
        let d = repo_with_commit("ci", "a.md", "один\n", "база");
        std::fs::write(d.join("a.md"), "один\nдва\n").unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-q", "-m", "правка дока"]);
        let res = diff_for_file(d.to_str().unwrap(), &d.join("a.md"));
        assert_eq!(res.mode, "commit");
        assert!(res.label.starts_with("коммит "), "{}", res.label);
        assert!(res.label.ends_with("· правка дока"), "{}", res.label);
        let lines: Vec<_> = res.hunks.iter().flat_map(|h| h.lines.iter()).collect();
        assert!(lines.iter().any(|l| l.t == "+" && l.s == "два"), "{lines:?}");
    }

    #[test]
    fn none_outside_git_repo() {
        let d = tmp_dir("norepo");
        std::fs::write(d.join("a.md"), "текст").unwrap();
        let res = diff_for_file(d.to_str().unwrap(), &d.join("a.md"));
        assert_eq!(res.mode, "none");
        assert!(res.hunks.is_empty());
    }

    #[test]
    fn none_for_untracked_file() {
        let d = repo_with_commit("untracked", "a.md", "есть\n", "база");
        std::fs::write(d.join("новый.md"), "не в git").unwrap();
        let res = diff_for_file(d.to_str().unwrap(), &d.join("новый.md"));
        assert_eq!(res.mode, "none");
    }

    #[test]
    fn none_for_binary_file() {
        let d = tmp_dir("bin");
        git(&d, &["init", "-q"]);
        std::fs::write(d.join("b.bin"), [0u8, 1, 2, 159, 146, 150]).unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-q", "-m", "бинарь"]);
        std::fs::write(d.join("b.bin"), [0u8, 7, 7, 7, 7, 7]).unwrap();
        // git отвечает «Binary files … differ» — ханков нет, режим none
        let res = diff_for_file(d.to_str().unwrap(), &d.join("b.bin"));
        assert_eq!(res.mode, "none");
    }
}
