//! Детектор интерактивных промптов на экране tmux-паны.
//!
//! Подтверждения slash-команд («Switch model?») и прочие пикеры выполняет сам
//! клиент Claude Code — они НЕ tool-call'ы, в хуки и транскрипт не попадают.
//! Единственный источник — экран паны. Парсим его, переиспользуя question-UI.

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::model::{Question, QuestionItem, QuestionOption, Status};
use crate::util::{ellipsize, now_ms, one_line};

fn re(p: &str) -> regex::Regex {
    regex::RegexBuilder::new(p).case_insensitive(true).build().unwrap()
}

/// Распознанный на экране вопрос (чистая часть — тестируется без tmux).
pub struct ScreenPrompt {
    pub title: String,
    pub options: Vec<String>,
    pub multi: bool,
}

/// Хвост экрана (≤18 строк) → интерактивный промпт, если он там есть.
pub fn parse_screen(tail: &[&str]) -> Option<ScreenPrompt> {
    let text = tail.join("\n");
    let screen_prompt = re(r"Enter to select|↑/↓ to navigate|to confirm|\(y/n\)|Do you want|Switch model\?");
    let first_option = re(r"(?m)^\s*[❯>]?\s*1[.)]\s+\S");
    if !screen_prompt.is_match(&text) || !first_option.is_match(&text) {
        return None;
    }

    let option_line = re(r"^\s*[❯>]?\s*\d+[.)]\s+(.+?)\s*$");
    let checkbox = re(r"\[[ xX✔]\]");
    let skip_label = re(r"^(Type something|Chat about this)\.?$");
    let mut options = Vec::new();
    let mut multi = false;
    for raw in tail {
        let Some(c) = option_line.captures(raw) else { continue };
        let mut label = c[1].trim().to_string();
        if checkbox.is_match(&label) {
            multi = true;
            label = checkbox.replace(&label, "").trim().to_string();
        }
        if !skip_label.is_match(&label) {
            options.push(label);
        }
    }
    if options.is_empty() {
        return None;
    }

    // блок вопроса = строки между рамкой-разделителем и первой опцией
    let first_opt_line = re(r"^\s*[❯>]?\s*1[.)]\s+");
    let frame = re(r"^\s*[─━_]{3,}\s*$");
    let first_idx = tail.iter().position(|l| first_opt_line.is_match(l)).unwrap_or(0);
    let mut start_idx = 0;
    for i in (0..first_idx).rev() {
        if frame.is_match(tail[i]) {
            start_idx = i + 1;
            break;
        }
    }
    let frame_only = re(r"^[─━_]+$");
    let checkmark_start = re(r"^[☐☒✔]");
    let cands: Vec<&str> = tail[start_idx..first_idx]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !frame_only.is_match(l) && !checkmark_start.is_match(l))
        .collect();
    // заголовок: строка-вопрос (с «?») либо самая верхняя строка блока
    let title_src = cands
        .iter()
        .find(|l| l.ends_with('?'))
        .or_else(|| cands.first())
        .copied()
        .unwrap_or("");
    let title = {
        let t = ellipsize(&one_line(title_src), 200);
        if t.is_empty() { "Подтверждение в терминале".to_string() } else { t }
    };

    Some(ScreenPrompt { title, options, multi })
}

/// Экран показывает обычный idle-промпт Claude Code (вопрос ушёл)?
pub fn is_idle_screen(text: &str) -> bool {
    re(r"bypass permissions on|for agents|esc to interrupt").is_match(text)
}

/// Один проход детектора по сессии (вызывается каждые 7с для всех).
pub async fn detect_stuck_prompt(d: &Arc<Daemon>, sid: &str) {
    let Some(s) = d.session(sid) else { return };
    let Some(pane) = s.tmux_pane.clone() else { return };
    // вопрос уже показан хуком — не вмешиваемся
    if s.question.as_ref().is_some_and(|q| !q.from_screen) {
        return;
    }
    // во время активной генерации на экран не лезем; idle/done/waiting — смотрим
    // (подтверждение появляется как раз когда сессия НЕ генерит)
    if s.status == Status::Working && now_ms() - s.updated_at < 8000 {
        return;
    }
    let Some(screen) = crate::tmux::capture_pane(&pane).await else { return };
    let lines: Vec<&str> = screen.lines().collect();
    // 17, не 18: у JS slice(-18) последний элемент — пустой хвост от trailing \n
    let tail: Vec<&str> = lines.iter().rev().take(17).rev().copied().collect();

    let Some(prompt) = parse_screen(&tail) else {
        // подтверждение ушло — снимаем экранный вопрос
        let had_screen_q = s.question.as_ref().is_some_and(|q| q.from_screen);
        if had_screen_q && is_idle_screen(&tail.join("\n")) {
            d.with_session(sid, |s| {
                s.question = None;
                s.status = Status::Idle;
                s.updated_at = now_ms();
            });
            d.push();
        }
        return;
    };

    // тот же вопрос уже на карточке — не дёргаем
    if let Some(q) = s.question.as_ref().filter(|q| q.from_screen) {
        if let Some(prev) = q.questions.first() {
            if prev.question == prompt.title && prev.options.len() == prompt.options.len() {
                return;
            }
        }
    }

    let project = s.project.clone().unwrap_or_else(|| "?".into());
    let detail = ellipsize(&prompt.title, 140);
    d.with_session(sid, |s| {
        s.status = Status::Waiting;
        s.question = Some(Question {
            from_screen: true,
            at: now_ms(),
            questions: vec![QuestionItem {
                question: prompt.title.clone(),
                header: String::new(),
                multi_select: prompt.multi,
                options: prompt
                    .options
                    .iter()
                    .take(9)
                    .map(|label| QuestionOption {
                        label: ellipsize(&one_line(label), 80),
                        description: String::new(),
                    })
                    .collect(),
            }],
        });
        s.detail = detail.clone();
    });
    if d.settings.bool("notifyWaiting") {
        d.notify(&format!("{project} — спрашивает"), &detail, Some(sid), "waiting");
    }
    d.push();
    // Не пишем текст промпта в лог (конф. данные) — только факт и кол-во опций.
    crate::log::line(&format!("[screen-prompt] {project}: {} опц.", prompt.options.len()));
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
    fn ignores_plain_idle_screen() {
        let tail = vec!["> ", "  bypass permissions on"];
        assert!(parse_screen(&tail).is_none());
        assert!(is_idle_screen("…esc to interrupt…"));
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
    }
}
