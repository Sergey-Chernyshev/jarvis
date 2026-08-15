//! Где живёт связка: эта машина или узел по ssh.
//!
//! Одна абстракция на два мира, и граница проведена по способу исполнения:
//! локально команды идут argv-массивом (никакого шелла — в путях бывают
//! пробелы и кавычки), по ssh шелл неизбежен — там каждый аргумент проходит
//! через `shell_quote`. Всё остальное (git-логика, гейты, такт) одинаково и
//! не знает, где исполняется.

use std::process::Stdio;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Host {
    Local,
    /// Узел: имя (для реестра сессий) и ssh-хост (для исполнения).
    Ssh { machine: String, host: String },
}

impl Host {
    pub fn machine(&self) -> &str {
        match self {
            Host::Local => "local",
            Host::Ssh { machine, .. } => machine,
        }
    }

    /// Команда шеллом в каталоге. Гейты и инициализация — только так: их
    /// пишет человек, и он вправе писать пайпы.
    pub async fn sh(&self, cwd: &str, cmd: &str, timeout: Duration) -> (i32, String) {
        match self {
            Host::Local => {
                crate::loops::runner::shell(std::path::Path::new(cwd), cmd, timeout).await
            }
            Host::Ssh { host, .. } => {
                let full = format!("cd {} && {{ {cmd}\n}}", crate::util::shell_quote(cwd));
                ssh(host, &full, timeout).await
            }
        }
    }

    /// `git -C dir <args>`: локально — argv как есть, по ssh — с цитированием
    /// каждого аргумента.
    pub async fn git(&self, dir: &str, args: &[&str]) -> (i32, String) {
        let (code, out, err) = self.git_split(dir, args).await;
        (code, merge(out, err))
    }

    /// То же, но потоками врозь: stdout — ДАННЫЕ, stderr — диагностика.
    ///
    /// Разделять обязательно везде, где вывод РАЗБИРАЮТ. Удалённый `bash -lc`
    /// ворчит на старте («setlocale: LC_ALL: cannot change locale»), git — про
    /// CRLF и detached HEAD; в слитом потоке это ворчание становится строкой
    /// `git status --porcelain`, то есть выдуманным файлом в списке правок, и
    /// веткой по имени «master bash: warning: …». Ровно это панель и
    /// показывала.
    pub async fn git_split(&self, dir: &str, args: &[&str]) -> (i32, String, String) {
        match self {
            Host::Local => local_git(dir, args).await,
            Host::Ssh { host, .. } => {
                let mut full = format!("git -C {}", crate::util::shell_quote(dir));
                for a in args {
                    full.push(' ');
                    full.push_str(&crate::util::shell_quote(a));
                }
                ssh_split(host, &full, Duration::from_secs(120)).await
            }
        }
    }
}

async fn local_git(dir: &str, args: &[&str]) -> (i32, String, String) {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    collect_split(cmd, Duration::from_secs(120)).await
}

/// `ssh -o BatchMode=yes host bash -lc <строка>`. BatchMode — принципиально:
/// такт крутится без человека, и ssh, вставший с вопросом про пароль, повесил
/// бы связку целиком. Не настроен ключ — честная ошибка в ленту.
async fn ssh(host: &str, script: &str, timeout: Duration) -> (i32, String) {
    let (code, out, err) = ssh_split(host, script, timeout).await;
    (code, merge(out, err))
}

/// То же, но потоки раздельно: stdout — данные, stderr — шум и диагностика.
async fn ssh_split(host: &str, script: &str, timeout: Duration) -> (i32, String, String) {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.arg("-o")
        .arg("BatchMode=yes")
        // Локаль этой машины не должна ехать на узел: там её может не быть, и
        // каждый запуск bash ругался бы в stderr «cannot change locale» —
        // ровно этот мусор и прилипал к списку каталогов. Синтаксис с минусом
        // понимает OpenSSH 8.7+; старому он безвреден: шаблон «-LC_*» просто
        // ничего не матчит.
        .arg("-o")
        .arg("SendEnv=-LC_*")
        .arg("-o")
        .arg("SendEnv=-LANG")
        .arg(host)
        .arg("bash")
        .arg("-lc")
        .arg(crate::util::shell_quote(script))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    collect_split(cmd, timeout).await
}

/// Слить потоки в один текст — для диагностики: у git и гейтов она в stderr,
/// и терять её нельзя.
fn merge(mut out: String, err: String) -> String {
    if !err.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(err.trim_end());
    }
    out
}

async fn collect_split(mut cmd: tokio::process::Command, timeout: Duration) -> (i32, String, String) {
    let Ok(Ok(out)) = tokio::time::timeout(timeout, cmd.output()).await else {
        return (-1, String::new(), format!("не уложилось в {} с", timeout.as_secs()));
    };
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

impl Host {
    /// Команда, чей stdout — ДАННЫЕ, а не диагностика.
    ///
    /// Отдельно от [`Host::sh`] принципиально: там потоки сливаются, потому
    /// что у git и гейтов диагностика живёт в stderr и терять её нельзя. А
    /// здесь наоборот — stderr это шум (удалённый bash любит поворчать про
    /// локаль на старте), и подмешивать его к данным значит показывать
    /// «/home/desktop bash: warning: setlocale…» вместо пути.
    pub async fn sh_data(&self, cwd: &str, cmd: &str, timeout: Duration) -> Result<String, String> {
        let (code, out, err) = match self {
            Host::Local => {
                let mut c = tokio::process::Command::new("/bin/sh");
                c.arg("-lc")
                    .arg(cmd)
                    .current_dir(cwd)
                    .env("JARVIS_IGNORE", "1")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                collect_split(c, timeout).await
            }
            Host::Ssh { host, .. } => {
                let full = format!("cd {} && {{ {cmd}\n}}", crate::util::shell_quote(cwd));
                ssh_split(host, &full, timeout).await
            }
        };
        if code == 0 {
            Ok(out)
        } else {
            Err(crate::util::ellipsize(&crate::util::one_line(&merge(out, err)), 300))
        }
    }

    /// Домашний каталог машины — с него начинается обзор.
    pub async fn home(&self) -> Result<String, String> {
        match self {
            Host::Local => std::env::var("HOME").map_err(|_| "не знаю $HOME".into()),
            Host::Ssh { .. } => {
                let out = self.sh_data("/", "printf %s \"$HOME\"", Duration::from_secs(15)).await?;
                // Первая строка stdout и ничего больше: любые приветствия из
                // rc-файлов не должны становиться частью пути.
                let home = out.lines().next().unwrap_or("").trim().to_string();
                if home.starts_with('/') {
                    Ok(home)
                } else {
                    Err(format!("не узнал $HOME узла: {}", crate::util::one_line(&out)))
                }
            }
        }
    }

    /// Подкаталоги пути — для обзора. Скрытые не показываем: их набирают
    /// руками в поле, обзор — про обычные проекты.
    pub async fn list_dirs(&self, path: &str) -> Result<Vec<String>, String> {
        match self {
            Host::Local => {
                let mut out = Vec::new();
                let rd = std::fs::read_dir(path).map_err(|e| format!("не прочитал каталог: {e}"))?;
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        out.push(name);
                    }
                }
                out.sort();
                Ok(out)
            }
            Host::Ssh { .. } => {
                // POSIX-набор, без GNU-расширений: узлы бывают разными.
                // Данные — только stdout: ворчание bash в stderr иначе
                // превращалось в псевдокаталоги списка.
                let out = self
                    .sh_data(path, "ls -1p | grep '/$' || true", Duration::from_secs(20))
                    .await?;
                let mut dirs: Vec<String> = out
                    .lines()
                    .map(|l| l.trim_end_matches('/').to_string())
                    .filter(|l| !l.is_empty() && !l.starts_with('.'))
                    .collect();
                dirs.sort();
                Ok(dirs)
            }
        }
    }
}

/// Родительский каталог удалённого пути — строками: `Path` этой машины про
/// чужую файловую систему ничего не знает.
pub fn parent_of(dir: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => trimmed[..i].to_string(),
        None => ".".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_sh_runs_in_the_directory() {
        let dir = std::env::temp_dir();
        let (code, out) = Host::Local.sh(&dir.to_string_lossy(), "pwd", Duration::from_secs(5)).await;
        assert_eq!(code, 0);
        assert!(!out.trim().is_empty());
    }

    #[tokio::test]
    async fn local_git_survives_spaces_in_paths() {
        let dir = std::env::temp_dir().join(format!("jarvis host {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy();
        let (code, _) = Host::Local.git(&d, &["init", "-q"]).await;
        assert_eq!(code, 0, "пробел в пути не должен ломать git");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_walks_remote_paths_without_local_fs() {
        assert_eq!(parent_of("/home/bob/proj"), "/home/bob");
        assert_eq!(parent_of("/home/bob/proj/"), "/home/bob");
        assert_eq!(parent_of("/proj"), "/");
    }

    #[test]
    fn machine_names_are_stable() {
        assert_eq!(Host::Local.machine(), "local");
        let h = Host::Ssh { machine: "vps".into(), host: "user@vps".into() };
        assert_eq!(h.machine(), "vps");
    }
}

#[cfg(test)]
mod browse_tests {
    use super::*;

    /// Ровно та поломка из жизни: удалённый bash ворчал в stderr про локаль, и
    /// мусор приклеивался к данным — «/home/desktop bash: warning: setlocale…»
    /// вместо пути. Данные обязаны быть только stdout'ом.
    #[tokio::test]
    async fn data_channel_ignores_stderr_noise() {
        let dir = std::env::temp_dir();
        let out = Host::Local
            .sh_data(
                &dir.to_string_lossy(),
                "echo /home/desktop; echo 'bash: warning: setlocale: LC_ALL: cannot change locale' >&2",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(out.trim(), "/home/desktop", "stderr просочился в данные");
    }

    /// А при ошибке stderr, наоборот, обязан попасть в сообщение: без него
    /// человеку нечего чинить.
    #[tokio::test]
    async fn data_channel_keeps_stderr_in_errors() {
        let dir = std::env::temp_dir();
        let err = Host::Local
            .sh_data(&dir.to_string_lossy(), "echo причина >&2; exit 3", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.contains("причина"), "{err}");
    }

    #[tokio::test]
    async fn local_home_and_dirs_are_real() {
        let home = Host::Local.home().await.unwrap();
        assert!(home.starts_with('/'));

        let base = std::env::temp_dir().join(format!("jarvis-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["бета", "альфа", ".скрытая"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("файл.txt"), "не каталог").unwrap();
        let dirs = Host::Local.list_dirs(&base.to_string_lossy()).await.unwrap();
        // Только каталоги, без скрытых, по алфавиту.
        assert_eq!(dirs, vec!["альфа".to_string(), "бета".to_string()]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
