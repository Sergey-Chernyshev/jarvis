//! Unix-socket HTTP-сервер демона: ~/.jarvis/run.sock.
//!
//! Сюда jarvis-hook кидает события из хуков Claude Code (curl за 0.3с).
//! Контракт: POST <любой путь> — событие; GET /state — самодиагностика;
//! GET <прочее> — "jarvis ok". Сокет 0600 — события только от владельца.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use crate::capability::{self, confirm::AutoDeny, grant::Consumer};
use crate::daemon::Daemon;
use crate::util::sock_path;

pub async fn serve(d: Arc<Daemon>) {
    let sock = sock_path();
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&sock);

    let listener = match tokio::net::UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("[jarvis] не смог открыть сокет {}: {err}", sock.display());
            return;
        }
    };
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    println!("[jarvis] слушаю {}", sock.display());

    let app = Router::new()
        .route("/state", get(get_state))
        // капабилити (инкр. 8): мост для MCP-сервера/внешних потребителей.
        .route("/capabilities", get(get_capabilities))
        .route("/capability", post(handle_capability))
        .fallback(fallback)
        // защита от мусора, но с запасом: диффы Edit бывают жирными
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(d);

    if let Err(err) = axum::serve(listener, app).await {
        eprintln!("[jarvis] server error: {err}");
    }
}

/// GET /state — что сейчас в реестре (для curl-диагностики).
async fn get_state(State(d): State<Arc<Daemon>>) -> Response {
    let body = serde_json::to_string_pretty(&d.snapshot()).unwrap_or_else(|_| "[]".into()) + "\n";
    ([("content-type", "application/json")], body).into_response()
}

async fn fallback(State(d): State<Arc<Daemon>>, req: Request) -> Response {
    match *req.method() {
        Method::GET => "jarvis ok\n".into_response(),
        Method::POST => {
            let Ok(body) = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            handle_event(&d, body)
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// GET /capabilities — список инструментов агента (проекция реестра в MCP tool
/// defs, отфильтрованная грантом агента). MCP-сервер форвардит это в tools/list.
async fn get_capabilities(State(d): State<Arc<Daemon>>) -> Response {
    let tools = d.caps.tools_json(&Consumer::agent().grant);
    let body = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".into());
    ([("content-type", "application/json")], body).into_response()
}

/// POST /capability — вызов капабилити через гейт. Тело: {consumer, id, args}.
/// Это межпроцессная проекция слоя истины (§5): MCP-сервер агента ходит сюда,
/// гейт (грант/провенанс/аудит) — в демоне, обойти его нельзя.
///
/// TODO(фаза 5): подтверждение side-effect требует UI-карточки в панели. Пока
/// её нет — confirmer = AutoDeny: read работает, control/settings безопасно
/// отклоняются (а не висят/исполняются молча). PanelConfirmer заменит здесь.
async fn handle_capability(State(d): State<Arc<Daemon>>, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "bad json").into_response();
    };
    let id = req.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
    let consumer = match req.get("consumer").and_then(|v| v.as_str()) {
        Some("panel") => Consumer::panel(),
        _ => Consumer::agent(),
    };

    let result = capability::invoke(
        &d.caps,
        d.clone(),
        &consumer,
        &id,
        args,
        &AutoDeny,
        &capability::audit::FileAudit,
        capability::GateConfig::default(),
    )
    .await;

    let out = match result {
        Ok(o) => json!({ "ok": true, "value": o.value, "provenance": o.provenance.as_str() }),
        Err(e) => json!({ "ok": false, "error": e.to_string(), "code": e.code() }),
    };
    let body = serde_json::to_string(&out).unwrap_or_else(|_| "{\"ok\":false}".into());
    ([("content-type", "application/json")], body).into_response()
}

fn handle_event(d: &Arc<Daemon>, body: Bytes) -> Response {
    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(evt) => {
            d.reduce(&evt);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
