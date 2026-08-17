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
    let tail: String = screen.lines().rev().take(20).collect::<Vec<_>>().join("\n").to_lowercase();
    tail.contains("trust this folder") && (tail.contains("do you trust") || tail.contains("yes, i trust"))
}

/// Команда агента руки: абсолютный путь к бинарю + флаги режима.
///
/// Абсолютный, потому что наш tmux мы поднимаем сами, мимо шима: `bash -lc` в
/// неинтерактивном режиме не читает rc-файлы, и голое `claude` могло бы не
/// найтись вовсе.
pub fn hand_command(dangerous: bool) -> Result<String, String> {
    let bin = crate::claude_bin::resolve_claude_bin()
        .ok_or_else(|| "claude не найден на этой машине".to_string())?;
    let flag = if dangerous { " --dangerously-skip-permissions" } else { "" };
    Ok(format!("{}{}", crate::util::shell_quote(&bin.to_string_lossy()), flag))
}

/// Поднять tmux-сессию руки и дождаться живого агента. Возвращает пану.
pub async fn spawn(worktree: &Path, slug: &str, cmd: &str) -> Result<String, String> {
    let name = format!("jarvis-hand-{slug}");
    let cwd = worktree.to_string_lossy();
    let pane = crate::tmux::tmux_j(&[
        "new-session", "-d", "-P", "-F", "#{pane_id}", "-s", &name, "-c", &cwd, "bash", "-lc", cmd,
    ])
    .await
    .map_err(|e| format!("tmux не поднял сессию: {e}"))?
    .trim()
    .to_string();

    // Ждём, пока TUI агента встанет: подтверждаем доверие, если спросил, и
    // считаем экран готовым, когда он перестал меняться. Стабильность вместо
    // поиска конкретной надписи: надписи дрейфуют от версии к версии, а
    // «перестал перерисовываться» — свойство любого вставшего TUI.
    let mut last = String::new();
    let mut stable = 0;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Some(screen) = crate::tmux::capture_pane(&pane).await else { continue };
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
    // Не дождались тишины — пана есть, агент, возможно, ещё грузится. Отдаём
    // пану: первое сообщение уйдёт через обычный reply, он у tmux терпеливый.
    Ok(pane)
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
        assert!(needs_trust("Do you trust this folder?\n> Yes, I trust this folder"));
        assert!(!needs_trust("я не стал бы trust этому выводу"));
        assert!(!needs_trust("Do you trust the output of this tool?"));
        assert!(!needs_trust(""));
    }
}
