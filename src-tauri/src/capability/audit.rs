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
    /// "asked" | "ok" | "denied:..." | "rejected" | "expired" | "stale" |
    /// "failed:..." | "notfound".
    ///
    /// `asked` — не исход, а вопрос: карточка показана, человек ещё думает.
    /// Пишется ДО ожидания, потому что вопрос, убитый перезапуском демона, иначе
    /// не оставляет ни строки: агент видит «демон недоступен», а человек —
    /// ничего. Пара «asked → исход» сходится по полю `ask`; `asked` без пары —
    /// ровно тот случай, когда спросили и не дождались.
    pub outcome: String,
    pub ms: u128,
    /// Идентификатор вопроса — только у пары строк подтверждения.
    pub ask: Option<String>,
}

impl AuditEntry {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "ts": chrono::Local::now().to_rfc3339(),
            "consumer": self.consumer,
            "id": self.id,
            "class": self.class,
            "args": self.args,
            "provenance": self.provenance,
            "outcome": self.outcome,
            "ms": self.ms as u64,
        });
        // Ключ появляется только там, где он что-то значит: строкам без
        // подтверждения пустое поле `ask` не нужно, а старые строки журнала
        // обязаны читаться дальше — формат прежний плюс необязательный ключ.
        if let (Some(ask), Some(obj)) = (self.ask.as_ref(), v.as_object_mut()) {
            obj.insert("ask".into(), Value::String(ask.clone()));
        }
        v
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

/// Вопросы, на которые никто не ответил: строка `asked` без парной строки
/// исхода. Ровно этот случай оставляет перезапуск демона — агенту уходит
/// «демон недоступен», а человеку до появления `asked` не доставалось ничего.
///
/// Чистая функция над уже разобранными строками: пары ищутся по полю `ask`,
/// порядок сохраняется — журнал append-only, и «последний» тут значит «свежий».
pub fn unanswered(rows: &[Value]) -> Vec<Value> {
    let asks: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|e| e.get("outcome").and_then(Value::as_str) != Some("asked"))
        .filter_map(|e| e.get("ask").and_then(Value::as_str))
        .collect();
    rows.iter()
        .filter(|e| e.get("outcome").and_then(Value::as_str) == Some("asked"))
        .filter(|e| !e.get("ask").and_then(Value::as_str).is_some_and(|a| asks.contains(a)))
        .cloned()
        .collect()
}

/// Чтение аудита для капабилити `audit.query`. Фильтры (опц.): `consumer`,
/// `id`, `outcome`, `limit` (по умолчанию последние 200) и `unanswered` —
/// «спросили и не дождались».
pub fn query(filter: &Value) -> Vec<Value> {
    let path = audit_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let want = |key: &str| filter.get(key).and_then(|v| v.as_str()).map(|s| s.to_string());
    let (fc, fi, fo) = (want("consumer"), want("id"), want("outcome"));
    let limit = filter.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

    let rows: Vec<Value> =
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    // Пары ищем по ВСЕМУ журналу, а не по отфильтрованному куску: иначе фильтр
    // сам бы и «потерял» строку исхода, а вопрос выглядел бы висящим.
    let rows = if filter.get("unanswered").and_then(Value::as_bool) == Some(true) {
        unanswered(&rows)
    } else {
        rows
    };

    let mut out: Vec<Value> = rows
        .into_iter()
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
        .collect::<Vec<Value>>();
    // последние `limit`
    let start = out.len().saturating_sub(limit);
    out.drain(..start);
    out
}
