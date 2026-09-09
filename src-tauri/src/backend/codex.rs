//! Codex-бэкенд (OpenAI `codex`). Sync-методы шва; async/stateful-части —
//! свободными функциями в profильных модулях. Транскрипт/agent-host наполняются
//! по инкрементам (см. план); здесь то, что известно статически.

use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{Agent, Backend};
use crate::transcript::ChatItem;

pub struct CodexBackend;

/// Статический инстанс для диспетчера `backend()`.
pub static CODEX: CodexBackend = CodexBackend;

/// Настоящий `codex` в PATH (+типовые каталоги), минуя наш шим `~/.jarvis/shims`.
pub fn resolve_codex_bin() -> Option<PathBuf> {
    crate::agent_instances::load_registry(&crate::util::jarvis_dir())
        .ok()?.launch_spec(None).ok().map(|spec| spec.program)
}

/// Найти rollout-файл codex по `session_id` (хвост имени файла = uuid сессии):
/// `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<sid>.jsonl`. Safety-net на случай,
/// когда hook-payload codex НЕ принёс `transcript_path` (хуки пропущены по hook-trust):
/// демон всё равно находит транскрипт по sid и достаёт модель + переписку. Возвращает
/// самый свежий матч; обход ограничен глубиной (YYYY/MM/DD — 3-4 уровня).
pub fn find_rollout_by_sid(sid: &str) -> Option<PathBuf> {
    let registry = crate::agent_instances::load_registry(&crate::util::jarvis_dir()).ok()?;
    find_rollout_in_registry(&registry, sid, None)
}

/// A known account never falls back to another home. Legacy events without
/// identity resolve only when exactly one enabled home contains that UUID.
pub fn find_rollout_for_instance(sid: &str, instance_id: &str) -> Option<PathBuf> {
    let registry = crate::agent_instances::load_registry(&crate::util::jarvis_dir()).ok()?;
    find_rollout_in_registry(&registry, sid, Some(instance_id))
}

fn find_rollout_in_registry(registry: &crate::agent_instances::Registry, sid: &str, instance_id: Option<&str>) -> Option<PathBuf> {
    if let Some(id) = instance_id { registry.resolve(Some(id)).ok()?; }
    let mut matches = std::collections::BTreeMap::new();
    for root in registry.roots(true).into_iter().filter(|root| instance_id.map_or(true, |id| root.instance_id == id)) {
        if let Some(path) = find_rollout_in(&root.path, sid) {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let best = matches.entry(root.instance_id).or_insert((modified, path.clone()));
            if modified > best.0 { *best = (modified, path); }
        }
    }
    if matches.len() == 1 { matches.into_values().next().map(|(_, path)| path) } else { None }
}

/// Чистое ядро поиска (тестируется на temp-каталоге без env): рекурсивный обход
/// `root` в поисках файла с хвостом `-<sid>.jsonl`, самый свежий по mtime.
pub(crate) fn find_rollout_in(root: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty() {
        return None;
    }
    let needle = format!("-{sid}.jsonl");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    fn walk(
        dir: &Path,
        needle: &str,
        best: &mut Option<(std::time::SystemTime, PathBuf)>,
        depth: u8,
    ) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, needle, best, depth + 1);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(needle))
            {
                if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().map_or(true, |(bt, _)| mt > *bt) {
                        *best = Some((mt, p));
                    }
                }
            }
        }
    }
    walk(root, &needle, &mut best, 0);
    best.map(|(_, p)| p)
}

impl Backend for CodexBackend {
    fn agent(&self) -> Agent {
        Agent::Codex
    }
    fn cli_found(&self) -> bool {
        resolve_codex_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        if super::codex_transcript::file_is_technical(file) {return Vec::new();}
        super::codex_transcript::display_entries(crate::transcript::read_recent_entries(file, max_bytes))
    }
    fn entries_from_text(&self, text: &str) -> Vec<Value> {
        super::codex_transcript::display_entries(crate::transcript::entries_from_text(text))
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        super::codex_transcript::to_chat_items(entry)
    }
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        super::codex_transcript::extract_title(entries)
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None // session_meta.git отсутствует в rollout — ветка недоступна
    }
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        super::codex_transcript::extract_model(entries)
    }
    fn transcript_dir_for(&self, _cwd: &str) -> Option<PathBuf> {
        None // Codex не кодирует cwd в путь; индекс — инкремент 6 (history)
    }
    fn find_transcript_by_sid(&self, sid: &str) -> Option<PathBuf> {
        find_rollout_by_sid(sid)
    }
    fn final_reply(&self, entries: &[Value]) -> Option<String> {
        super::codex_transcript::full_final_reply(entries)
    }
    fn final_reply_from_stop(&self, payload: &serde_json::Map<String, Value>) -> Option<String> {
        payload
            .get("last_assistant_message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reply| !reply.is_empty())
            .map(String::from)
    }
    fn supports_custom_answer(&self) -> bool {
        false // в codex-пикере строки «Other» нет — свой текст доставить некуда
    }
    fn validate_model(&self, model: &str) -> Result<(), String> {
        // Аллоулистом НЕ ограничиваем: набор моделей OpenAI дрейфует от релиза к
        // релизу, и `models()` здесь — подсказка для пикера, а не полный список.
        // Проверку на «чистоту» оставляем: строка уходит в tmux-пану.
        crate::convo::skills::ensure_clean(model, "модель")
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("codex resume {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        // The exact slug is also the session control identity. Collapsing every
        // GPT-5 variant to "GPT-5" made the model picker select a different model.
        id.to_string()
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-5-codex", "Codex"),
            ("gpt-5", "GPT-5"),
        ]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        &["minimal", "low", "medium", "high", "xhigh"]
    }
    fn has_separate_effort(&self) -> bool {
        false
    }
    fn price(&self, _model: &str) -> (f64, f64) {
        // ОЦЕНКА (OpenAI прайс дрейфует) — gpt-5-класс, $/1M (in, out).
        (1.25, 10.0)
    }
    /// ОЦЕНКА: у gpt-5-класса окно 400k. Незнакомая модель — `None`: набор
    /// моделей Codex дрейфует между релизами, и подставлять чужое число нельзя.
    fn context_window(&self, model: &str) -> Option<u64> {
        let m = model.to_lowercase();
        (m.contains("gpt-5") || m.contains("gpt5") || m.contains("codex")).then_some(400_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollout_lookup_preserves_instance_identity_and_refuses_ambiguous_legacy_ids() {
        use crate::agent_instances::{DiscoveryContext, InstanceConfig, InstanceEntry};
        let root = std::env::temp_dir().join(format!("jarvis-rollout-accounts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("home/.codex"); let personal = root.join("personal");
        for home in [&work, &personal] {
            std::fs::create_dir_all(home.join("sessions/2026/09/05")).unwrap();
            std::fs::write(home.join("sessions/2026/09/05/rollout-0-same-uuid.jsonl"), "{}\n").unwrap();
        }
        let context = DiscoveryContext { machine: "local".into(), home: root.join("home"), codex_home: None, path_dirs: vec![], cli_candidates: vec![], excluded_dirs: vec![] };
        let config = InstanceConfig { entries: vec![InstanceEntry { home: personal.clone(), label: "Personal".into(), enabled: true, machine: "local".into(), cli: None, desktop_launcher: None }], ..Default::default() };
        let registry = crate::agent_instances::discover_with(&config, &context).unwrap();
        assert!(find_rollout_in_registry(&registry, "same-uuid", None).is_none());
        let id = crate::agent_instances::instance_id("local", &personal).unwrap();
        let found = find_rollout_in_registry(&registry, "same-uuid", Some(&id)).unwrap();
        assert!(found.starts_with(std::fs::canonicalize(&personal).unwrap()));
        assert!(find_rollout_in_registry(&registry, "same-uuid", Some("unknown")).is_none());
        std::fs::remove_file(personal.join("sessions/2026/09/05/rollout-0-same-uuid.jsonl")).unwrap();
        std::fs::create_dir_all(personal.join("archived_sessions")).unwrap();
        std::fs::write(personal.join("archived_sessions/rollout-0-archived-uuid.jsonl"), "{}\n").unwrap();
        assert!(find_rollout_in_registry(&registry, "archived-uuid", Some(&id)).unwrap().to_string_lossy().contains("archived_sessions"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn friendly_model_codex_names() {
        assert_eq!(CODEX.friendly_model("gpt-5-codex"), "gpt-5-codex");
        assert_eq!(CODEX.friendly_model("gpt-5.5"), "gpt-5.5");
        assert_eq!(CODEX.resume_cmd("xyz"), "codex resume xyz");
        assert!(!CODEX.has_separate_effort());
    }

    #[test]
    fn find_rollout_locates_by_sid_recursively() {
        // ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<sid>.jsonl — обход вглубь.
        let root = std::env::temp_dir().join("jarvis-find-rollout-test");
        let _ = std::fs::remove_dir_all(&root);
        let day = root.join("2026/06/29");
        std::fs::create_dir_all(&day).unwrap();
        let want = day.join("rollout-200-AAA-BBB.jsonl");
        std::fs::write(&want, b"{}\n").unwrap();
        std::fs::write(day.join("rollout-100-CCC-DDD.jsonl"), b"{}\n").unwrap();

        assert_eq!(
            find_rollout_in(&root, "AAA-BBB").as_deref(),
            Some(want.as_path())
        );
        assert_eq!(find_rollout_in(&root, "ZZZ"), None, "нет матча → None");
        assert_eq!(find_rollout_in(&root, ""), None, "пустой sid → None");

        let _ = std::fs::remove_dir_all(&root);
    }
}
