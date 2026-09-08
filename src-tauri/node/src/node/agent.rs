//! Запуск агента ради данных, а не ради сессии: сейчас это только лимиты.
//!
//! Лимиты привязаны к аккаунту, а аккаунт живёт на той машине, где стоит агент.
//! Спросить их можно только там — отсюда эта ручка. Разбор ответа остаётся на
//! клиенте: узел отдаёт текст как есть, ровно как отдаёт кусок транскрипта.

use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Сколько живёт закэшированный ответ. `/usage` поднимает настоящий headless-
/// запуск агента: он не быстрый и не бесплатный, а проценты меняются медленно.
const CACHE: Duration = Duration::from_secs(5 * 60);

/// Потолок ожидания. Холодный старт агента бывает долгим, но вечно висеть
/// запрос не должен — телефон на том конце ждёт живого ответа.
const RUN_TIMEOUT: Duration = Duration::from_secs(90);

static CACHED: OnceLock<Mutex<HashMap<String, (Instant, String)>>> = OnceLock::new();

/// Свежие лимиты аккаунта: текст `claude /usage` как есть.
pub async fn usage(fresh: bool) -> Value { usage_for(fresh, None).await }

pub async fn usage_for(fresh: bool, source: Option<&super::sources::Source>) -> Value {
    if source.is_some_and(|source| source.agent != "claude") {
        return json!({"text":"","error":"Официальная квота Codex через /usage недоступна; расход доступен из транскриптов","sourceId":source.map(|source| &source.id)});
    }
    let key = source.map(|source| source.id.clone()).unwrap_or_else(|| "default-claude".into());
    let cache = CACHED.get_or_init(Default::default);
    if !fresh {
        if let Some((at, text)) = cache.lock().unwrap().get(&key).cloned() {
            if at.elapsed() < CACHE {
                return json!({ "text": text, "cached": true, "ageMs": at.elapsed().as_millis() as u64 });
            }
        }
    }
    match run(source).await {
        Ok(text) => {
            cache.lock().unwrap().insert(key, (Instant::now(), text.clone()));
            json!({ "text": text, "cached": false, "ageMs": 0 })
        }
        Err(e) => json!({ "text": "", "error": e }),
    }
}

async fn run(source: Option<&super::sources::Source>) -> Result<String, String> {
    // Через логин-шелл с дополненным PATH — по той же причине, что и запуск
    // сессии: под systemd агент иначе просто не находится.
    let command = if let Some(source) = source {
        format!("export CLAUDE_CONFIG_DIR={}\nclaude -p --no-session-persistence /usage",super::tmux::sh_quote(&source.home.to_string_lossy()))
    } else { "claude -p --no-session-persistence /usage".into() };
    let script = super::tmux::with_agent_path(&command);
    let mut cmd = tokio::process::Command::new("bash");
    cmd.env("JARVIS_IGNORE", "1").args(["-lc", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = timeout(RUN_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "агент не ответил за 90 с".to_string())?
        .map_err(|e| format!("не запустился: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "пустой ответ".into() } else { err });
    }
    Ok(text)
}
