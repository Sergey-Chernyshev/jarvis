//! Кэш сводок ходов на диске + генерация (LLM-слой поверх turns.rs).
//! Файл на сессию: ~/.jarvis/turn-summaries/<sid>.json, версия = PROMPT_VERSION
//! (смена промпта/схемы инвалидирует кэш целиком).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::turns::{TurnCard, PROMPT_VERSION};
use crate::util::jarvis_dir;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    v: u32,
    turns: HashMap<String, TurnCard>,
}

/// Один write-lock на все сессии: записи редкие и мелкие, гранулярность не нужна.
static WRITE: Mutex<()> = Mutex::new(());

fn sanitize(sid: &str) -> String {
    sid.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

fn dir() -> PathBuf {
    jarvis_dir().join("turn-summaries")
}

fn file_for(base: &Path, sid: &str) -> PathBuf {
    base.join(format!("{}.json", sanitize(sid)))
}

fn load_in(base: &Path, sid: &str) -> HashMap<String, TurnCard> {
    let Ok(raw) = fs::read_to_string(file_for(base, sid)) else {
        return HashMap::new();
    };
    match serde_json::from_str::<CacheFile>(&raw) {
        Ok(c) if c.v == PROMPT_VERSION => c.turns,
        _ => HashMap::new(), // битый или старая версия — пересоберётся лениво
    }
}

fn save_in(base: &Path, sid: &str, key: &str, card: &TurnCard) {
    let _g = WRITE.lock().unwrap();
    let mut c = CacheFile { v: PROMPT_VERSION, turns: load_in(base, sid) };
    c.turns.insert(key.to_string(), card.clone());
    let _ = fs::create_dir_all(base);
    if let Ok(json) = serde_json::to_string(&c) {
        let _ = fs::write(file_for(base, sid), json);
    }
}

/// Кэш сводок сессии (пустой, если файла нет или версия промпта сменилась).
pub fn load_cards(sid: &str) -> HashMap<String, TurnCard> {
    load_in(&dir(), sid)
}

pub fn save_card(sid: &str, key: &str, card: &TurnCard) {
    save_in(&dir(), sid, key, card);
}

#[cfg(test)]
mod tests {
    use super::*;

    // каталог на тест (cargo test параллелен — общий каталог дал бы гонку)
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-turnsum-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_load_roundtrip() {
        let base = tmp("roundtrip");
        let card = TurnCard { summary: "Сделано.".into(), ..Default::default() };
        save_in(&base, "sid-1", "100", &card);
        let got = load_in(&base, "sid-1");
        assert_eq!(got.get("100"), Some(&card));
        assert!(load_in(&base, "нет-такой").is_empty());
    }

    #[test]
    fn version_mismatch_resets() {
        let base = tmp("version");
        fs::create_dir_all(&base).unwrap();
        fs::write(
            file_for(&base, "sid-2"),
            r#"{"v": 0, "turns": {"1": {"summary": "старьё", "files": [], "docs_digest": "", "commands": ""}}}"#,
        )
        .unwrap();
        assert!(load_in(&base, "sid-2").is_empty(), "старая версия промпта → кэш пуст");
    }

    #[test]
    fn sid_sanitized_for_filename() {
        assert_eq!(sanitize("a1-b2"), "a1-b2");
        assert_eq!(sanitize("../evil/й"), "evil");
    }
}
