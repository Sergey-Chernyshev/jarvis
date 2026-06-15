//! Тайминги пайплайна → ~/.jarvis/metrics.jsonl (JSON-lines: одна метрика = одна
//! строка JSON, тривиально анализируется через jq). Пишем ТОЛЬКО в режиме логов
//! (настройка `diagnostics`). Append-only, ротация при разрастании. Best-effort —
//! ошибки записи глотаем, на горячий путь не влияем.
//!
//! Анализ, примеры:
//!   jq -s 'map(select(.kind=="haiku").ms)|add/length'  ~/.jarvis/metrics.jsonl  # средн. haiku
//!   jq 'select(.kind=="tts_synth")'                     ~/.jarvis/metrics.jsonl  # синтез голоса
//!   jq 'select(.kind=="stop_to_notify")|.ms'            ~/.jarvis/metrics.jsonl  # результат→увед

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{json, Value};

use crate::util::{jarvis_dir, now_ms};

static ENABLED: AtomicBool = AtomicBool::new(false);
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Включить/выключить запись метрик (из настройки `diagnostics`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Точка отсчёта для записи длительности.
pub fn now() -> Instant {
    Instant::now()
}

fn path() -> std::path::PathBuf {
    jarvis_dir().join("metrics.jsonl")
}

/// Метрика-длительность: kind + ms (с момента `since`) + произвольные поля.
pub fn record(kind: &str, since: Instant, extra: Value) {
    if !enabled() {
        return;
    }
    let mut obj = match extra {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("ts".into(), json!(now_ms()));
    obj.insert("kind".into(), json!(kind));
    obj.insert("ms".into(), json!(since.elapsed().as_millis() as i64));
    write_line(&Value::Object(obj));
}

/// Метрика-снимок без длительности (напр. RAM/CPU).
pub fn snapshot(kind: &str, fields: Value) {
    if !enabled() {
        return;
    }
    let mut obj = match fields {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("ts".into(), json!(now_ms()));
    obj.insert("kind".into(), json!(kind));
    write_line(&Value::Object(obj));
}

fn write_line(v: &Value) {
    let p = path();
    let _ = std::fs::create_dir_all(jarvis_dir());
    if std::fs::metadata(&p).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&p, p.with_extension("jsonl.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{v}");
    }
}
