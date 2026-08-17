//! Живой хвост транскрипта открытого чата: панель смотрит один чат за раз.
//!
//! Инкрементальное чтение по offset с поллом раз в секунду (fs-события на
//! macOS капризны, а stat дёшев). Файла может ещё не быть (свежая сессия до
//! первого промпта) — ждём появления.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::AppHandle;

use crate::backend::{backend, Agent};
use crate::windows;

pub struct TailHandle {
    current: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Сессия открытого чата — гейт для сводок ходов (Stop суммаризирует
    /// только открытый чат, чтобы не жечь служебный LLM на каждый Stop).
    session: Mutex<Option<String>>,
}

impl TailHandle {
    pub fn new() -> Self {
        Self { current: Mutex::new(None), session: Mutex::new(None) }
    }

    pub fn stop(&self) {
        if let Some(h) = self.current.lock().unwrap().take() {
            h.abort();
        }
        *self.session.lock().unwrap() = None;
    }

    pub fn start(&self, app: AppHandle, agent: Agent, session_id: String, file: String) {
        self.stop();
        *self.session.lock().unwrap() = Some(session_id.clone());
        let handle = tauri::async_runtime::spawn(tail_loop(app, agent, session_id, PathBuf::from(file)));
        *self.current.lock().unwrap() = Some(handle);
    }

    /// Хвост чата на удалённом узле. Файла тут нет — дочитываем по HTTP с
    /// того смещения, на котором остановилось открытие чата.
    pub fn start_remote(
        &self,
        app: AppHandle,
        agent: Agent,
        session_id: String,
        node: std::sync::Arc<crate::remote::Node>,
        file: String,
        from: u64,
    ) {
        self.stop();
        *self.session.lock().unwrap() = Some(session_id.clone());
        let handle = tauri::async_runtime::spawn(remote_tail_loop(
            app, agent, session_id, node, file, from,
        ));
        *self.current.lock().unwrap() = Some(handle);
    }

    /// Сессия, чей чат сейчас открыт (tail активен), либо None.
    pub fn active_session(&self) -> Option<String> {
        self.session.lock().unwrap().clone()
    }
}

async fn tail_loop(app: AppHandle, agent: Agent, session_id: String, file: PathBuf) {
    // стартуем с текущего конца: историю уже отдал chat:open
    let mut offset: u64 = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let mut rest = String::new();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Ok(meta) = std::fs::metadata(&file) else { continue }; // файла ещё нет
        let size = meta.len();
        if size < offset {
            offset = 0; // файл переписали с нуля — начинаем заново
        }
        if size == offset {
            continue;
        }
        let chunk = match read_range(&file, offset, size) {
            Some(c) => c,
            None => continue,
        };
        offset = size;
        let items = parse_append(agent, &mut rest, &chunk);
        if !items.is_empty() {
            emit_append(&app, &session_id, items);
        }
    }
}

/// Хвост удалённого транскрипта. Отличий от локального два: читаем по HTTP и
/// опрашиваем вдвое реже — каждый круг идёт через ssh-туннель, а не через stat
/// локального файла.
async fn remote_tail_loop(
    app: AppHandle,
    agent: Agent,
    session_id: String,
    node: std::sync::Arc<crate::remote::Node>,
    file: String,
    from: u64,
) {
    let mut offset = from;
    let mut rest = String::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Ok(client) = node.client() else { continue }; // туннель переподнимается
        let chunk = match client.file(&file, offset).await {
            Ok(Some(c)) => c,
            Ok(None) => continue, // транскрипта ещё нет
            Err(_) => continue,   // узел моргнул — поллер сам поднимет связь
        };
        if chunk.rewound(offset) {
            // файл переписали с нуля (/clear, новый rollout) — старый хвост
            // приклеивать не к чему
            rest.clear();
        }
        offset = chunk.next;
        if chunk.data.is_empty() {
            continue;
        }
        let items = parse_append(agent, &mut rest, &chunk.data);
        if !items.is_empty() {
            emit_append(&app, &session_id, items);
        }
    }
}

/// Дописать кусок к недочитанной строке и разобрать всё, что стало целым.
/// `rest` остаётся с новым хвостом-половинкой.
fn parse_append(
    agent: Agent,
    rest: &mut String,
    chunk: &str,
) -> Vec<crate::transcript::ChatItem> {
    let combined = format!("{rest}{chunk}");
    let mut lines: Vec<&str> = combined.split('\n').collect();
    let tail = lines.pop().unwrap_or("").to_string(); // неполная строка ждёт следующего чтения
    let mut items = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            items.extend(backend(agent).to_chat_items(&v));
        }
    }
    *rest = tail;
    items
}

fn emit_append(app: &AppHandle, session_id: &str, items: Vec<crate::transcript::ChatItem>) {
    windows::emit_to_panel(
        app,
        "chat:append",
        &serde_json::json!({ "sessionId": session_id, "items": items }),
    );
}

fn read_range(file: &PathBuf, from: u64, to: u64) -> Option<String> {
    let mut f = std::fs::File::open(file).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![0u8; (to - from) as usize];
    f.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_line_is_held_until_it_is_complete() {
        let mut rest = String::new();
        // строка приехала половинками — разобрать её можно только целиком
        let a = parse_append(Agent::Claude, &mut rest, "{\"type\":\"user\"");
        assert!(a.is_empty());
        assert_eq!(rest, "{\"type\":\"user\"");
        let b = parse_append(Agent::Claude, &mut rest, ",\"message\":{\"content\":\"привет\"}}\n");
        assert_eq!(b.len(), 1, "склеенная строка разобралась");
        assert!(rest.is_empty());
    }

    #[test]
    fn active_session_none_by_default_and_after_stop() {
        let t = TailHandle::new();
        assert_eq!(t.active_session(), None);
        t.stop();
        assert_eq!(t.active_session(), None);
    }
}
