//! Read-only terminal question detection on the session's owning machine.
use crate::daemon::Daemon;
use crate::model::{Question, QuestionItem, QuestionOption, ScreenQuestionState, Status};
use crate::util::{ellipsize, now_ms, one_line};
use std::sync::Arc;

fn re(p: &str) -> regex::Regex {
    regex::RegexBuilder::new(p)
        .case_insensitive(true)
        .build()
        .unwrap()
}

#[derive(Debug, Clone)]
pub struct ScreenPrompt {
    pub title: String,
    pub options: Vec<String>,
    pub descriptions: Vec<String>,
    pub multi: bool,
    pub state: ScreenQuestionState,
}

/// Parse a complete captured screen, retaining original numbers and input state.
/// Unknown/custom keymaps are left visible in the terminal instead of guessed.
pub fn parse_screen(lines: &[&str]) -> Option<ScreenPrompt> {
    let text = lines.join("\n");
    let progress = re(r"(?i)question\s+\d+\s*/\s*\d+");
    let codex_input = progress.is_match(&text)
        && re(r"(?i)to submit answer|to submit|add notes|type your answer").is_match(&text);
    let recognized = re(r"(?i)Enter to select|↑/↓ to navigate|to confirm|\(y/n\)|Do you want|Switch model\?|Enter to submit|select.*enter|tab.*navigate|space to select").is_match(&text) || codex_input;
    if !recognized {
        return None;
    }
    // The last question block wins; older prompts can remain in scrollback.
    let option = re(r"^\s*([❯>›→]?)\s*(\d+)[.)]\s+(.+?)\s*$");
    let first = lines
        .iter()
        .rposition(|line| option.captures(line).is_some_and(|c| &c[2] == "1"));
    let block_start = lines.iter().rposition(|line| progress.is_match(line));
    if first.is_none() && !codex_input {
        return None;
    }
    let start = first.or(block_start).unwrap_or(0);
    let checkbox = re(r"\[([ xX✔✓])\]");
    let other = re(r"^(Type something|Other|None of the above)(?:[. …:].*)?$");
    let mut options = Vec::new();
    let mut descriptions = Vec::new();
    let mut numbers = Vec::new();
    let mut selected = Vec::new();
    let mut cursor = 0;
    let mut custom = None;
    let mut multi = false;
    let mut previous = 0;
    for line in &lines[start..] {
        let Some(c) = option.captures(line) else {
            continue;
        };
        let number: u32 = c[2].parse().ok()?;
        if number != previous + 1 || number > 100 {
            return None;
        }
        previous = number;
        if !c[1].is_empty() {
            cursor = number;
        }
        let mut label = c[3].trim().to_string();
        if let Some(mark) = checkbox.captures(&label) {
            multi = true;
            if mark[1].trim().len() > 0 {
                selected.push(number);
            }
            label = checkbox.replace(&label, "").trim().to_string();
        }
        // Codex renders its description in a second, padded terminal column.
        // It is not part of the selectable label returned to the provider.
        let columns = re(r"\s{2,}");
        let mut parts = columns.splitn(&label, 2);
        let option_label = parts.next().unwrap_or("").trim().to_string();
        let description = parts.next().unwrap_or("").trim().to_string();
        label = option_label;
        if re(r"^Chat about this(?:[. …:].*)?$").is_match(&label) {
            // Claude's final action leaves the picker; it is not Other.
            continue;
        } else if other.is_match(&label) {
            custom = Some(number);
        } else {
            numbers.push(number);
            options.push(label);
            descriptions.push(description);
        }
    }
    if options.is_empty() && !codex_input {
        return None;
    }
    let first_idx = first.unwrap_or(lines.len());
    let mut title_start = block_start
        .filter(|i| *i < first_idx)
        .map(|i| i + 1)
        .unwrap_or(0);
    if title_start == 0 {
        for i in (0..first_idx).rev() {
            if re(r"^\s*[─━_]{3,}\s*$").is_match(lines[i]) {
                title_start = i + 1;
                break;
            }
        }
    }
    let candidates: Vec<&str> = lines[title_start..first_idx]
        .iter()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty()
                && !progress.is_match(l)
                && !re(r"^[─━_☐☒✔]+$|Type your answer|to submit|esc to|Add notes").is_match(l)
        })
        .collect();
    let title = candidates
        .iter()
        .find(|line| line.ends_with('?'))
        .or_else(|| candidates.first())
        .copied()
        .unwrap_or("Вопрос в терминале")
        .to_string();
    let picker = if codex_input {
        "codex-input"
    } else {
        "terminal"
    }
    .to_string();
    let page = block_start
        .map(|i| progress.find(lines[i]).map(|m| m.as_str()).unwrap_or(""))
        .unwrap_or("");
    let fingerprint = crate::question_delivery::fingerprint(
        &serde_json::json!([
            title,
            options,
            descriptions,
            numbers,
            custom,
            multi,
            picker,
            page
        ])
        .to_string(),
    );
    let draft_start = block_start.unwrap_or(0);
    let has_written_draft = lines[draft_start..].iter().any(|line| {
        re(r"^\s*›\s+\S").is_match(line)
            && !option.is_match(line)
            && !re(r"Type your answer|Add notes|Select an option").is_match(line)
    });
    let editing =
        (codex_input && (re(r"tab or esc to clear notes").is_match(&text) || has_written_draft))
            || re(r"ctrl\+g to edit in").is_match(&text);
    Some(ScreenPrompt {
        title,
        options,
        descriptions,
        multi,
        state: ScreenQuestionState {
            fingerprint,
            cursor,
            selected,
            option_numbers: numbers,
            custom_index: custom,
            picker,
            editing,
        },
    })
}

pub fn parse_capture(text: &str) -> Option<ScreenPrompt> {
    parse_screen(&text.lines().collect::<Vec<_>>())
}

pub fn is_idle_screen(text: &str) -> bool {
    re(r"bypass permissions on|for agents|esc to interrupt|context left|for shortcuts")
        .is_match(text)
        || (re(r"(?m)^\s*›\s+Ask Codex to do anything\s*$").is_match(text)
            && re(r"(?m)^\s*\S+\s+(?:none|minimal|low|medium|high|xhigh|max|default)\s+·\s+").is_match(text))
}

pub async fn detect_stuck_prompt(d: &Arc<Daemon>, sid: &str) {
    let Some(s) = d.session(sid) else {
        return;
    };
    let Some(pane) = s.tmux_pane.clone() else {
        return;
    };
    if s.question.as_ref().is_some_and(|q| !q.from_screen) {
        return;
    }
    if s.status == Status::Working && now_ms() - s.updated_at < 8000 {
        return;
    }
    // Never resolve a remote %N against the local tmux server.
    let Ok(target) = d.pane_target(&s) else {
        return;
    };
    let Ok(screen) = target.screen(&pane).await else {
        return;
    };
    let previous = s.question.clone();
    let Some(prompt) = parse_capture(&screen) else {
        if previous.as_ref().is_some_and(|q| q.from_screen) && is_idle_screen(&screen) {
            let mut removed = false;
            d.with_session(sid, |current| {
                if current
                    .question
                    .as_ref()
                    .zip(previous.as_ref())
                    .is_some_and(|(a, b)| crate::question_delivery::same_request(a, b))
                {
                    current.question = None;
                    current.status = Status::Idle;
                    current.updated_at = now_ms();
                    removed = true;
                }
            });
            if removed {
                crate::windows::toast_remove(d, &format!("q-{sid}"));
                d.push();
            }
        }
        return;
    };
    if previous
        .as_ref()
        .and_then(|q| q.screen.as_ref())
        .is_some_and(|state| state.fingerprint == prompt.state.fingerprint)
    {
        return;
    }
    let custom = prompt.state.custom_index.is_some() || prompt.state.picker == "codex-input";
    let notes = prompt.state.picker == "codex-input"
        && prompt.state.custom_index.is_none()
        && !prompt.options.is_empty();
    let mut q = Question {
        from_screen: true,
        at: now_ms(),
        screen: Some(prompt.state.clone()),
        questions: vec![QuestionItem {
            question: prompt.title.clone(),
            multi_select: prompt.multi,
            custom_allowed: Some(custom),
            custom_mode: if notes { "notes" } else { "alternative" }.into(),
            options: prompt
                .options
                .iter()
                .enumerate()
                .map(|(i, label)| QuestionOption {
                    label: label.clone(),
                    description: prompt.descriptions[i].clone(),
                    ..QuestionOption::default()
                })
                .collect(),
            ..QuestionItem::default()
        }],
        ..Question::default()
    };
    crate::question_delivery::identify(&mut q, None);
    let detail = ellipsize(&one_line(&prompt.title), 140);
    let mut applied = false;
    d.with_session(sid, |current| {
        // A hook/new turn may have arrived while the remote capture was pending.
        let unchanged = match (&current.question, &previous) {
            (None, None) => current.updated_at == s.updated_at,
            (Some(a), Some(b)) => crate::question_delivery::same_request(a, b),
            _ => false,
        };
        if unchanged {
            current.status = Status::Waiting;
            current.question = Some(q);
            current.detail = detail.clone();
            applied = true;
        }
    });
    if !applied {
        return;
    }
    if d.settings.bool("notifyWaiting") {
        let project = s.project.as_deref().unwrap_or("Агент");
        d.notify_id(
            &format!("q-{sid}"),
            &format!("{project} — спрашивает"),
            &detail,
            Some(sid),
            "waiting",
        );
    }
    d.push();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_switch_confirmation() {
        let tail = vec![
            "──────────────────────────",
            " Switch model?",
            " This will start a new turn.",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Enter to select",
        ];
        let p = parse_screen(&tail).expect("должен распознать");
        assert_eq!(p.title, "Switch model?");
        assert_eq!(p.options, vec!["Yes", "No"]);
        assert!(!p.multi);
    }

    #[test]
    fn detects_multiselect_checkboxes() {
        let tail = vec![
            "────────",
            " Which ones?",
            " ❯ 1. [x] Alpha",
            "   2. [ ] Beta",
            " Enter to confirm",
        ];
        let p = parse_screen(&tail).unwrap();
        assert!(p.multi);
        assert_eq!(p.options, vec!["Alpha", "Beta"]);
    }

    #[test]
    fn claude_21258_other_is_distinct_from_chat_and_existing_drafts_are_protected() {
        let text = "←  ☒ Machine  ☐ Checks  ✔ Submit  →\nWhich checks?\n❯ 1. [✔] Unit\n  Fast tests\n  2. [ ] UI\n  Visual checks\n  3. [ ] Type something\n     Next\n────────\n  4. Chat about this\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";
        let parsed = parse_capture(text).unwrap();
        assert_eq!(parsed.options, vec!["Unit", "UI"]);
        assert_eq!(parsed.state.custom_index, Some(3));
        assert_eq!(parsed.state.selected, vec![1]);
        assert_eq!(parsed.title, "Which checks?");
        assert!(!parsed.state.editing);
        let written = text.replace("Type something", "User's existing draft").replace("Esc to cancel", "ctrl+g to edit in Vim · Esc to cancel");
        assert!(parse_capture(&written).unwrap().state.editing);
    }

    #[test]
    fn ignores_plain_idle_screen() {
        let tail = vec!["> ", "  bypass permissions on"];
        assert!(parse_screen(&tail).is_none());
        assert!(is_idle_screen("…esc to interrupt…"));
        assert!(is_idle_screen("• Questions 2/2 answered\n› Ask Codex to do anything\n  gpt-5.5 medium · /tmp/fixture   Plan mode (shift+tab to cycle)"));
        assert!(!is_idle_screen("An example: Ask Codex to do anything"));
    }

    #[test]
    fn skips_type_something_option() {
        let tail = vec![
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Type something.",
            " Enter to select",
        ];
        let p = parse_screen(&tail).unwrap();
        assert_eq!(p.options, vec!["Yes"]);
        assert_eq!(p.state.custom_index, Some(2));
    }

    #[test]
    fn cursor_checkbox_and_real_option_numbers_survive_parsing() {
        let p = parse_capture(
            "Which ones?\n  1. [x] Alpha\n❯ 2. [ ] Beta\n  3. Other\nEnter to confirm",
        )
        .unwrap();
        assert_eq!(p.state.cursor, 2);
        assert_eq!(p.state.selected, vec![1]);
        assert_eq!(p.state.option_numbers, vec![1, 2]);
        assert_eq!(p.state.custom_index, Some(3));
        let moved = parse_capture(
            "Which ones?\n❯ 1. [x] Alpha\n  2. [ ] Beta\n  3. Other\nEnter to confirm",
        )
        .unwrap();
        assert_eq!(p.state.fingerprint, moved.state.fingerprint);
        let changed = parse_capture(
            "Which ones?\n❯ 1. [x] Gamma\n  2. [ ] Beta\n  3. Other\nEnter to confirm",
        )
        .unwrap();
        assert_ne!(p.state.fingerprint, changed.state.fingerprint);
    }

    #[test]
    fn codex_input_and_text_only_have_explicit_capabilities() {
        let p = parse_capture("Question 1/2\nWhere?\n  1. Local\n› 2. VM\n  3. None of the above\nenter to submit answer | tab to add notes").unwrap();
        assert_eq!(p.title, "Where?");
        assert_eq!(p.state.picker, "codex-input");
        assert_eq!(p.state.cursor, 2);
        assert_eq!(p.state.custom_index, Some(3));
        let text = parse_capture("Question 2/2\nTell me more\nType your answer (optional)\nenter to submit answer | esc to interrupt").unwrap();
        assert_eq!(text.title, "Tell me more");
        assert!(text.options.is_empty());
    }

    #[test]
    fn incomplete_or_unrecognized_screen_cannot_be_navigated() {
        assert!(parse_capture("Choose?\n❯ 2. Second\n  3. Third\nEnter to select").is_none());
        assert!(parse_capture("Choose?\n❯ 1. First\n  3. Third\nEnter to select").is_none());
        assert!(parse_capture("A numbered document\n1. Example\n2. Another").is_none());
    }
}
