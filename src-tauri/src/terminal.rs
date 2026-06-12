//! Переход к терминалу сессии: лесенка от точного к грубому.
//! tmux → вкладка по tty (Terminal/iTerm2 через AppleScript) →
//! GUI-приложение-владелец (JetBrains, VS Code…).

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

/// ppid + командная строка процесса.
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

/// Вверх по цепочке родителей до GUI-приложения (.app) — IDE-терминалы и пр.
pub async fn gui_ancestor_app(pid: i64) -> Option<GuiApp> {
    let re = regex::Regex::new(r"/([^/]+)\.app/Contents/MacOS/").unwrap();
    let mut cur = pid;
    for _ in 0..10 {
        if cur <= 1 {
            return None;
        }
        let (ppid, command) = ps1(cur).await?;
        if let Some(c) = re.captures(&command) {
            return Some(GuiApp { pid: cur, name: c[1].to_string() });
        }
        cur = ppid;
    }
    None
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
