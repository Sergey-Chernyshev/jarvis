//! Аудит каждого вызова капабилити (§7). First-class: append-only JSONL в
//! `~/.jarvis/audit.jsonl`, читается капабилитей `audit.query`. Сток
//! абстрагирован трейтом, чтобы тесты проверяли запись без файловой системы.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::util::jarvis_dir;

/// Одна запись аудита: кто/что/аргументы/провенанс/исход/время.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub consumer: String,
    pub id: String,
    pub class: &'static str,
    pub args: Value,
    pub provenance: &'static str,
    /// "ok" | "denied:..." | "rejected" | "failed:..." | "notfound"
    pub outcome: String,
    pub ms: u128,
}

impl AuditEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "ts": chrono::Local::now().to_rfc3339(),
            "consumer": self.consumer,
            "id": self.id,
            "class": self.class,
            "args": self.args,
            "provenance": self.provenance,
            "outcome": self.outcome,
            "ms": self.ms as u64,
        })
    }
}

/// Куда писать аудит. `FileAudit` — боевой, `MemAudit` — тесты.
pub trait AuditSink: Send + Sync {
    fn record(&self, entry: &AuditEntry);
}

fn audit_path() -> std::path::PathBuf {
    jarvis_dir().join("audit.jsonl")
}

const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Боевой сток: дописывает строку JSON в `~/.jarvis/audit.jsonl`. Best-effort —
/// ошибки записи глотаем, демон от аудита не зависит (как `log.rs`).
pub struct FileAudit;
impl AuditSink for FileAudit {
    fn record(&self, entry: &AuditEntry) {
        use std::io::Write;
        let path = audit_path();
        let _ = std::fs::create_dir_all(jarvis_dir());
        if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = std::fs::rename(&path, path.with_extension("jsonl.old"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            if let Ok(line) = serde_json::to_string(&entry.to_json()) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

/// Тестовый сток: копит записи в памяти.
pub struct MemAudit {
    pub entries: Mutex<Vec<AuditEntry>>,
}
impl MemAudit {
    pub fn new() -> Self {
        MemAudit { entries: Mutex::new(Vec::new()) }
    }
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
    pub fn last(&self) -> Option<AuditEntry> {
        self.entries.lock().unwrap().last().cloned()
    }
}
impl AuditSink for MemAudit {
    fn record(&self, entry: &AuditEntry) {
        self.entries.lock().unwrap().push(entry.clone());
    }
}

/// Чтение аудита для капабилити `audit.query`. Фильтры (опц.): `consumer`,
/// `id`, `outcome`, `limit` (по умолчанию последние 200).
pub fn query(filter: &Value) -> Vec<Value> {
    let path = audit_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let want = |key: &str| filter.get(key).and_then(|v| v.as_str()).map(|s| s.to_string());
    let (fc, fi, fo) = (want("consumer"), want("id"), want("outcome"));
    let limit = filter.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

    let mut out: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| {
            let m = |k: &str, f: &Option<String>| {
                f.as_ref()
                    .map(|want| e.get(k).and_then(|v| v.as_str()) == Some(want.as_str()))
                    .unwrap_or(true)
            };
            // outcome фильтруем по префиксу (failed:* / denied:*)
            let outcome_ok = fo
                .as_ref()
                .map(|w| {
                    e.get("outcome")
                        .and_then(|v| v.as_str())
                        .map(|o| o == w || o.starts_with(&format!("{w}:")))
                        .unwrap_or(false)
                })
                .unwrap_or(true);
            m("consumer", &fc) && m("id", &fi) && outcome_ok
        })
        .collect();
    // последние `limit`
    let start = out.len().saturating_sub(limit);
    out.drain(..start);
    out
}
