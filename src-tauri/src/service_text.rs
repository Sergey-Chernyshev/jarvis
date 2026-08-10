//! Служебные секции агентов, которые не являются частью разговора.
//!
//! Claude и Codex впрыскивают в текст парные теги (`<system-reminder>`,
//! `<oai-mem-citation>`, `<environment_context>` …). Раньше их отсекала
//! проверка «текст начинается с `<`», но агент дописывает такую секцию и в
//! КОНЕЦ обычного ответа — тогда пользователь видел теги прямо в чате.
//! Поэтому секции вырезаются из текста, а не отбрасывают сообщение целиком.

/// Закрытый список: неизвестный тег может быть частью кода или примера
/// (`Vec<String>`, `<div>`), вырезать его нельзя.
pub const SERVICE_SECTION_TAGS: &[&str] = &[
    // Codex
    "oai-mem-citation",
    "citation_entries",
    "rollout_ids",
    "environment_context",
    "codex_internal_context",
    "in-app-browser-context",
    "response-annotations",
    "collaboration_mode",
    "multi_agent_mode",
    "workspace_roots",
    "skills_instructions",
    // Claude
    "system-reminder",
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-caveat",
    "task-notification",
    "user_instructions",
    "user-prompt-submit-hook",
];

/// Убирает парные служебные секции вместе с содержимым и схлопывает пустые
/// строки, оставшиеся на их месте. Непарный открывающий тег обрезает хвост:
/// поток мог оборваться на середине секции.
pub fn strip_service_sections(input: &str) -> String {
    let mut text = input.to_string();
    for tag in SERVICE_SECTION_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        loop {
            let Some(start) = text.find(&open) else { break };
            let mut end = match text[start..].find(&close) {
                Some(offset) => start + offset + close.len(),
                None => text.len(),
            };
            // секция занимала свои строки целиком — забираем и перевод строки
            // после неё, иначе на её месте останется пустая строка внутри абзаца
            if text[end..].starts_with('\n') {
                end += 1;
            }
            text.replace_range(start..end, "");
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut blank = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_service_sections;

    /// Случай со скриншота владельца: Codex дописывает секцию в КОНЕЦ ответа.
    #[test]
    fn service_section_at_the_end_is_cut_out() {
        let reply = "Ничего не удалял.\n\n<oai-mem-citation>\n<citation_entries>\n\
MEMORY.md:22-23|note=[x]\n</citation_entries>\n<rollout_ids>\n</rollout_ids>\n\
</oai-mem-citation>";
        assert_eq!(strip_service_sections(reply), "Ничего не удалял.");
    }

    #[test]
    fn claude_system_reminder_inside_a_message_is_cut_out() {
        let msg = "Сделай Х\n<system-reminder>внутренняя подсказка</system-reminder>\nи Y";
        assert_eq!(strip_service_sections(msg), "Сделай Х\nи Y");
    }

    #[test]
    fn unclosed_section_cuts_the_tail() {
        assert_eq!(strip_service_sections("Готово.\n<oai-mem-citation>\nMEM"), "Готово.");
    }

    #[test]
    fn ordinary_angle_brackets_survive() {
        for source in ["Условие `a < b` и <div> в примере", "Вернул Vec<String>"] {
            assert_eq!(strip_service_sections(source), source);
        }
    }

    #[test]
    fn message_that_is_only_a_service_section_disappears() {
        assert_eq!(strip_service_sections("<task-notification>x</task-notification>"), "");
    }
}
