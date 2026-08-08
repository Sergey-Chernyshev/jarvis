//! Запуск агента ради данных, а не ради сессии: сейчас это только лимиты.
//!
//! Лимиты привязаны к аккаунту, а аккаунт живёт на той машине, где стоит агент.
//! Спросить их можно только там — отсюда эта ручка. Разбор ответа остаётся на
//! клиенте: узел отдаёт текст как есть, ровно как отдаёт кусок транскрипта.

use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Сколько живёт закэшированный ответ. `/usage` поднимает настоящий headless-
/// запуск агента: он не быстрый и не бесплатный, а проценты меняются медленно.
const CACHE: Duration = Duration::from_secs(5 * 60);

/// Потолок ожидания. Холодный старт агента бывает долгим, но вечно висеть
/// запрос не должен — телефон на том конце ждёт живого ответа.
const RUN_TIMEOUT: Duration = Duration::from_secs(90);

static CACHED: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// Свежие лимиты аккаунта: текст `claude /usage` как есть.
pub async fn usage(fresh: bool) -> Value {
    if !fresh {
        if let Some((at, text)) = CACHED.lock().unwrap().clone() {
            if at.elapsed() < CACHE {
                return json!({ "text": text, "cached": true, "ageMs": at.elapsed().as_millis() as u64 });
            }
        }
    }
    match run().await {
        Ok(text) => {
            *CACHED.lock().unwrap() = Some((Instant::now(), text.clone()));
            json!({ "text": text, "cached": false, "ageMs": 0 })
        }
        Err(e) => json!({ "text": "", "error": e }),
    }
}

async fn run() -> Result<String, String> {
    // Через логин-шелл с дополненным PATH — по той же причине, что и запуск
    // сессии: под systemd агент иначе просто не находится.
    let script = super::tmux::with_agent_path("claude -p --no-session-persistence /usage");
    let mut cmd = tokio::process::Command::new("bash");
    cmd.args(["-lc", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = timeout(RUN_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "агент не ответил за 90 с".to_string())?
        .map_err(|e| format!("не запустился: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "пустой ответ".into() } else { err });
    }
    Ok(text)
}
