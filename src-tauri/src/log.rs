//! Файловый лог демона: ~/.jarvis/jarvis.log.
//!
//! Нужен, чтобы постфактум разбирать поведение без подключения к stdout:
//! поток событий хуков, статусы доставки/уведомлений, тайминги пайплайна.
//! НЕ пишем конфиденциальное: текст промптов/ответов агента, тело уведомлений,
//! содержимое транскриптов — только метки событий, типы и усечённые id сессий.
//! Best-effort — ошибки записи глотаем, демон от лога не зависит.

use std::io::Write;

use crate::util::jarvis_dir;

const MAX_BYTES: u64 = 4 * 1024 * 1024; // при разрастании — ротация в .old

fn log_path() -> std::path::PathBuf {
    jarvis_dir().join("jarvis.log")
}

/// Локальная метка времени ЧЧ:ММ:СС.мс (chrono уже в зависимостях).
fn stamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

/// Дописать строку в лог (и продублировать в stdout — его ловит nohup).
pub fn line(msg: &str) {
    if cfg!(test) {
        return; // юнит-тесты не должны писать в боевой ~/.jarvis/jarvis.log
    }
    println!("{msg}"); // stdout → daemon.log при запуске под nohup
    let path = log_path();
    let _ = std::fs::create_dir_all(jarvis_dir());
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", stamp(), msg);
    }
}
