//! Session-scoped terminal access. Desktop stream handles never expose a raw
//! remote handle or let renderer payloads select a different machine or pane.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{daemon::Daemon, model::Session, remote::RemoteStatus, tmux::TerminalKey};

const MAX_SCREEN_BYTES: usize = 128 * 1024;
const MAX_STREAMS: usize = 64;
const STREAM_TTL: Duration = Duration::from_secs(300);
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_PASTE_BYTES: usize = 1024 * 1024;
static STREAMS: OnceLock<Mutex<HashMap<String, StreamBinding>>> = OnceLock::new();
static STREAM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionIdentity {
    id: String,
    created_at: i64,
    remote: Option<String>,
    pane: String,
    agent: Option<String>,
    instance_id: Option<String>,
    provider_session_id: Option<String>,
    provider_home: Option<String>,
    control_mode: Option<String>,
}

impl SessionIdentity {
    fn of(session: &Session) -> Result<Self, String> {
        let pane = pane_id(session).ok_or("Сессия не подключена к tmux")?;
        Ok(Self {
            id: session.id.clone(),
            created_at: session.created_at,
            remote: session.remote.clone(),
            pane: pane.into(),
            // Hook pid is the short-lived hook's parent, not necessarily the
            // pane process. The stream backend pins actual tmux server/pane
            // PIDs; routine hook events must not invalidate this binding.
            agent: session.agent.clone(),
            instance_id: session.instance_id.clone(),
            provider_session_id: session.provider_session_id.clone(),
            provider_home: session.provider_home.clone(),
            control_mode: session.control_mode.clone(),
        })
    }
}

#[derive(Clone)]
enum StreamRoute {
    Local,
    Remote(Arc<crate::remote::Node>),
}

impl StreamRoute {
    fn same_machine(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local, Self::Local) => true,
            // A settings reload/replacement requires a fresh open even when
            // the visible node name is unchanged. Never reuse its old handle.
            (Self::Remote(a), Self::Remote(b)) => Arc::ptr_eq(a, b) && a.cfg == b.cfg,
            _ => false,
        }
    }

    async fn dispatch(&self, action: &str, payload: &Value) -> Value {
        match self {
            Self::Local => crate::terminal_stream::dispatch(action, payload).await,
            Self::Remote(node) => node.terminal_action(action, payload).await,
        }
    }
}

#[derive(Clone)]
struct StreamBinding {
    owner: SessionIdentity,
    snapshot: Session,
    route: StreamRoute,
    backend_id: String,
    touched: Instant,
}

fn streams() -> &'static Mutex<HashMap<String, StreamBinding>> {
    STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_route(session: &Session, d: &Daemon) -> Result<StreamRoute, String> {
    resolve_remote(session.remote.as_deref(), |name| d.remotes.node(name))
}

fn resolve_remote(
    name: Option<&str>,
    lookup: impl FnOnce(&str) -> Option<Arc<crate::remote::Node>>,
) -> Result<StreamRoute, String> {
    match name {
        None => Ok(StreamRoute::Local),
        Some(name) => lookup(name)
            .map(StreamRoute::Remote)
            .ok_or_else(|| format!("Узел «{name}» не подключён")),
    }
}

fn check_owner(
    binding: &StreamBinding,
    session: &Session,
    route: &StreamRoute,
) -> Result<(), String> {
    if binding.owner != SessionIdentity::of(session)? || !binding.route.same_machine(route) {
        return Err("Сессия или её терминал изменились. Подключись к терминалу заново".into());
    }
    Ok(())
}

fn terminal_session(d: &Daemon, session_id: &str) -> Option<Session> {
    d.session(session_id).or_else(|| crate::launch_task::terminal_session(session_id))
}

fn current_route(
    d: &Daemon,
    session_id: &str,
    binding: &StreamBinding,
    allow_drain: bool,
) -> Result<StreamRoute, String> {
    let session = terminal_session(d, session_id);
    let route = resolve_remote(binding.owner.remote.as_deref(), |name| d.remotes.node(name))?;
    check_current_owner(binding, session.as_ref(), &route, allow_drain)?;
    Ok(route)
}

fn check_current_owner(
    binding: &StreamBinding,
    session: Option<&Session>,
    route: &StreamRoute,
    allow_drain: bool,
) -> Result<(), String> {
    match session {
        Some(session) => check_owner(binding, session, route),
        // Poll only reads the old control client's bounded ring. A vanished
        // session must not discard its final page, or select a new pane/node.
        None if allow_drain && binding.route.same_machine(route) => Ok(()),
        None => Err("Сессия не найдена или её узел изменился".into()),
    }
}

/// Whitelist fields before injecting the trusted pane/stream handle. Unknown
/// fields are errors, including target/path overrides on otherwise valid calls.
fn checked_payload(action: &str, payload: &Value) -> Result<Value, String> {
    let fields: &[&str] = match action {
        "open" => &["historyLines"],
        "poll" => &["streamId", "cursor"],
        "input" => &["streamId", "data", "paste"],
        "resize" => &["streamId", "cols", "rows"],
        "close" | "history" | "export" => &["streamId"],
        _ => return Err("Неизвестное действие терминала".into()),
    };
    let object = payload
        .as_object()
        .ok_or("Параметры терминала должны быть объектом")?;
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("Недопустимые параметры терминала".into());
    }
    if action != "open" {
        let id = payload["streamId"]
            .as_str()
            .ok_or("Не указан поток терминала")?;
        if id.is_empty()
            || id.len() > 160
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err("Некорректный поток терминала".into());
        }
    }
    match action {
        "open"
            if payload.get("historyLines").is_some_and(|value| {
                !value.as_u64().is_some_and(|v| (2000..=100000).contains(&v))
            }) =>
        {
            return Err("Некорректный размер истории терминала".into());
        }
        "poll" if payload["cursor"].as_u64().is_none() => {
            return Err("Некорректный курсор терминала".into())
        }
        "input" => {
            if payload
                .get("paste")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("Некорректный режим вставки терминала".into());
            }
            let data = payload["data"]
                .as_array()
                .ok_or("Ввод терминала должен содержать байты")?;
            let limit = if payload["paste"] == true {
                MAX_PASTE_BYTES
            } else {
                MAX_INPUT_BYTES
            };
            if data.is_empty()
                || data.len() > limit
                || data.iter().any(|v| v.as_u64().map_or(true, |b| b > 255))
            {
                return Err("Некорректный или слишком большой ввод терминала".into());
            }
            if payload["paste"] == true {
                let bytes: Vec<u8> = data.iter().map(|v| v.as_u64().unwrap() as u8).collect();
                if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                    return Err("Вставка должна быть текстом UTF-8 без нулевых байтов".into());
                }
            }
        }
        "resize" => {
            if !payload["cols"]
                .as_u64()
                .is_some_and(|v| (20..=500).contains(&v))
                || !payload["rows"]
                    .as_u64()
                    .is_some_and(|v| (2..=300).contains(&v))
            {
                return Err("Некорректный размер терминала".into());
            }
        }
        _ => {}
    }
    Ok(payload.clone())
}

fn failure(session_id: &str, message: impl Into<String>) -> Value {
    json!({"ok": false, "sessionId": session_id, "error": message.into()})
}

fn with_connection(mut reply: Value, session: &Session, route: &StreamRoute) -> Value {
    let status = match route {
        StreamRoute::Remote(node) => Some(node.status()),
        _ => None,
    };
    let mut info = connection(session, status.as_ref());
    let ok = reply["ok"] == true;
    info["canRead"] = json!(ok);
    info["canInput"] = json!(ok && reply["closed"] != true);
    if ok {
        info["connected"] = json!(true);
        info["error"] = json!("");
    }
    if let Some(error) = reply.get("error").and_then(Value::as_str) {
        info["error"] = json!(error);
    }
    reply["sessionId"] = json!(session.id);
    reply["connection"] = info;
    reply
}

async fn close_binding(binding: StreamBinding) {
    let _ = binding
        .route
        .dispatch("close", &json!({"streamId": binding.backend_id}))
        .await;
}

pub async fn action(
    d: &Arc<Daemon>,
    session_id: &str,
    action: &str,
    payload: &Value,
    downloads: Option<PathBuf>,
) -> Value {
    let mut request = match checked_payload(action, payload) {
        Ok(request) => request,
        Err(error) => return failure(session_id, error),
    };
    let binding = if action != "open" {
        let id = request["streamId"].as_str().unwrap();
        let mut all = streams().lock().unwrap_or_else(|e| e.into_inner());
        match all.get_mut(id) {
            Some(binding) if binding.owner.id == session_id => {
                binding.touched = Instant::now();
                Some(binding.clone())
            }
            _ => {
                return failure(
                    session_id,
                    "Поток не принадлежит этой сессии или уже закрыт",
                )
            }
        }
    } else {
        None
    };
    let session = match terminal_session(d, session_id) {
        Some(session) => session,
        None if action == "poll" => binding.as_ref().unwrap().snapshot.clone(),
        None => return failure(session_id, "Сессия не найдена"),
    };
    let owner = match SessionIdentity::of(&session) {
        Ok(owner) => owner,
        Err(error) => return unavailable(&session, connection(&session, None), error),
    };
    let route = match resolve_route(&session, d) {
        Ok(route) => route,
        Err(error) => return failure(session_id, error),
    };

    if action == "open" {
        let expired = {
            let mut all = streams().lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<_> = all
                .iter()
                .filter(|(_, b)| b.touched.elapsed() > STREAM_TTL)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| all.remove(&id))
                .collect::<Vec<_>>()
        };
        for binding in expired {
            tauri::async_runtime::spawn(close_binding(binding));
        }
        request["pane"] = json!(owner.pane);
        let mut reply = route.dispatch("open", &request).await;
        if reply["ok"] != true {
            return with_connection(reply, &session, &route);
        }
        let Some(backend_id) = reply["streamId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 160)
            .map(str::to_string)
        else {
            return failure(session_id, "Терминал не вернул идентификатор потока");
        };
        let binding = StreamBinding {
            owner,
            snapshot: session.clone(),
            route: route.clone(),
            backend_id,
            touched: Instant::now(),
        };
        if let Err(error) = current_route(d, session_id, &binding, false) {
            close_binding(binding).await;
            return failure(session_id, error);
        }
        let id = format!(
            "terminal-{}-{}-{}",
            crate::util::now_ms(),
            std::process::id(),
            STREAM_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let inserted = {
            let mut all = streams().lock().unwrap_or_else(|e| e.into_inner());
            if all.len() >= MAX_STREAMS {
                false
            } else {
                all.insert(id.clone(), binding.clone());
                true
            }
        };
        if !inserted {
            close_binding(binding).await;
            return failure(
                session_id,
                "Слишком много открытых терминалов. Закрой неиспользуемые вкладки",
            );
        }
        reply["streamId"] = json!(id);
        return with_connection(reply, &session, &route);
    }

    let id = request["streamId"].as_str().unwrap().to_string();
    let binding = binding.unwrap();
    if let Err(error) = check_owner(&binding, &session, &route) {
        streams()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        close_binding(binding).await;
        return failure(session_id, error);
    }
    request["streamId"] = json!(binding.backend_id);
    if action == "close" {
        streams()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
    let backend_action = if action == "export" {
        "history"
    } else {
        action
    };
    let mut reply = route.dispatch(backend_action, &request).await;
    if action != "close" {
        // An in-flight poll/history/open must not expose data after a session
        // is rebound while the network request is pending.
        if let Err(error) = current_route(d, session_id, &binding, action == "poll") {
            streams()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            close_binding(binding).await;
            return failure(session_id, error);
        }
    }
    if reply.get("streamId").is_some() {
        reply["streamId"] = json!(id);
    }
    if action == "export" && reply["ok"] == true {
        let Some(text) = reply["text"].as_str().map(str::to_string) else {
            return failure(session_id, "Терминал не вернул историю");
        };
        let Some(directory) = downloads else {
            return failure(session_id, "Не удалось найти папку загрузок");
        };
        let truncated = reply["truncated"] == true;
        reply = match tokio::task::spawn_blocking(move || save_export(&directory, &text)).await {
            Ok(Ok(path)) => json!({"ok": true, "path": path, "truncated": truncated}),
            Ok(Err(error)) => failure(session_id, error),
            Err(error) => failure(session_id, format!("Не удалось сохранить вывод: {error}")),
        };
    }
    let mut reply = with_connection(reply, &session, &route);
    if action == "poll" && terminal_session(d, session_id).is_none() {
        reply["connection"]["canInput"] = json!(false);
    }
    reply
}

fn save_export(directory: &Path, text: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory).map_err(|e| format!("Папка загрузок: {e}"))?;
    for _ in 0..8 {
        let sequence = STREAM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let filename = format!(
            "jarvis-terminal-{}-{}-{sequence}.txt",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );
        let path = directory.join(filename);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Не удалось сохранить вывод: {error}")),
        };
        if let Err(error) = file
            .write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = std::fs::remove_file(&path);
            return Err(format!("Не удалось сохранить вывод: {error}"));
        }
        return Ok(path);
    }
    Err("Не удалось создать файл вывода".into())
}

fn pane_id(session: &Session) -> Option<&str> {
    if session.control_mode.as_deref() == Some("external") {
        return None;
    }
    session.tmux_pane.as_deref().filter(|pane| {
        pane.strip_prefix('%')
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn connection(session: &Session, remote: Option<&RemoteStatus>) -> Value {
    let error = match (&session.remote, remote) {
        (Some(name), None) => format!("Узел «{name}» не подключён"),
        (_, Some(node)) => node.error.clone(),
        _ => String::new(),
    };
    json!({
        "kind": if session.remote.is_some() { "remote" } else { "local" },
        "name": session.remote.as_deref().unwrap_or("Этот компьютер"),
        "transport": if session.remote.is_some() { "ssh" } else { "tmux" },
        "connected": if session.remote.is_some() { remote.is_some_and(|node| node.connected) } else { true },
        "outdated": remote.is_some_and(|node| node.outdated),
        "pane": pane_id(session),
        "canRead": false,
        "canInput": false,
        "error": error,
    })
}

fn bounded_screen(text: String) -> (String, bool) {
    if text.len() <= MAX_SCREEN_BYTES {
        return (text, false);
    }
    let mut start = text.len() - MAX_SCREEN_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

fn unavailable(session: &Session, connection: Value, error: String) -> Value {
    let mut result =
        json!({ "ok": false, "sessionId": session.id, "error": error, "connection": connection });
    if pane_id(session).is_none() {
        let agent = crate::backend::Agent::from_opt(session.agent.as_deref());
        result["needsTmux"] = json!(true);
        result["resumeCmd"] = json!(crate::backend::backend(agent).resume_cmd(session.agent_id()));
        if let Some(name) = &session.remote {
            result["onNode"] = json!(name);
        }
    }
    result
}

pub async fn snapshot(d: &Arc<Daemon>, session_id: &str) -> Value {
    let Some(session) = terminal_session(d, session_id) else {
        return json!({ "ok": false, "sessionId": session_id, "error": "Сессия не найдена" });
    };
    let remote = session
        .remote
        .as_deref()
        .and_then(|name| d.remotes.node(name))
        .map(|node| node.status());
    let mut connection = connection(&session, remote.as_ref());
    let Some(pane) = pane_id(&session) else {
        return unavailable(
            &session,
            connection,
            "Для терминала в чате запусти агента через Jarvis в tmux".into(),
        );
    };
    let target = match d.pane_target(&session) {
        Ok(target) => target,
        Err(error) => return unavailable(&session, connection, error),
    };
    match target.screen(pane).await {
        Ok(screen) => {
            // A successful screen read is fresher than the event poller's
            // cached connection flag (which can lag while reconnecting).
            connection["connected"] = json!(true);
            connection["canRead"] = json!(true);
            connection["canInput"] = json!(true);
            connection["error"] = json!("");
            let (text, truncated) = bounded_screen(screen);
            json!({ "ok": true, "sessionId": session_id, "text": text,
                "capturedAt": crate::util::now_ms(), "truncated": truncated, "connection": connection })
        }
        Err(error) => {
            connection["error"] = json!(error);
            unavailable(&session, connection, error)
        }
    }
}

pub async fn send_key(d: &Arc<Daemon>, session_id: &str, key: &str) -> Value {
    let Some(parsed) = TerminalKey::parse(key) else {
        return json!({ "ok": false, "sessionId": session_id, "error": "Допустимы только Enter, Escape и Ctrl-C" });
    };
    let Some(session) = terminal_session(d, session_id) else {
        return json!({ "ok": false, "sessionId": session_id, "error": "Сессия не найдена" });
    };
    let Some(pane) = pane_id(&session) else {
        return unavailable(
            &session,
            connection(&session, None),
            "Сессия не подключена к tmux".into(),
        );
    };
    let target = match d.pane_target(&session) {
        Ok(target) => target,
        Err(error) => return json!({ "ok": false, "sessionId": session_id, "error": error }),
    };
    match target.terminal_key(pane, parsed).await {
        Ok(()) => json!({ "ok": true, "sessionId": session_id, "key": key }),
        Err(error) => json!({ "ok": false, "sessionId": session_id, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_session() -> Session {
        let mut session = Session::new("session-a".into(), 42);
        session.tmux_pane = Some("%4".into());
        session.pid = Some(123);
        session.agent = Some("codex".into());
        session
    }

    fn binding(session: &Session, route: StreamRoute) -> StreamBinding {
        StreamBinding {
            owner: SessionIdentity::of(session).unwrap(),
            snapshot: session.clone(),
            route,
            backend_id: "backend-handle".into(),
            touched: Instant::now(),
        }
    }

    #[test]
    fn stream_payload_cannot_override_the_session_route_or_export_path() {
        for action in [
            "open", "poll", "input", "resize", "close", "history", "export",
        ] {
            let base = match action {
                "open" => json!({}),
                "poll" => json!({"streamId":"desktop-1", "cursor":0}),
                "input" => json!({"streamId":"desktop-1", "data":[65]}),
                "resize" => json!({"streamId":"desktop-1", "cols":80, "rows":24}),
                _ => json!({"streamId":"desktop-1"}),
            };
            assert!(checked_payload(action, &base).is_ok(), "valid {action}");
            for field in [
                "pane",
                "sessionId",
                "remote",
                "machine",
                "path",
                "backendId",
            ] {
                let mut payload = base.clone();
                payload[field] = json!("attacker-target");
                assert!(
                    checked_payload(action, &payload).is_err(),
                    "{action}: {field}"
                );
            }
        }
        assert!(checked_payload("open", &json!({})).is_ok());
        for lines in [2000, 10000, 100000] {
            assert!(checked_payload("open", &json!({"historyLines":lines})).is_ok());
        }
        for lines in [
            json!(null),
            json!(-1),
            json!(1999),
            json!(100001),
            json!(2000.5),
            json!("100000"),
        ] {
            assert!(checked_payload("open", &json!({"historyLines":lines})).is_err());
        }
        assert!(checked_payload("kill", &json!({})).is_err());
        assert!(checked_payload("../control", &json!({})).is_err());
        assert!(checked_payload("export", &json!({"streamId":"desktop-1"})).is_ok());
    }

    #[test]
    fn stream_input_preserves_bytes_and_rejects_non_bytes_and_oversized_writes() {
        let data = "Привет\u{1b}[A".as_bytes();
        let payload = json!({"streamId":"desktop-1", "data": data});
        assert_eq!(checked_payload("input", &payload).unwrap(), payload);
        for data in [
            json!([-1]),
            json!([256]),
            json!([1.5]),
            json!(["65"]),
            json!([]),
            json!("hello"),
        ] {
            assert!(
                checked_payload("input", &json!({"streamId":"desktop-1", "data":data})).is_err()
            );
        }
        assert!(checked_payload(
            "input",
            &json!({"streamId":"desktop-1", "data":vec![0; MAX_INPUT_BYTES + 1]})
        )
        .is_err());
        let paste =
            json!({"streamId":"desktop-1", "data":vec![65; MAX_INPUT_BYTES + 1], "paste":true});
        assert_eq!(checked_payload("input", &paste).unwrap(), paste);
        assert!(checked_payload(
            "input",
            &json!({"streamId":"desktop-1", "data":vec![65; MAX_PASTE_BYTES + 1], "paste":true})
        )
        .is_err());
        for invalid in [json!(null), json!("true"), json!(1)] {
            assert!(checked_payload(
                "input",
                &json!({"streamId":"desktop-1", "data":[65], "paste":invalid})
            )
            .is_err());
        }
        for data in [json!([0]), json!([255]), json!([208])] {
            assert!(checked_payload(
                "input",
                &json!({"streamId":"desktop-1", "data":data, "paste":true})
            )
            .is_err());
        }
        assert!(checked_payload("poll", &json!({"streamId":"desktop-1", "cursor":-1})).is_err());
        assert!(checked_payload(
            "resize",
            &json!({"streamId":"desktop-1", "cols":500,"rows":300})
        )
        .is_ok());
        for (cols, rows) in [(0, 20), (19, 20), (501, 20), (80, 0), (80, 301)] {
            assert!(checked_payload(
                "resize",
                &json!({"streamId":"desktop-1", "cols":cols,"rows":rows})
            )
            .is_err());
        }
    }

    #[test]
    fn binding_rejects_foreign_sessions_and_rebound_pane_or_provider_identity() {
        let session = live_session();
        let binding = binding(&session, StreamRoute::Local);
        let mut progress = session.clone();
        progress.lifecycle_revision += 1;
        progress.updated_at += 1;
        progress.detail = "working".into();
        progress.pid = Some(456);
        assert!(check_owner(&binding, &progress, &StreamRoute::Local).is_ok());
        let mut variants = Vec::new();
        let mut next = session.clone();
        next.id = "session-b".into();
        variants.push(next);
        let mut next = session.clone();
        next.tmux_pane = Some("%5".into());
        variants.push(next);
        let mut next = session.clone();
        next.created_at += 1;
        variants.push(next);
        let mut next = session.clone();
        next.remote = Some("vm".into());
        variants.push(next);
        let mut next = session.clone();
        next.instance_id = Some("other-account".into());
        variants.push(next);
        let mut next = session.clone();
        next.provider_home = Some("/other/.codex".into());
        variants.push(next);
        let mut next = session.clone();
        next.provider_session_id = Some("other-conversation".into());
        variants.push(next);
        let mut next = session.clone();
        next.control_mode = Some("external".into());
        variants.push(next);
        for next in variants {
            assert!(check_owner(&binding, &next, &StreamRoute::Local).is_err());
        }
    }

    #[test]
    fn missing_remote_never_resolves_to_local_and_recreated_node_invalidates_binding() {
        assert!(matches!(
            resolve_remote(None, |_| panic!("local must not look up a remote")).unwrap(),
            StreamRoute::Local
        ));
        for name in ["vm", "", "local"] {
            assert!(resolve_remote(Some(name), |_| None).is_err());
        }
        let cfg = crate::remote::RemoteCfg {
            name: "terminal-routing-test".into(),
            ssh_host: "example.invalid".into(),
            ..Default::default()
        };
        let old = Arc::new(crate::remote::Node::new(cfg.clone()));
        let replaced = Arc::new(crate::remote::Node::new(cfg));
        let mut session = live_session();
        session.remote = Some("terminal-routing-test".into());
        let binding = binding(&session, StreamRoute::Remote(old.clone()));
        assert!(check_owner(&binding, &session, &StreamRoute::Remote(old)).is_ok());
        assert!(check_owner(&binding, &session, &StreamRoute::Remote(replaced)).is_err());
        assert!(check_owner(&binding, &session, &StreamRoute::Local).is_err());
    }

    #[test]
    fn terminal_poll_drains_a_vanished_session_but_never_a_rebound_identity() {
        let session = live_session();
        let local = binding(&session, StreamRoute::Local);
        assert!(check_current_owner(&local, None, &StreamRoute::Local, true).is_ok());
        assert!(check_current_owner(&local, None, &StreamRoute::Local, false).is_err());
        let mut rebound = session.clone();
        rebound.tmux_pane = Some("%88".into());
        assert!(check_current_owner(&local, Some(&rebound), &StreamRoute::Local, true).is_err());

        let cfg = crate::remote::RemoteCfg {
            name: "drain-test".into(),
            ssh_host: "example.invalid".into(),
            ..Default::default()
        };
        let old = Arc::new(crate::remote::Node::new(cfg.clone()));
        let replacement = Arc::new(crate::remote::Node::new(cfg));
        let mut remote_session = session;
        remote_session.remote = Some("drain-test".into());
        let remote = binding(&remote_session, StreamRoute::Remote(old.clone()));
        assert!(check_current_owner(&remote, None, &StreamRoute::Remote(old), true).is_ok());
        assert!(
            check_current_owner(&remote, None, &StreamRoute::Remote(replacement), true).is_err()
        );
        assert!(check_current_owner(&remote, None, &StreamRoute::Local, true).is_err());
    }

    #[test]
    fn terminal_export_is_unique_private_and_preserves_complete_unicode_text() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-terminal-export-test-{}-{}",
            std::process::id(),
            STREAM_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let text = "Привет 👋\nlong line\n".repeat(1000);
        let first = save_export(&dir, &text).unwrap();
        let second = save_export(&dir, "second").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), text);
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
        assert_eq!(first.parent(), Some(dir.as_path()));
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("jarvis-terminal-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(save_export(&first, "must not overwrite").is_err());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_does_not_guess_a_target_without_an_exact_pane() {
        let mut session = Session::new("s".into(), 0);
        for pane in ["", "project:0", "0", "%", "-a", "%1; kill-server", "%1\n"] {
            session.tmux_pane = Some(pane.into());
            assert!(pane_id(&session).is_none(), "{pane}");
        }
        session.tmux_pane = Some("%123".into());
        assert_eq!(pane_id(&session), Some("%123"));
    }

    #[test]
    fn external_session_rejects_stale_pane_metadata() {
        let mut session = Session::new("external".into(), 0);
        session.tmux_pane = Some("%1".into());
        session.control_mode = Some("external".into());
        assert!(pane_id(&session).is_none());
        session.remote = Some("vm".into());
        assert!(pane_id(&session).is_none());
    }

    #[test]
    fn removed_remote_is_offline_and_never_becomes_local() {
        let mut session = Session::new("vps:s".into(), 0);
        session.remote = Some("vps".into());
        session.tmux_pane = Some("%1".into());
        let status = connection(&session, None);
        assert_eq!(status["kind"], "remote");
        assert_eq!(status["connected"], false);
        assert_eq!(status["canInput"], false);
        assert_eq!(status["transport"], "ssh");
    }

    #[test]
    fn terminal_errors_preserve_remote_resume_identity() {
        let mut session = Session::new("vps:conversation-1".into(), 0);
        session.remote = Some("vps".into());
        session.agent = Some("codex".into());
        let error = unavailable(&session, connection(&session, None), "Нет паны".into());
        assert_eq!(error["needsTmux"], true);
        assert_eq!(error["onNode"], "vps");
        assert_eq!(error["resumeCmd"], "codex resume conversation-1");
    }

    #[test]
    fn terminal_snapshot_is_bounded_without_corrupting_utf8() {
        let screen = "я".repeat(MAX_SCREEN_BYTES) + "👋 ready";
        let (text, truncated) = bounded_screen(screen.clone());
        assert!(truncated);
        assert!(text.len() <= MAX_SCREEN_BYTES);
        assert!(screen.ends_with(&text));
        assert!(text.ends_with("👋 ready"));
        assert_eq!(bounded_screen("".into()), (String::new(), false));
    }
}
