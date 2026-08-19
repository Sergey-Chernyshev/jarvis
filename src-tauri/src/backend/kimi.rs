//! Kimi-бэкенд (Moonshot `kimi`, Kimi Code CLI). Sync-методы шва; async/stateful-части —
//! свободными функциями в профильных модулях. Транскрипт наполняется по инкрементам
//! (см. спеку `2026-08-19-kimi-cli-support-design.md`); здесь то, что известно статически.
//!
//! Важно про источники: `~/.kimi-code` — это **Kimi Code CLI**, актуальный продукт.
//! Legacy `kimi-cli` жил в `~/.kimi` и имел другой набор хуков; его документацию
//! использовать нельзя.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Agent, Backend};
use crate::transcript::ChatItem;

pub struct KimiBackend;

/// Статический инстанс для диспетчера `backend()`.
pub static KIMI: KimiBackend = KimiBackend;

/// Дом Kimi Code CLI: `$KIMI_CODE_HOME` или `~/.kimi-code`.
pub fn kimi_home() -> PathBuf {
    match std::env::var("KIMI_CODE_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => crate::util::home_dir().join(".kimi-code"),
    }
}

/// Настоящий `kimi` в PATH (+типовые каталоги), минуя наш шим `~/.jarvis/shims`.
///
/// Штатная установка кладёт бинарь в `<дом>/bin/kimi` и добавляет его в PATH,
/// поэтому дом проверяем явно — так детект работает и до правки PATH.
pub fn resolve_kimi_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        kimi_home().join("bin"),
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = crate::util::jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue;
        }
        let p = d.join("kimi");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Найти каталог сессии Kimi по `session_id`.
///
/// Хук Kimi НЕ приносит путь к транскрипту (в отличие от Claude и Codex), поэтому
/// демон обязан находить его сам. Раскладка: `<дом>/sessions/<wd_ключ>/<session_id>/`,
/// где `wd_ключ` = `wd_<слаг>_<sha256(abs_cwd)[..12]>`. Хеш мы намеренно НЕ считаем:
/// это тянуло бы зависимость ради O(1) там, где воркспейсов единицы, — вместо этого
/// перебираем каталоги первого уровня и проверяем наличие подкаталога с именем sid.
pub fn find_session_dir_by_sid(sid: &str) -> Option<PathBuf> {
    find_session_dir_in(&kimi_home().join("sessions"), sid)
}

/// Чистое ядро поиска (тестируется на temp-каталоге без env).
fn find_session_dir_in(root: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty() || sid.contains('/') {
        return None; // пустой или подозрительный sid — не ходим по путям
    }
    let rd = std::fs::read_dir(root).ok()?;
    for e in rd.flatten() {
        let cand = e.path().join(sid);
        if cand.is_dir() {
            return Some(cand);
        }
    }
    None
}

/// Транскрипт главного агента сессии. Сабагенты пишут свои `wire.jsonl`
/// в `agents/agent-N/`; для ленты чата нужен только `main`.
pub fn wire_path_for_sid(sid: &str) -> Option<PathBuf> {
    find_session_dir_by_sid(sid).map(|d| d.join("agents/main/wire.jsonl"))
}

impl Backend for KimiBackend {
    fn agent(&self) -> Agent {
        Agent::Kimi
    }
    fn cli_found(&self) -> bool {
        resolve_kimi_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        // wire.jsonl линейный (append-only, без uuid/parentUuid) → просто хвост JSONL.
        crate::transcript::read_recent_entries(file, max_bytes)
    }
    fn entries_from_text(&self, text: &str) -> Vec<Value> {
        crate::transcript::entries_from_text(text)
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        super::kimi_transcript::to_chat_items(entry)
    }
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        // основной заголовок живёт в state.json рядом с wire.jsonl (читает демон);
        // здесь фолбэк — первая реплика юзера, как у Codex.
        super::kimi_transcript::extract_title(entries)
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None // Kimi не сохраняет ветку нигде — фолбэк по .git/HEAD от cwd
    }
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        super::kimi_transcript::extract_model(entries)
    }
    fn transcript_dir_for(&self, _cwd: &str) -> Option<PathBuf> {
        None // путь к транскрипту резолвится по sid, а не по cwd — см. wire_path_for_sid
    }
    fn find_transcript_by_sid(&self, sid: &str) -> Option<PathBuf> {
        // Для Kimi это не фолбэк, а основной путь: его хуки `transcript_path`
        // не приносят вовсе — проверено на живом payload всех событий.
        wire_path_for_sid(sid).filter(|p| p.exists())
    }
    fn final_reply(&self, entries: &[Value]) -> Option<String> {
        super::kimi_transcript::full_final_reply(entries)
    }
    fn supports_custom_answer(&self) -> bool {
        // Пикер `AskUserQuestion` вживую не откалиброван; пока не проверено —
        // не обещаем. Инкремент 5.
        false
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("kimi -S {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        // На проводе бывает и полный алиас (`kimi-code/k3`), и короткое имя (`k3`).
        let v = id.rsplit('/').next().unwrap_or(id).to_lowercase();
        if v.starts_with("k3") {
            return if v.contains("256k") { "K3-256k" } else { "K3" }.to_string();
        }
        if v.contains("kimi-for-coding") {
            return if v.contains("highspeed") {
                "K2.7 Coding Highspeed"
            } else {
                "K2.7 Coding"
            }
            .to_string();
        }
        v
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("kimi-code/k3", "K3"),
            ("kimi-code/k3-256k", "K3-256k"),
            ("kimi-code/kimi-for-coding", "K2.7 Coding"),
            ("kimi-code/kimi-for-coding-highspeed", "K2.7 Coding Highspeed"),
        ]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        // `support_efforts` модели K3 из config.toml; дефолт — high.
        &["low", "high", "max"]
    }
    fn has_separate_effort(&self) -> bool {
        true // у Kimi effort задаётся отдельно от модели, как у Claude
    }
    fn price(&self, _model: &str) -> (f64, f64) {
        // ОЦЕНКА: официальных цен в конфиге Kimi нет, $/1M (in, out).
        (0.6, 2.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_model_understands_alias_and_short_name() {
        assert_eq!(KIMI.friendly_model("kimi-code/k3"), "K3");
        assert_eq!(KIMI.friendly_model("k3"), "K3");
        assert_eq!(KIMI.friendly_model("kimi-code/k3-256k"), "K3-256k");
        assert_eq!(KIMI.friendly_model("kimi-code/kimi-for-coding"), "K2.7 Coding");
        assert_eq!(
            KIMI.friendly_model("kimi-code/kimi-for-coding-highspeed"),
            "K2.7 Coding Highspeed"
        );
    }

    #[test]
    fn resume_and_effort_shape() {
        assert_eq!(KIMI.resume_cmd("session_abc"), "kimi -S session_abc");
        assert!(KIMI.has_separate_effort(), "effort у Kimi отдельный, не внутри /model");
        assert_eq!(KIMI.effort_levels(), &["low", "high", "max"]);
        assert_eq!(KIMI.models().len(), 4);
    }

    #[test]
    fn find_session_dir_scans_workspaces() {
        // <дом>/sessions/<wd_*>/<session_id>/ — sid лежит на втором уровне.
        let root = std::env::temp_dir().join("jarvis-kimi-session-test");
        let _ = std::fs::remove_dir_all(&root);
        let want = root.join("wd_proj_0123456789ab/session_AAA");
        std::fs::create_dir_all(&want).unwrap();
        std::fs::create_dir_all(root.join("wd_other_ba9876543210/session_BBB")).unwrap();

        assert_eq!(find_session_dir_in(&root, "session_AAA").as_deref(), Some(want.as_path()));
        assert_eq!(find_session_dir_in(&root, "session_ZZZ"), None, "нет матча → None");
        assert_eq!(find_session_dir_in(&root, ""), None, "пустой sid → None");
        assert_eq!(find_session_dir_in(&root, "../etc"), None, "sid с путём отвергаем");

        let _ = std::fs::remove_dir_all(&root);
    }
}
