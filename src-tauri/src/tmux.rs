//! tmux-транспорт: отдельный сервер `-L jarvis` (его поднимает claude-шим).
//!
//! Это канал ВВОДА демона: вставка ответов в пану, слэш-команды пульта,
//! ответы на вопросы клавишами. Текст всегда уходит элементом argv —
//! никакой интерполяции в shell-строку.

use std::process::Stdio;
use std::time::Duration;
use tokio::time::sleep;

/// `tmux -L jarvis <args>`: stdout при успехе, текст ошибки при провале.
pub async fn tmux_j(args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.arg("-L")
        .arg("jarvis")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .map_err(|_| "tmux: таймаут".to_string())?
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "tmux: ошибка".into() } else { err })
    }
}

pub async fn pane_alive(pane: &str) -> bool {
    tmux_j(&["display-message", "-p", "-t", pane, "ok"]).await.is_ok()
}

pub async fn capture_pane(pane: &str) -> Option<String> {
    tmux_j(&["capture-pane", "-t", pane, "-p"]).await.ok()
}

/// Человекочитаемое имя tmux-сессии паны — для бейджа в панели.
pub async fn session_name(pane: &str) -> Option<String> {
    tmux_j(&["display-message", "-p", "-t", pane, "#{session_name}"])
        .await
        .ok()
        .map(|s| crate::util::one_line(&s))
        .filter(|s| !s.is_empty())
}

/// Вставка промпта в пану. C-u срезает недописанный черновик в строке ввода —
/// иначе вставка доклеится к нему и Enter отправит склейку.
/// set-buffer → paste-buffer (bracketed, ради многострочных) → отдельный Enter.
pub async fn reply(pane: &str, prompt: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?;
    tmux_j(&["set-buffer", "-b", "jarvis-reply", "--", prompt]).await?;
    tmux_j(&["paste-buffer", "-p", "-d", "-b", "jarvis-reply", "-t", pane]).await?;
    // даём TUI дожевать bracketed-paste, иначе Enter иногда обгоняет вставку
    // и текст остаётся в строке ввода неотправленным
    sleep(Duration::from_millis(90)).await;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    Ok(())
}

/// Пульт: слэш-команда с аргументом (`/model sonnet`, `/effort high`).
/// На длинной сессии /model показывает «Switch model?» — подтверждаем
/// выделенный по умолчанию вариант (Yes) ещё одним Enter, если он есть.
pub async fn paste_slash(pane: &str, text: &str) -> Result<(), String> {
    tmux_j(&["send-keys", "-t", pane, "C-u"]).await?; // не клеимся к черновику
    tmux_j(&["set-buffer", "-b", "jarvis-cmd", "--", text]).await?;
    tmux_j(&["paste-buffer", "-p", "-d", "-b", "jarvis-cmd", "-t", pane]).await?;
    tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
    sleep(Duration::from_millis(700)).await;
    if let Some(screen) = capture_pane(pane).await {
        // 11, не 12: у JS slice(-12) последний элемент — пустой хвост от trailing \n
        let tail: Vec<&str> = screen.lines().rev().take(11).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        let confirm = regex::RegexBuilder::new(r"Switch model\?|Enter to select|to confirm")
            .case_insensitive(true)
            .build()
            .unwrap();
        if confirm.is_match(&tail) {
            tmux_j(&["send-keys", "-t", pane, "Enter"]).await?;
        }
    }
    Ok(())
}

/// Метаданные живой паны для адопта осиротевших сессий при рестарте демона.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub session_name: String,
    pub cwd: String,
    pub pid: i64,
}

/// Живые паны сервера jarvis с метаданными (id, имя сессии, cwd, pid процесса
/// паны). Семантика арм: `Ok(Some)` — успех, `Ok(None)` — tmux не установлен
/// (реестр не трогаем), `Err` — ошибка/пустой сервер.
/// Разделитель полей — таб: ни id, ни имя сессии, ни pid его не содержат, а путь
/// идёт последним полем.
pub async fn list_panes_meta() -> Result<Option<Vec<PaneInfo>>, ()> {
    let mut cmd = tokio::process::Command::new("tmux");
    cmd.args([
        "-L",
        "jarvis",
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{session_name}\t#{pane_pid}\t#{pane_current_path}",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(4), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut it = line.splitn(4, '\t');
                    let pane_id = it.next()?.trim();
                    if pane_id.is_empty() {
                        return None;
                    }
                    let session_name = it.next().unwrap_or("").trim().to_string();
                    let pid = it.next().unwrap_or("").trim().parse::<i64>().unwrap_or(0);
                    let cwd = it.next().unwrap_or("").trim().to_string();
                    Some(PaneInfo {
                        pane_id: pane_id.to_string(),
                        session_name,
                        cwd,
                        pid,
                    })
                })
                .collect(),
        )),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(()),
    }
}

/// Подписать tmux-окно заголовком сессии (терминал подписывает сам себя).
pub async fn rename_window(pane: &str, name: &str) -> Result<(), String> {
    tmux_j(&["rename-window", "-t", pane, name]).await.map(|_| ())
}

/// Ответ на AskUserQuestion клавишами (механика проверена на живом пикере):
/// single-select — цифра выбирает и подтверждает сразу; multiSelect — цифры
/// тогглят чекбоксы, Right ведёт на Submit-таб, там Review-экран, где «1» = Submit.
pub async fn answer_question(pane: &str, indices: &[u32], multi: bool) -> Result<(), String> {
    if multi {
        for n in indices {
            tmux_j(&["send-keys", "-t", pane, &n.to_string()]).await?;
            sleep(Duration::from_millis(150)).await;
        }
        tmux_j(&["send-keys", "-t", pane, "Right"]).await?;
        sleep(Duration::from_millis(200)).await;
        tmux_j(&["send-keys", "-t", pane, "1"]).await?; // Review: «1. Submit answers»
    } else {
        tmux_j(&["send-keys", "-t", pane, &indices[0].to_string()]).await?;
    }
    Ok(())
}

/// «Где это?» — секундный оверлей прямо в терминале сессии.
/// popup рисуется в подключённом клиенте — у detached-сессии его нет.
pub async fn ping(pane: &str) -> Result<(), String> {
    let clients = tmux_j(&["list-clients", "-t", pane, "-F", "#{client_name}"])
        .await
        .unwrap_or_default();
    if crate::util::one_line(&clients).is_empty() {
        return Err("Окно терминала не подключено (detached) — показать негде".into());
    }
    tmux_j(&[
        "display-popup", "-t", pane, "-w", "34", "-h", "3", "-E",
        "printf \"\\n   ◇ Jarvis: вот эта сессия\"; sleep 1",
    ])
    .await
    .map(|_| ())
    .map_err(|e| format!("Поповер не показался: {}", crate::util::ellipsize(&crate::util::one_line(&e), 80)))
}

/// Фокус-лесенка, ступень tmux: switch-client, не вышло — select-window.
pub async fn focus(pane: &str) -> bool {
    let direct = tokio::process::Command::new("tmux")
        .args(["switch-client", "-t", pane])
        .output()
        .await;
    if matches!(&direct, Ok(o) if o.status.success()) {
        return true;
    }
    let select = tokio::process::Command::new("tmux")
        .args(["select-window", "-t", pane])
        .output()
        .await;
    matches!(&select, Ok(o) if o.status.success())
}
