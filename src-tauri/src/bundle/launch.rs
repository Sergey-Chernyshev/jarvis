//! Запуск руки: агент в tmux прямо в её worktree, без окон терминала.
//!
//! Пять рук — это было бы пять окон Terminal.app; связке они не нужны: за
//! руками смотрят через чаты Jarvis. Поэтому поднимаем tmux-сессии на том же
//! сервере `-L jarvis`, где живут обычные сессии, — весь пульт (ответы,
//! клавиши, экран паны) работает с ними из коробки, а хуки Claude Code сами
//! регистрируют сессию в демоне.

use std::path::Path;
use std::time::Duration;

/// Первый вопрос нового каталога — «доверяешь ли ты этой папке?». До ответа
/// агент не стартует и не шлёт ни одного хука. Подтверждаем сами: каталог
/// создали мы же по просьбе человека, отказ доверять ему не имеет смысла.
/// Детектор требует пары маркеров — слово «trust» в обычном выводе не повод
/// жать Enter вслепую (проверено на узле, откуда этот приём и взят).
fn needs_trust(screen: &str) -> bool {
    let tail: String = screen
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    tail.contains("trust this folder")
        && (tail.contains("do you trust") || tail.contains("yes, i trust"))
}

/// Команда агента руки: абсолютный путь к бинарю + флаги режима.
///
/// Абсолютный, потому что наш tmux мы поднимаем сами, мимо шима: `bash -lc` в
/// неинтерактивном режиме не читает rc-файлы, и голое `claude` могло бы не
/// найтись вовсе.
pub fn hand_command(agent: &str, dangerous: bool) -> Result<String, String> {
    let bin = match agent {
        "claude" => crate::claude_bin::resolve_claude_bin(),
        "codex" => crate::backend::codex::resolve_codex_bin(),
        _ => return Err("неизвестный агент связки".into()),
    }
    .ok_or_else(|| format!("{agent} не найден на этой машине"))?;
    let flag = match (agent, dangerous) {
        ("codex", true) => " --dangerously-bypass-approvals-and-sandbox",
        (_, true) => " --dangerously-skip-permissions",
        _ => "",
    };
    Ok(format!(
        "{}{}",
        crate::util::shell_quote(&bin.to_string_lossy()),
        flag
    ))
}

/// Preflight happens before creating a repo or worktree, on the chosen host.
/// Executable presence does not imply valid authentication.
pub async fn preflight(
    host: &super::host::Host,
    agent: &str,
    dangerous: bool,
) -> Result<(), String> {
    if !matches!(agent, "claude" | "codex") {
        return Err("неизвестный агент связки".into());
    }
    match host {
        super::host::Host::Local => hand_command(agent, dangerous).map(|_| ()),
        super::host::Host::Ssh { .. } | super::host::Host::ConfiguredSsh { .. } => {
            let command = format!(
                "export PATH=\"$HOME/.local/bin:$HOME/bin:$HOME/.bun/bin:$HOME/.npm-global/bin:$HOME/.local/share/pnpm:$HOME/.claude/local:/usr/local/bin:/opt/homebrew/bin:$PATH\"; command -v {agent} >/dev/null"
            );
            let (code, why) = host.sh("/", &command, Duration::from_secs(15)).await;
            if code == 0 {
                Ok(())
            } else {
                Err(format!(
                    "{agent} не найден или узел недоступен: {}",
                    crate::util::ellipsize(&why, 200)
                ))
            }
        }
    }
}

/// Поднять tmux-сессию руки и дождаться живого агента. Возвращает пану.
pub async fn spawn(worktree: &Path, slug: &str, cmd: &str) -> Result<String, String> {
    let name = format!("jarvis-hand-{slug}");
    let cwd = worktree.to_string_lossy();
    let pane = crate::tmux::tmux_j(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        &name,
        "-c",
        &cwd,
        "bash",
        "-lc",
        cmd,
    ])
    .await
    .map_err(|e| format!("tmux не поднял сессию: {e}"))?
    .trim()
    .to_string();

    if !pane.starts_with('%') || pane.len() < 2 || !pane[1..].bytes().all(|b| b.is_ascii_digit()) {
        return Err("tmux не вернул идентификатор запущенного агента".into());
    }

    // Ждём, пока TUI агента встанет: подтверждаем доверие, если спросил, и
    // считаем экран готовым, когда он перестал меняться. Стабильность вместо
    // поиска конкретной надписи: надписи дрейфуют от версии к версии, а
    // «перестал перерисовываться» — свойство любого вставшего TUI.
    let mut last = String::new();
    let mut stable = 0;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let dead =
            crate::tmux::tmux_j(&["display-message", "-p", "-t", &pane, "#{pane_dead}"]).await;
        if dead.as_deref().map(str::trim) != Ok("0") {
            return Err("агент завершился до готовности; проверь CLI и его авторизацию".into());
        }
        let Some(screen) = crate::tmux::capture_pane(&pane).await else {
            continue;
        };
        if needs_trust(&screen) {
            let _ = crate::tmux::tmux_j(&["send-keys", "-t", &pane, "Enter"]).await;
            stable = 0;
            last.clear();
            continue;
        }
        if !screen.trim().is_empty() && screen == last {
            stable += 1;
            if stable >= 2 {
                return Ok(pane);
            }
        } else {
            stable = 0;
            last = screen;
        }
    }
    let _ = crate::tmux::tmux_j(&["kill-pane", "-t", &pane]).await;
    Err("агент не стал готов за 20 секунд; запуск остановлен".into())
}

/// Отдать руке её задачу — первым сообщением в чат.
pub async fn first_message(pane: &str, task: &str) -> Result<(), String> {
    crate::tmux::reply(pane, task).await
}

/// Прервать руку — тем же, чем прерывают агента в терминале.
pub async fn interrupt(pane: &str) {
    let _ = crate::tmux::tmux_j(&["send-keys", "-t", pane, "Escape"]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_detector_needs_the_marker_pair() {
        assert!(needs_trust(
            "Do you trust this folder?\n> Yes, I trust this folder"
        ));
        assert!(!needs_trust("я не стал бы trust этому выводу"));
        assert!(!needs_trust("Do you trust the output of this tool?"));
        assert!(!needs_trust(""));
    }
}
