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
        match self {
            Host::Local => local_git(dir, args).await,
            Host::Ssh { host, .. } => {
                let mut full = format!("git -C {}", crate::util::shell_quote(dir));
                for a in args {
                    full.push(' ');
                    full.push_str(&crate::util::shell_quote(a));
                }
                ssh(host, &full, Duration::from_secs(120)).await
            }
        }
    }
}

async fn local_git(dir: &str, args: &[&str]) -> (i32, String) {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    collect(cmd, Duration::from_secs(120)).await
}

/// `ssh -o BatchMode=yes host bash -lc <строка>`. BatchMode — принципиально:
/// такт крутится без человека, и ssh, вставший с вопросом про пароль, повесил
/// бы связку целиком. Не настроен ключ — честная ошибка в ленту.
async fn ssh(host: &str, script: &str, timeout: Duration) -> (i32, String) {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg(host)
        .arg("bash")
        .arg("-lc")
        .arg(crate::util::shell_quote(script))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    collect(cmd, timeout).await
}

async fn collect(mut cmd: tokio::process::Command, timeout: Duration) -> (i32, String) {
    let Ok(Ok(out)) = tokio::time::timeout(timeout, cmd.output()).await else {
        return (-1, format!("не уложилось в {} с", timeout.as_secs()));
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(err.trim_end());
    }
    (out.status.code().unwrap_or(-1), text)
}

impl Host {
    /// Домашний каталог машины — с него начинается обзор.
    pub async fn home(&self) -> Result<String, String> {
        match self {
            Host::Local => std::env::var("HOME").map_err(|_| "не знаю $HOME".into()),
            Host::Ssh { host, .. } => {
                let (code, out) = ssh(host, "printf %s \"$HOME\"", Duration::from_secs(15)).await;
                let out = out.trim().to_string();
                if code == 0 && out.starts_with('/') {
                    Ok(out)
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
                let (code, out) = self
                    .sh(path, "LC_ALL=C ls -1p | grep '/$' || true", Duration::from_secs(20))
                    .await;
                if code != 0 {
                    return Err(crate::util::ellipsize(&crate::util::one_line(&out), 200));
                }
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
