//! Переход к терминалу сессии: лесенка от точного к грубому.
//! tmux → вкладка по tty → GUI-приложение-владелец (JetBrains, VS Code…).
//!
//! Общая часть — обход дерева процессов через `ps` (одинаково работает на macOS
//! и Linux). Платформенное — только опознание GUI-приложения и активация окна:
//! macOS ходит в AppleScript, Linux — в `wmctrl`/`xdotool`.
//!
//! Каждая ступень возвращает `false`, если не сработала, и вызывающий переходит
//! к следующей. Поэтому отсутствие `wmctrl` — это не ошибка, а просто «фокус
//! перевести не смогли».

use std::process::Stdio;
use std::time::Duration;

pub struct GuiApp {
    pub pid: i64,
    pub name: String,
}

async fn run(cmd: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let mut c = tokio::process::Command::new(cmd);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(timeout, c.output()).await.ok()?.ok()
}

/// ppid + командная строка процесса. `ps -o ppid=,command=` есть и в BSD-ps
/// (macOS), и в procps (Linux) — обход дерева общий.
async fn ps1(pid: i64) -> Option<(i64, String)> {
    let out = run("ps", &["-o", "ppid=,command=", "-p", &pid.to_string()], Duration::from_secs(4)).await?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    let (ppid, command) = line.split_once(char::is_whitespace)?;
    Some((ppid.trim().parse().ok()?, command.trim().to_string()))
}

/// Вверх по цепочке родителей до GUI-приложения (IDE-терминалы и пр.).
/// Опознание — платформенное: на macOS это путь внутри `.app`, на Linux —
/// имя известного бинаря терминала/IDE.
pub async fn gui_ancestor_app(pid: i64) -> Option<GuiApp> {
    let mut cur = pid;
    for _ in 0..10 {
        if cur <= 1 {
            return None;
        }
        let (ppid, command) = ps1(cur).await?;
        if let Some(name) = imp::match_gui_app(&command) {
            return Some(GuiApp { pid: cur, name });
        }
        cur = ppid;
    }
    None
}

pub async fn activate_app_by_pid(pid: i64) -> bool {
    imp::activate_app_by_pid(pid).await
}

pub async fn activate_app_by_name(name: &str) -> bool {
    imp::activate_app_by_name(name).await
}

/// Точный фокус вкладки терминала по tty. Самая верхняя ступень лесенки —
/// работает только там, где терминал скриптуется.
pub async fn focus_terminal_by_tty(tty: &str) -> bool {
    imp::focus_terminal_by_tty(tty).await
}

/* ================= macOS ================= */

#[cfg(target_os = "macos")]
mod imp {
    use super::{run, Duration};

    /// Владелец — приложение из бандла: `/…/Foo.app/Contents/MacOS/foo`.
    pub fn match_gui_app(command: &str) -> Option<String> {
        let re = regex::Regex::new(r"/([^/]+)\.app/Contents/MacOS/").ok()?;
        re.captures(command).map(|c| c[1].to_string())
    }

    pub async fn activate_app_by_pid(pid: i64) -> bool {
        let script = format!(
            "tell application \"System Events\" to set frontmost of (first application process whose unix id is {pid}) to true"
        );
        run("osascript", &["-e", &script], Duration::from_secs(4))
            .await
            .is_some_and(|o| o.status.success())
    }

    pub async fn activate_app_by_name(name: &str) -> bool {
        let quoted = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
        let script = format!("tell application {quoted} to activate");
        run("osascript", &["-e", &script], Duration::from_secs(4))
            .await
            .is_some_and(|o| o.status.success())
    }

    /// Скриптуемые терминалы: точный фокус вкладки по tty.
    const FOCUS_SCRIPT: &str = r#"
on run argv
  set theTty to item 1 of argv
  try
    if application "iTerm2" is running then
      tell application "iTerm2"
        repeat with w in windows
          repeat with t in tabs of w
            repeat with se in sessions of t
              if tty of se is theTty then
                select se
                select t
                activate
                return "ok"
              end if
            end repeat
          end repeat
        end repeat
      end tell
    end if
  end try
  try
    if application "Terminal" is running then
      tell application "Terminal"
        repeat with w in windows
          repeat with t in tabs of w
            if tty of t is theTty then
              set selected of t to true
              set index of w to 1
              activate
              return "ok"
            end if
          end repeat
        end repeat
      end tell
    end if
  end try
  return "no"
end run"#;

    pub async fn focus_terminal_by_tty(tty: &str) -> bool {
        run("osascript", &["-e", FOCUS_SCRIPT, tty], Duration::from_secs(5))
            .await
            .is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "ok")
    }
}

/* ================= Linux ================= */

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{run, Duration};

    /// Известные владельцы терминала: эмуляторы и IDE со встроенным терминалом.
    /// Бандлов, как на macOS, тут нет, поэтому опознаём по имени исполняемого файла.
    const GUI_APPS: &[&str] = &[
        // терминалы
        "gnome-terminal-server", "konsole", "xfce4-terminal", "terminator", "tilix",
        "alacritty", "kitty", "wezterm-gui", "wezterm", "foot", "footclient", "ptyxis",
        "xterm", "urxvt", "st", "contour", "ghostty", "warp-terminal", "tabby",
        // IDE со встроенным терминалом
        "code", "code-insiders", "codium", "cursor", "zed",
        "idea", "pycharm", "webstorm", "rustrover", "goland", "clion", "phpstorm",
        "rubymine", "datagrip", "android-studio", "fleet",
    ];

    /// Имя бинаря из командной строки процесса (первый токен, без пути).
    fn binary_name(command: &str) -> Option<&str> {
        let first = command.split_whitespace().next()?;
        Some(first.rsplit('/').next().unwrap_or(first))
    }

    pub fn match_gui_app(command: &str) -> Option<String> {
        let bin = binary_name(command)?;
        // точное совпадение либо префикс: у JetBrains это idea.sh, pycharm.sh и т.п.
        GUI_APPS
            .iter()
            .find(|a| bin == **a || bin.starts_with(&format!("{a}.")))
            .map(|a| (*a).to_string())
    }

    /// Активация окна по pid: сперва wmctrl (умеет искать по pid),
    /// затем xdotool. Нет ни того, ни другого — ступень просто не сработала.
    pub async fn activate_app_by_pid(pid: i64) -> bool {
        // wmctrl -l -p печатает: <winid> <desktop> <pid> <host> <title>
        if let Some(out) = run("wmctrl", &["-l", "-p"], Duration::from_secs(4)).await {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let win = text.lines().find_map(|line| {
                    let mut it = line.split_whitespace();
                    let id = it.next()?;
                    let _desktop = it.next()?;
                    let owner: i64 = it.next()?.parse().ok()?;
                    (owner == pid).then(|| id.to_string())
                });
                if let Some(id) = win {
                    if run("wmctrl", &["-i", "-a", &id], Duration::from_secs(4))
                        .await
                        .is_some_and(|o| o.status.success())
                    {
                        return true;
                    }
                }
            }
        }
        run(
            "xdotool",
            &["search", "--pid", &pid.to_string(), "windowactivate", "%1"],
            Duration::from_secs(4),
        )
        .await
        .is_some_and(|o| o.status.success())
    }

    /// Активация по имени: wmctrl ищет подстроку в заголовке окна.
    pub async fn activate_app_by_name(name: &str) -> bool {
        if run("wmctrl", &["-a", name], Duration::from_secs(4))
            .await
            .is_some_and(|o| o.status.success())
        {
            return true;
        }
        run(
            "xdotool",
            &["search", "--name", name, "windowactivate", "%1"],
            Duration::from_secs(4),
        )
        .await
        .is_some_and(|o| o.status.success())
    }

    /// Переносимого способа найти вкладку терминала по tty на Linux нет:
    /// ни один эмулятор не отдаёт наружу соответствие tty→вкладка. Возвращаем
    /// `false` — вызывающий спускается на ступень «GUI-приложение-владелец»,
    /// а точный путь и так закрыт tmux'ом, который кросс-платформенный.
    pub async fn focus_terminal_by_tty(_tty: &str) -> bool {
        false
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #[test]
    fn matches_known_terminals_and_ides() {
        let m = |c: &str| super::imp::match_gui_app(c);
        assert_eq!(m("/usr/libexec/gnome-terminal-server").as_deref(), Some("gnome-terminal-server"));
        assert_eq!(m("/usr/bin/kitty --single-instance").as_deref(), Some("kitty"));
        assert_eq!(m("/opt/idea/bin/idea.sh").as_deref(), Some("idea"));
        assert_eq!(m("/usr/share/code/code --unity-launch").as_deref(), Some("code"));
    }

    #[test]
    fn ignores_plain_processes() {
        assert!(super::imp::match_gui_app("/usr/bin/bash").is_none());
        assert!(super::imp::match_gui_app("claude --resume abc").is_none());
    }
}
