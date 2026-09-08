//! Codex's `/model` is a TUI picker, not an inline model command. Every
//! confirmation below is bound to a captured picker and an exact option.
use crate::{model::{Session, Status}, tmux::{Key, Target}};
use std::{collections::HashSet, sync::{Mutex, OnceLock}, time::Duration};
use tokio::time::sleep;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Picker { title: String, rows: Vec<String>, cursor: usize }

fn picker(screen: &str, title: &str) -> Option<Picker> {
    let lines: Vec<_> = screen.lines().collect();
    let start = lines.iter().rposition(|line| line.trim() == title)?;
    let rows_re = regex::Regex::new(r"^\s*([›>]?)\s*\d+\.\s+(.+?)\s*$").ok()?;
    let mut rows = Vec::new(); let mut cursor = None;
    for line in lines.iter().skip(start + 1) {
        if line.contains("Press enter to confirm") { break; }
        if let Some(caps) = rows_re.captures(line) {
            if !caps[1].is_empty() { if cursor.is_some() { return None; } cursor = Some(rows.len()); }
            rows.push(caps[2].split("  ").next()?.trim().to_string());
        }
    }
    if rows.is_empty() || rows.len() > 40 { return None; }
    Some(Picker { title: title.into(), rows, cursor: cursor? })
}
fn model_id(label: &str) -> &str { label.split_whitespace().next().unwrap_or("") }
fn effort_id(label: &str) -> Option<&'static str> {
    let label = label.trim().to_ascii_lowercase();
    if label.starts_with("extra high") { return Some("xhigh"); }
    ["minimal", "low", "medium", "high", "max", "ultra"].into_iter()
        .find(|id| label == *id || label.starts_with(&format!("{id} ")))
}
// Status text alone is insufficient: the native composer may contain a draft.
// Unknown placeholders are deliberately left for the user to handle in the TUI.
fn empty_codex_composer(screen: &str) -> bool {
    let Some((index, prompt)) = screen.lines().enumerate().filter(|(_, line)| line.trim_start().starts_with('›')).last() else { return false; };
    if prompt.trim() != "› Ask Codex to do anything" { return false; }
    let footer = regex::Regex::new(r"^\S+\s+(?:none|minimal|low|medium|high|xhigh|max|ultra|default)\s+·\s+").unwrap();
    let remaining: Vec<_> = screen.lines().skip(index + 1).map(str::trim).filter(|line| !line.is_empty()).collect();
    remaining.first().is_some_and(|line| footer.is_match(line))
}

pub fn empty_claude_composer(screen: &str) -> bool {
    screen.lines().filter(|line| line.trim_start().starts_with('❯')).last().is_some_and(|line| line.trim() == "❯")
}

pub fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 120 && model.as_bytes()[0].is_ascii_alphanumeric()
        && model.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._:/".contains(&c))
}
pub fn control_error(session: &Session, setting: bool) -> Option<&'static str> {
    if session.control_mode.as_deref() == Some("external") { return Some("Этот чат доступен только для чтения. Продолжи его в приложении агента."); }
    if setting && !matches!(session.status, Status::Idle | Status::Done) { return Some("Модель можно изменить после ответа агента и завершения открытого вопроса."); }
    None
}

static CHANGING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
pub struct ChangeGuard(String);
impl ChangeGuard {
    pub fn claim(id: &str) -> Result<Self, &'static str> {
        if !CHANGING.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(id.into()) {
            return Err("Смена модели уже выполняется");
        }
        Ok(Self(id.into()))
    }
}
impl Drop for ChangeGuard { fn drop(&mut self) { if let Some(changing) = CHANGING.get() { changing.lock().unwrap_or_else(|e|e.into_inner()).remove(&self.0); } } }

async fn wait_picker(target: &Target, pane: &str, title: &str) -> Result<Picker, String> {
    for _ in 0..24 {
        if let Some(view) = picker(&target.screen(pane).await?, title) { return Ok(view); }
        sleep(Duration::from_millis(100)).await;
    }
    Err("Codex не показал ожидаемый выбор. Продолжи выбор модели в терминале.".into())
}
async fn select_checked(target: &Target, pane: &str, before: &Picker, index: usize) -> Result<(), String> {
    let latest = picker(&target.screen(pane).await?, &before.title).ok_or("Окно выбора уже изменилось")?;
    if latest != *before || index >= before.rows.len() { return Err("Окно выбора уже изменилось. Повтори выбор в терминале.".into()); }
    let key = if index < before.cursor { "Up" } else { "Down" };
    let steps = vec![Key::Named(key.into()); index.abs_diff(before.cursor)];
    if !steps.is_empty() { target.question_keys(pane, &steps).await?; }
    for _ in 0..20 {
        let current = picker(&target.screen(pane).await?, &before.title).ok_or("Окно выбора уже изменилось")?;
        if current.rows != before.rows { return Err("Список моделей изменился. Повтори выбор в терминале.".into()); }
        if current.cursor == index { target.question_keys(pane, &[Key::Named("Enter".into())]).await?; return Ok(()); }
        sleep(Duration::from_millis(100)).await;
    }
    Err("Не удалось подтвердить выбранную строку. Продолжи в терминале.".into())
}

pub async fn select_codex(target: &Target, pane: &str, model: &str, previous_effort: Option<&str>) -> Result<String, String> {
    if !valid_model(model) { return Err("Некорректный идентификатор модели".into()); }
    let initial = target.screen(pane).await?;
    if initial.contains("esc to interrupt") || crate::screen_prompt::parse_capture(&initial).is_some()
        || !empty_codex_composer(&initial) {
        return Err("В терминале есть черновик или открыт другой экран. Сохрани ввод и повтори выбор модели.".into());
    }
    // No Ctrl-U: never discard a native draft. Only this fixed command goes to
    // a verified empty composer; each subsequent confirmation is checked.
    target.question_keys(pane, &[Key::Text("/model".into()), Key::Named("Enter".into())]).await?;
    let models = wait_picker(target, pane, "Select Model and Effort").await?;
    let matches: Vec<_> = models.rows.iter().enumerate().filter(|(_, row)| model_id(row) == model).collect();
    if matches.len() != 1 { return Err("Эта модель отсутствует в списке Codex. Выбери доступную модель в терминале.".into()); }
    select_checked(target, pane, &models, matches[0].0).await?;
    let reasoning = wait_picker(target, pane, &format!("Select Reasoning Level for {model}")).await?;
    let selected = previous_effort.and_then(|effort| reasoning.rows.iter().position(|row| effort_id(row) == Some(effort))).unwrap_or(reasoning.cursor);
    let effort = effort_id(&reasoning.rows[selected]).ok_or("Codex открыл дополнительные уровни рассуждения. Продолжи в терминале.")?;
    select_checked(target, pane, &reasoning, selected).await?;
    let confirmed = format!("Model changed to {model} {effort}");
    for _ in 0..24 {
        let screen = target.screen(pane).await?;
        let changed = screen.lines().any(|line| line.trim().trim_start_matches('•').trim() == confirmed);
        if changed && !screen.contains("Select Reasoning Level for ") && !screen.contains("Select Model and Effort") && crate::screen_prompt::is_idle_screen(&screen) {
            return Ok(effort.into());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err("Codex не подтвердил смену модели. Проверь терминал; повторная команда не отправлялась.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exact_picker_identity_and_selection() {
        let text = "history\nSelect Model and Effort\n  1. gpt-5.6-sol (default)    Fast\n› 2. gpt-5.6-terra (current)  Balanced\n  3. gpt-5.5                  Other\nPress enter to confirm or esc to go back";
        let parsed = picker(text, "Select Model and Effort").unwrap();
        assert_eq!(parsed.cursor, 1); assert_eq!(model_id(&parsed.rows[0]), "gpt-5.6-sol");
        assert!(picker(text, "Select Reasoning Level for gpt-5.6-sol").is_none());
        assert!(picker(&text.replace("  1.", "› 1."), "Select Model and Effort").is_none());
        assert_eq!(effort_id("Extra high (current)"), Some("xhigh"));
        assert_eq!(effort_id("More reasoning…"), None);
    }
    #[test] fn native_draft_is_not_mistaken_for_an_idle_composer() {
        let idle = "› Ask Codex to do anything\n\n  gpt-5.6-sol high · /tmp/work";
        assert!(empty_codex_composer(idle));
        for draft in ["Do not lose this draft", "/model draft", "first line\nsecond line"] {
            assert!(!empty_codex_composer(&format!("› {draft}\n  gpt-5.6-sol high · /tmp/work\n99% context left")));
        }
        assert!(!empty_codex_composer("› Ask Codex to do anything\nsecond draft line\n  gpt-5.6-sol high · /tmp/work"));
        assert!(empty_claude_composer("❯ \nfor shortcuts"));
        assert!(!empty_claude_composer("❯ Native draft\nfor shortcuts"));
    }
    #[test] fn model_validation_rejects_prompt_and_option_injection() {
        for model in ["", "/model", "--help", "gpt\n/quit", "gpt 5", "gpt;whoami", "$(id)"] { assert!(!valid_model(model)); }
        for model in ["gpt-5.6-sol", "provider/model-1:free", "claude-sonnet-4-6"] { assert!(valid_model(model)); }
    }
    #[test] fn external_and_busy_sessions_cannot_change_model() {
        let mut session = Session::new("s".into(), 0); session.status = Status::Idle; session.tmux_pane = Some("%1".into());
        assert!(control_error(&session, true).is_none());
        session.status = Status::Waiting; assert!(control_error(&session, true).is_some()); assert!(control_error(&session, false).is_none());
        session.control_mode = Some("external".into()); assert!(control_error(&session, false).is_some());
        session.remote = Some("vm".into()); assert!(control_error(&session, false).is_some());
    }
    #[tokio::test]
    #[ignore = "Requires the isolated Codex model fixture and a fixture-only tmux wrapper"]
    async fn isolated_codex_model_probe() {
        let root = std::path::PathBuf::from(std::env::var("JARVIS_QA_MODEL_FIXTURE").expect("fixture path"));
        assert!(root.starts_with(std::env::temp_dir()) || root.starts_with("/tmp"));
        assert!(root.join("owned-model-fixture").exists());
        let model = std::fs::read_to_string(root.join("requested-model")).unwrap();
        let result = select_codex(&Target::Local, "%0", model.trim(), Some("high")).await;
        std::fs::write(root.join("model-result.json"), serde_json::to_vec(&result).unwrap()).unwrap();
        if root.join("expect-refusal").exists() { assert!(result.is_err(), "native draft must be preserved"); }
        else { assert!(result.is_ok(), "{result:?}"); }
    }
    #[test] fn duplicate_model_requests_do_not_interleave_keys() {
        let guard = ChangeGuard::claim("fixture").unwrap(); assert!(ChangeGuard::claim("fixture").is_err()); drop(guard); assert!(ChangeGuard::claim("fixture").is_ok());
    }
}
