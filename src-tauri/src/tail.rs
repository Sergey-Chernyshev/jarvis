//! Живой хвост транскрипта: независимая подписка для каждого окна.
//!
//! Инкрементальное чтение по offset с поллом раз в секунду (fs-события на
//! macOS капризны, а stat дёшев). Файла может ещё не быть (свежая сессия до
//! первого промпта) — ждём появления.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use crate::backend::{backend, Agent};

#[derive(Default)]
pub struct TailManager {
    windows: Mutex<HashMap<String, Arc<TailHandle>>>,
}

impl TailManager {
    pub fn new() -> Self { Self::default() }

    pub fn for_window(&self, label: &str) -> Arc<TailHandle> {
        self.windows.lock().unwrap().entry(label.to_owned())
            .or_insert_with(|| Arc::new(TailHandle::for_window(label))).clone()
    }

    pub fn remove_window(&self, label: &str) {
        if let Some(tail) = self.windows.lock().unwrap().remove(label) {
            // Invalidate in-flight remote opens as well as the installed poller.
            tail.stop();
        }
    }

    pub fn is_watching(&self, session_id: &str) -> bool {
        self.windows.lock().unwrap().values()
            .any(|tail| tail.active_session().as_deref() == Some(session_id))
    }
}

pub struct TailHandle {
    owner: String,
    state: Mutex<TailState>,
}

#[derive(Default)]
struct TailState {
    generation: u64,
    current: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Сессия открытого чата — гейт для сводок ходов (Stop суммаризирует
    /// только открытый чат, чтобы не жечь служебный LLM на каждый Stop).
    session: Option<String>,
}

impl TailHandle {
    fn for_window(label: &str) -> Self {
        Self { owner: label.to_owned(), state: Mutex::new(TailState::default()) }
    }

    pub fn stop(&self) {
        self.begin_open();
    }

    /// Reserve at request entry, before remote I/O. A later open or close
    /// invalidates this generation even if its HTTP reply arrives first.
    pub fn begin_open(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        if let Some(h) = state.current.take() {
            h.abort();
        }
        state.session = None;
        state.generation
    }

    fn install(
        &self, generation: u64, session_id: String,
        spawn: impl FnOnce() -> tauri::async_runtime::JoinHandle<()>,
    ) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return false;
        }
        if let Some(previous) = state.current.take() {
            previous.abort();
        }
        state.session = Some(session_id);
        state.current = Some(spawn());
        true
    }

    pub fn start(&self, generation: u64, app: AppHandle, agent: Agent, session_id: String, file: String, from: u64) -> bool {
        self.install(generation, session_id.clone(), || {
            tauri::async_runtime::spawn(tail_loop(app, self.owner.clone(), agent, session_id, PathBuf::from(file), from))
        })
    }

    /// Хвост чата на удалённом узле. Файла тут нет — дочитываем по HTTP с
    /// того смещения, на котором остановилось открытие чата.
    pub fn start_remote(
        &self,
        generation: u64,
        app: AppHandle,
        agent: Agent,
        session_id: String,
        node: std::sync::Arc<crate::remote::Node>,
        file: String,
        from: u64,
    ) -> bool {
        self.install(generation, session_id.clone(), || {
            tauri::async_runtime::spawn(remote_tail_loop(
                app, self.owner.clone(), agent, session_id, node, file, from,
            ))
        })
    }

    /// Сессия, чей чат сейчас открыт (tail активен), либо None.
    pub fn active_session(&self) -> Option<String> {
        self.state.lock().unwrap().session.clone()
    }
}

async fn tail_loop(app: AppHandle, owner: String, agent: Agent, session_id: String, file: PathBuf, from: u64) {
    // Точное смещение history snapshot: новый stat здесь терял записи,
    // дописанные между чтением истории и первым запуском задачи хвоста.
    let mut offset = from;
    let mut rest = Vec::new();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let items = poll_file(agent, &file, &mut offset, &mut rest);
        if !items.is_empty() {
            emit_append(&app, &owner, &session_id, items);
        }
    }
}

/// Хвост удалённого транскрипта. Отличий от локального два: читаем по HTTP и
/// опрашиваем вдвое реже — каждый круг идёт через ssh-туннель, а не через stat
/// локального файла.
async fn remote_tail_loop(
    app: AppHandle,
    owner: String,
    agent: Agent,
    session_id: String,
    node: std::sync::Arc<crate::remote::Node>,
    file: String,
    from: u64,
) {
    let mut offset = from;
    let mut rest = Vec::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Ok(client) = node.client() else { continue }; // туннель переподнимается
        let chunk = match client.file(&file, offset).await {
            Ok(Some(c)) => c,
            Ok(None) => continue, // транскрипта ещё нет
            Err(_) => continue,   // узел моргнул — поллер сам поднимет связь
        };
        let items = append_remote_chunk(agent, &mut offset, &mut rest, &chunk);
        if !items.is_empty() {
            emit_append(&app, &owner, &session_id, items);
        }
    }
}

fn append_remote_chunk(
    agent: Agent, offset: &mut u64, rest: &mut Vec<u8>, chunk: &crate::remote::FileChunk,
) -> Vec<crate::transcript::ChatItem> {
    if chunk.rewound(*offset) {
        rest.clear();
        // Узел зажимает from к новому размеру файла. Нужен новый запрос с 0,
        // иначе всё содержимое переписанного транскрипта будет потеряно.
        *offset = 0;
        return Vec::new();
    }
    *offset = chunk.next;
    parse_append(agent, rest, chunk.data.as_bytes())
}

fn poll_file(
    agent: Agent, file: &Path, offset: &mut u64, rest: &mut Vec<u8>,
) -> Vec<crate::transcript::ChatItem> {
    let Ok(meta) = std::fs::metadata(file) else { return Vec::new(); };
    let size = meta.len();
    if size < *offset {
        *offset = 0;
        rest.clear();
    }
    if size == *offset { return Vec::new(); }
    let Some(chunk) = read_range(file, *offset, size) else { return Vec::new(); };
    *offset += chunk.len() as u64;
    parse_append(agent, rest, &chunk)
}

/// Дописать кусок к недочитанной строке и разобрать всё, что стало целым.
/// `rest` остаётся с новым хвостом-половинкой.
fn parse_append(
    agent: Agent,
    rest: &mut Vec<u8>,
    chunk: &[u8],
) -> Vec<crate::transcript::ChatItem> {
    // UTF-8 декодируем только вместе с целой JSONL-строкой. Иначе байты
    // кириллицы/emoji на стыке чтений заменялись на U+FFFD навсегда.
    rest.extend_from_slice(chunk);
    let Some(end) = rest.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for line in rest[..end].split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
            items.extend(backend(agent).to_chat_items(&v));
        }
    }
    rest.drain(..end);
    items
}

fn emit_append(app: &AppHandle, owner: &str, session_id: &str, items: Vec<crate::transcript::ChatItem>) {
    // Two windows may watch the same session; broadcasting would duplicate items.
    let _ = app.emit_to(
        owner,
        "chat:append",
        &serde_json::json!({ "sessionId": session_id, "items": items }),
    );
}

fn read_range(file: &Path, from: u64, to: u64) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(file).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(to.checked_sub(from)?).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// История и курсор читаются из одного snapshot. Незавершённая строка
/// остаётся в файле: tail дочитает её с начала, включая неполный UTF-8.
pub fn read_snapshot(agent: Agent, file: &Path, max_bytes: u64) -> (Vec<serde_json::Value>, u64) {
    let Some(size) = std::fs::metadata(file).ok().map(|m| m.len()) else {
        return (Vec::new(), 0);
    };
    let start = size.saturating_sub(max_bytes);
    let Some(bytes) = read_range(file, start, size) else {
        return (Vec::new(), 0);
    };
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let head = if start == 0 { 0 } else {
        bytes[..end].iter().position(|&b| b == b'\n').map_or(end, |i| i + 1)
    };
    let text = String::from_utf8_lossy(&bytes[head..end]);
    (backend(agent).entries_from_text(&text), start + end as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_line_is_held_until_it_is_complete() {
        let mut rest = Vec::new();
        // строка приехала половинками — разобрать её можно только целиком
        let a = parse_append(Agent::Claude, &mut rest, b"{\"type\":\"user\"");
        assert!(a.is_empty());
        assert_eq!(rest, b"{\"type\":\"user\"");
        let b = parse_append(Agent::Claude, &mut rest, ",\"message\":{\"content\":\"привет\"}}\n".as_bytes());
        assert_eq!(b.len(), 1, "склеенная строка разобралась");
        assert!(rest.is_empty());
    }

    #[test]
    fn utf8_survives_every_possible_chunk_boundary() {
        let line = "{\"type\":\"user\",\"message\":{\"content\":\"Привет 👋\"}}\n";
        for split in 0..line.len() {
            let mut rest = Vec::new();
            let mut items = parse_append(Agent::Claude, &mut rest, &line.as_bytes()[..split]);
            items.extend(parse_append(Agent::Claude, &mut rest, &line.as_bytes()[split..]));
            assert_eq!(items.len(), 1, "split={split}");
            assert_eq!(items[0].text, "Привет 👋", "split={split}");
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn snapshot_cursor_does_not_skip_new_or_partial_records() {
        let file = std::env::temp_dir().join(format!("jarvis-tail-snapshot-{}.jsonl", std::process::id()));
        let first = "{\"uuid\":\"u1\",\"type\":\"user\",\"message\":{\"content\":\"первый\"}}\n";
        let second = "{\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"type\":\"user\",\"message\":{\"content\":\"второй\"}}\n";
        let split = second.find('в').unwrap() + 1; // середина UTF-8
        std::fs::write(&file, [first.as_bytes(), &second.as_bytes()[..split]].concat()).unwrap();
        let (history, offset) = read_snapshot(Agent::Claude, &file, 512 * 1024);
        assert_eq!(history.len(), 1);
        assert_eq!(offset, first.len() as u64);
        std::fs::write(&file, format!("{first}{second}")).unwrap();
        let bytes = read_range(&file, offset, (first.len() + second.len()) as u64).unwrap();
        let items = parse_append(Agent::Claude, &mut Vec::new(), &bytes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "второй");
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn truncated_file_discards_partial_record_from_previous_file() {
        let file = std::env::temp_dir().join(format!("jarvis-tail-rewind-{}.jsonl", std::process::id()));
        let old = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{}", "a".repeat(200));
        std::fs::write(&file, old).unwrap();
        let mut offset = 0;
        let mut rest = Vec::new();
        assert!(poll_file(Agent::Claude, &file, &mut offset, &mut rest).is_empty());
        assert!(!rest.is_empty());
        std::fs::write(&file, "{\"type\":\"user\",\"message\":{\"content\":\"новый\"}}\n").unwrap();
        let items = poll_file(Agent::Claude, &file, &mut offset, &mut rest);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "новый");
        assert!(rest.is_empty());
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn remote_truncation_reloads_from_zero_instead_of_skipping_new_file() {
        let mut offset = 500;
        let mut rest = b"{\"old\":".to_vec();
        let rewound = crate::remote::FileChunk { from: 40, next: 40, size: 40, ..Default::default() };
        assert!(append_remote_chunk(Agent::Claude, &mut offset, &mut rest, &rewound).is_empty());
        assert_eq!(offset, 0);
        assert!(rest.is_empty());
        let data = "{\"type\":\"user\",\"message\":{\"content\":\"новый\"}}\n".to_string();
        let reloaded = crate::remote::FileChunk { from: 0, next: data.len() as u64, data, ..Default::default() };
        let items = append_remote_chunk(Agent::Claude, &mut offset, &mut rest, &reloaded);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "новый");
    }

    #[test]
    fn active_session_none_by_default_and_after_stop() {
        let t = TailHandle::for_window("main");
        assert_eq!(t.active_session(), None);
        t.stop();
        assert_eq!(t.active_session(), None);
    }

    #[test]
    fn late_open_cannot_replace_a_newer_chat_or_restart_after_close() {
        let t = TailHandle::for_window("main");
        let old = t.begin_open();
        let new = t.begin_open();
        assert!(t.install(new, "new".into(), || tauri::async_runtime::spawn(std::future::pending())));
        assert!(!t.install(old, "old".into(), || panic!("stale request must not spawn a tail")));
        assert_eq!(t.active_session().as_deref(), Some("new"));
        let pending = t.begin_open();
        assert_eq!(t.active_session(), None);
        t.stop();
        assert!(!t.install(pending, "closed".into(), || panic!("closed chat must not restart")));
        assert_eq!(t.active_session(), None);
    }

    #[test]
    fn windows_keep_independent_tails_and_closing_invalidates_pending_opens() {
        let manager = TailManager::new();
        let a = manager.for_window("workspace");
        let b = manager.for_window("workspace-chat");
        assert!(Arc::ptr_eq(&a, &manager.for_window("workspace")));
        assert_eq!(b.owner, "workspace-chat");
        let generation_a = a.begin_open();
        let generation_b = b.begin_open();
        assert!(a.install(generation_a, "first".into(), || tauri::async_runtime::spawn(std::future::pending())));
        assert!(b.install(generation_b, "second".into(), || tauri::async_runtime::spawn(std::future::pending())));
        assert!(manager.is_watching("first"));
        assert!(manager.is_watching("second"));
        let pending = a.begin_open();
        manager.remove_window("workspace");
        assert!(!a.install(pending, "late".into(), || panic!("closed window must not restart")));
        assert!(!manager.is_watching("first"));
        assert!(manager.is_watching("second"));
        manager.remove_window("workspace-chat");
        assert!(!manager.is_watching("second"));
    }
}
