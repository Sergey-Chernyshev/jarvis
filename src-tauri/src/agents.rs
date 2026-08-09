//! Реестр внешних агентов: не только claude и codex.
//!
//! Агентских CLI стало много — Qwen Code, OpenCode, свои внутренние утилиты, —
//! а зашитая пара имён означала, что Jarvis их не видит вовсе. Реестр делает
//! агентом любую команду: человек вводит путь до бинарника, остальное —
//! tmux-шим и хуки жизненного цикла — настраивается само.
//!
//! Честная граница возможностей. У чужого CLI нет хуков Claude Code, поэтому
//! Jarvis знает о нём ровно то, что видит сам: сессия началась (шим сказал),
//! сессия закончилась (шим сказал), и живой экран паны между ними. Статусы
//! «думает/спрашивает», транскрипт и расход — это то, что агент должен уметь
//! рассказывать о себе сам; выдумывать их по чужому стеку было бы враньём.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Свой агент из настроек (`customAgents`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CustomAgent {
    /// Идентификатор: имя шима, метка сессий, ключ реестра. Латиница.
    pub id: String,
    /// Человеческое имя для панели. Пустое — берётся id.
    pub name: String,
    /// Бинарь: абсолютный путь или имя, которое найдётся в PATH.
    pub bin: String,
    /// Команда возобновления, `{sid}` подставится. Пустая — агент не умеет.
    pub resume: String,
    /// Флаг «опасного режима» (аналог --dangerously-skip-permissions).
    /// Пустой — у агента такого нет, и мы не выдумываем.
    pub dangerous_flag: String,
}

/// Имена, под которыми свой агент жить не может.
///
/// claude и codex заняты настоящими бэкендами; остальное — служебные имена,
/// столкновение с которыми превращает отладку в археологию.
const RESERVED: &[&str] = &["claude", "codex", "jarvis", "tmux", "sh", "bash", "zsh"];

/// Чем плох этот агент; пусто — годен.
pub fn problems(a: &CustomAgent) -> Vec<String> {
    let mut out = Vec::new();
    let id = a.id.trim();
    if id.is_empty() {
        out.push("нет идентификатора".into());
    } else {
        if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            out.push("идентификатор — только латиница в нижнем регистре, цифры, дефис и подчёркивание".into());
        }
        if id.len() > 24 {
            out.push("идентификатор длиннее 24 символов".into());
        }
        if RESERVED.contains(&id) {
            out.push(format!("имя «{id}» занято"));
        }
    }
    if a.bin.trim().is_empty() {
        out.push("не указан бинарь".into());
    }
    if !a.resume.trim().is_empty() && !a.resume.contains("{sid}") {
        out.push("в команде возобновления нет {sid} — ей нечем назвать сессию".into());
    }
    out
}

/// Прочитать реестр из настроек. Кривые записи пропускаются молча: файл
/// настроек правят и руками, и падать из-за одной битой строки нельзя.
pub fn parse(settings: &Value) -> Vec<CustomAgent> {
    let Some(arr) = settings.get("customAgents").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    arr.iter()
        .filter_map(|v| serde_json::from_value::<CustomAgent>(v.clone()).ok())
        .filter(|a| problems(a).is_empty())
        .filter(|a| seen.insert(a.id.clone()))
        .collect()
}

/// Найти агента по метке сессии.
pub fn find<'a>(agents: &'a [CustomAgent], id: &str) -> Option<&'a CustomAgent> {
    agents.iter().find(|a| a.id == id)
}

/// Команда запуска для терминала.
///
/// Новая сессия — просто имя шима: он лежит в shims-каталоге, который запуск
/// ставит первым в PATH, и оборачивает агента в tmux с хуками жизненного
/// цикла. Возобновление — шаблон человека как есть; о флагах чужого CLI мы
/// не знаем ничего и не выдумываем.
pub fn command(a: &CustomAgent, session_id: Option<&str>, dangerous: bool) -> String {
    let mut cmd = match session_id {
        Some(sid) if !a.resume.trim().is_empty() => a.resume.replace("{sid}", sid),
        _ => a.id.clone(),
    };
    if dangerous && !a.dangerous_flag.trim().is_empty() {
        cmd.push(' ');
        cmd.push_str(a.dangerous_flag.trim());
    }
    cmd
}

/// Пары (id, бинарь) для установщика шимов: install самодостаточен и полную
/// модель агента не знает.
pub fn shim_specs(agents: &[CustomAgent]) -> Vec<(String, String)> {
    agents.iter().map(|a| (a.id.clone(), a.bin.clone())).collect()
}

/// Готовые карточки известных CLI — человек выбирает и правит путь.
///
/// Только имя и бинарь: флаги возобновления у этих утилит меняются от версии
/// к версии, и вписанный наугад шаблон обернулся бы командой, которая молча
/// не работает. Пусть человек добавит его сам, если его версия умеет.
pub fn presets() -> Vec<CustomAgent> {
    vec![
        CustomAgent {
            id: "opencode".into(),
            name: "OpenCode".into(),
            bin: "opencode".into(),
            ..Default::default()
        },
        CustomAgent { id: "pi".into(), name: "Pi".into(), bin: "pi".into(), ..Default::default() },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok_agent() -> CustomAgent {
        CustomAgent {
            id: "qwen".into(),
            name: "Qwen Code".into(),
            bin: "/usr/local/bin/qwen".into(),
            ..Default::default()
        }
    }

    #[test]
    fn reserved_and_junk_ids_are_rejected() {
        for bad in ["claude", "codex", "jarvis", "tmux"] {
            let a = CustomAgent { id: bad.into(), bin: "x".into(), ..Default::default() };
            assert!(!problems(&a).is_empty(), "{bad} не должен пройти");
        }
        for bad in ["Мой Агент", "with space", "UPPER", ""] {
            let a = CustomAgent { id: bad.into(), bin: "x".into(), ..Default::default() };
            assert!(!problems(&a).is_empty(), "«{bad}» не должен пройти");
        }
        assert!(problems(&ok_agent()).is_empty());
    }

    #[test]
    fn resume_template_must_name_the_session() {
        let mut a = ok_agent();
        a.resume = "qwen --continue".into();
        assert!(!problems(&a).is_empty(), "шаблону без {{sid}} нечем назвать сессию");
        a.resume = "qwen --resume {sid}".into();
        assert!(problems(&a).is_empty());
    }

    #[test]
    fn parse_skips_broken_entries_and_duplicates() {
        let v = json!({ "customAgents": [
            { "id": "qwen", "bin": "qwen" },
            { "id": "claude", "bin": "x" },          // занято
            { "id": "qwen", "bin": "другой" },        // дубль
            { "id": "плохой id", "bin": "x" },        // кириллица
            "мусор",
            { "id": "opencode", "bin": "/opt/oc" },
        ]});
        let got = parse(&v);
        assert_eq!(got.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["qwen", "opencode"]);
    }

    #[test]
    fn command_uses_shim_resume_and_dangerous_flag() {
        let mut a = ok_agent();
        // Новая сессия — имя шима, а не путь: шим и оборачивает в tmux.
        assert_eq!(command(&a, None, false), "qwen");
        // Возобновления нет — честно запускаем новую, а не выдумываем флаг.
        assert_eq!(command(&a, Some("s1"), false), "qwen");
        a.resume = "qwen --resume {sid}".into();
        assert_eq!(command(&a, Some("s1"), false), "qwen --resume s1");
        // Опасный режим без флага — ничего не добавляем.
        assert_eq!(command(&a, None, true), "qwen");
        a.dangerous_flag = "--yolo".into();
        assert_eq!(command(&a, None, true), "qwen --yolo");
    }

    #[test]
    fn presets_are_valid_and_do_not_invent_flags() {
        for p in presets() {
            assert!(problems(&p).is_empty(), "{}", p.id);
            assert!(p.resume.is_empty(), "{}: флаг возобновления вписан наугад", p.id);
        }
    }
}
