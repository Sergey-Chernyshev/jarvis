//! Unix-socket HTTP-сервер демона: ~/.jarvis/run.sock.
//!
//! Сюда jarvis-hook кидает события из хуков Claude Code (curl за 0.3с).
//! Контракт: POST <любой путь> — событие; GET /state — самодиагностика;
//! GET <прочее> — "jarvis ok". Сокет 0600 — события только от владельца.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use crate::capability::{self, grant::Consumer, tokens::TokenStore};
use crate::daemon::Daemon;
use crate::plugins::protocol::{EventsQuery, RegisterRequest};
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
        .route("/plugin/register", post(plugin_register))
        .route("/plugin/events", get(plugin_events))
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

/// Идентичность сокет-потребителя ТОЛЬКО по токену. panel недостижим извне.
fn consumer_for(store: &TokenStore, token: Option<&str>) -> Option<Consumer> {
    store.resolve(token?)
}

fn plugin_id_for_token(store: &TokenStore, token: Option<&str>) -> Option<String> {
    let consumer = consumer_for(store, token)?;
    consumer
        .id
        .strip_prefix("plugin:")
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

enum RegisterRouteError {
    Unauthorized,
    BadJson,
    Host(crate::plugins::HostRegistrationError),
}

fn register_error_parts(error: &RegisterRouteError) -> (StatusCode, &'static str) {
    match error {
        RegisterRouteError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
        RegisterRouteError::BadJson => (StatusCode::BAD_REQUEST, "bad_json"),
        RegisterRouteError::Host(crate::plugins::HostRegistrationError::NotFound) => {
            (StatusCode::UNAUTHORIZED, "unauthorized")
        }
        RegisterRouteError::Host(error) => {
            let status = match error {
                crate::plugins::HostRegistrationError::Runtime(
                    crate::plugins::supervisor::RegistrationError::Conflict(_),
                ) => StatusCode::CONFLICT,
                crate::plugins::HostRegistrationError::Runtime(
                    crate::plugins::supervisor::RegistrationError::Incompatible { .. },
                ) => StatusCode::UPGRADE_REQUIRED,
                crate::plugins::HostRegistrationError::NotFound => unreachable!(),
            };
            (status, error.code())
        }
    }
}

fn json_response(status: StatusCode, value: Value) -> Response {
    let body = serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".into());
    (status, [("content-type", "application/json")], body).into_response()
}

fn register_error_response(error: RegisterRouteError) -> Response {
    let (status, code) = register_error_parts(&error);
    let message = match &error {
        RegisterRouteError::Unauthorized
        | RegisterRouteError::Host(crate::plugins::HostRegistrationError::NotFound) => {
            "нет/неизвестен plugin token".to_string()
        }
        RegisterRouteError::BadJson => "некорректный JSON".to_string(),
        RegisterRouteError::Host(error) => error.to_string(),
    };
    json_response(
        status,
        json!({ "ok": false, "error": message, "code": code }),
    )
}

async fn plugin_register(
    State(d): State<Arc<Daemon>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers.get("x-jarvis-token").and_then(|v| v.to_str().ok());
    let Some(plugin_id) = plugin_id_for_token(&d.tokens, token) else {
        return register_error_response(RegisterRouteError::Unauthorized);
    };
    let request = match serde_json::from_slice::<RegisterRequest>(&body) {
        Ok(request) => request,
        Err(_) => return register_error_response(RegisterRouteError::BadJson),
    };
    if let Err(error) = d
        .plugins
        .register(&plugin_id, &request, crate::util::now_ms())
    {
        return register_error_response(RegisterRouteError::Host(error));
    }
    crate::plugins::emit_statuses(&d);
    json_response(StatusCode::OK, json!({ "ok": true }))
}

async fn plugin_events(
    State(d): State<Arc<Daemon>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    let token = headers.get("x-jarvis-token").and_then(|v| v.to_str().ok());
    let Some(plugin_id) = plugin_id_for_token(&d.tokens, token) else {
        return register_error_response(RegisterRouteError::Unauthorized);
    };
    let (after, limit, wait_ms) = query.clamped();
    match d
        .plugins
        .poll_events(&plugin_id, after, limit, wait_ms)
        .await
    {
        Ok(events) => {
            let next_seq = events.last().map(|event| event.seq).unwrap_or(after);
            json_response(
                StatusCode::OK,
                json!({ "ok": true, "events": events, "nextSeq": next_seq }),
            )
        }
        Err(_) => register_error_response(RegisterRouteError::Unauthorized),
    }
}

/// POST /capability — вызов капабилити через гейт. Тело: {id, args}.
/// Это межпроцессная проекция слоя истины (§5): MCP-сервер агента ходит сюда,
/// гейт (грант/провенанс/аудит) — в демоне, обойти его нельзя.
/// Идентичность потребителя — ТОЛЬКО по токену из заголовка x-jarvis-token (INV-PANEL).
async fn handle_capability(
    State(d): State<Arc<Daemon>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers.get("x-jarvis-token").and_then(|v| v.to_str().ok());
    let Some(consumer) = consumer_for(&d.tokens, token) else {
        return (StatusCode::UNAUTHORIZED, "{\"ok\":false,\"error\":\"нет/неизвестен токен\",\"code\":\"unauthorized\"}").into_response();
    };

    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "bad json").into_response();
    };
    let id = req.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));

    let confirmer = crate::capability::confirm_panel::PanelConfirmer {
        app: d.app.clone(),
        pending: d.pending.clone(),
        daemon: d.clone(),
    };

    let result = capability::invoke(
        &d.caps,
        d.clone(),
        &consumer,
        &id,
        args,
        &confirmer,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_token_store(tag: &str) -> TokenStore {
        TokenStore::at(std::env::temp_dir().join(format!(
            "jarvis-server-{tag}-{}-tokens.json",
            std::process::id()
        )))
    }

    #[test]
    fn missing_or_unknown_token_has_no_consumer() {
        let store = crate::capability::tokens::TokenStore::at(
            std::env::temp_dir().join(format!("jarvis-srv-{}.json", std::process::id())),
        );
        let agent = store.ensure_agent_token();
        assert!(consumer_for(&store, None).is_none(), "нет токена → нет потребителя");
        assert!(consumer_for(&store, Some("bogus")).is_none());
        // INV-PANEL: валидный agent-токен даёт agent, НИКОГДА не panel
        assert_eq!(consumer_for(&store, Some(&agent)).unwrap().id, "agent");
    }

    #[test]
    fn plugin_route_rejects_agent_token() {
        let store = test_token_store("reject-agent");
        let token = store.ensure_agent_token();

        assert!(plugin_id_for_token(&store, Some(&token)).is_none());
    }

    #[test]
    fn plugin_route_uses_token_identity_not_body_identity() {
        let store = test_token_store("identity");
        let token = store
            .ensure_plugin_token("agent-vm", &[crate::capability::contract::RiskClass::Read])
            .unwrap();
        let body = json!({ "pluginId": "other", "protocolVersion": 1, "pid": 42 });

        let routed = plugin_id_for_token(&store, Some(&token));

        assert_eq!(routed.as_deref(), Some("agent-vm"));
        assert_ne!(routed.as_deref(), body["pluginId"].as_str());
    }

    #[test]
    fn register_error_maps_to_stable_http_status_and_code() {
        assert_eq!(
            register_error_parts(&RegisterRouteError::Unauthorized),
            (StatusCode::UNAUTHORIZED, "unauthorized")
        );
        assert_eq!(
            register_error_parts(&RegisterRouteError::Host(
                crate::plugins::HostRegistrationError::Runtime(
                    crate::plugins::supervisor::RegistrationError::Conflict("wrong pid".into())
                )
            )),
            (StatusCode::CONFLICT, "registration_conflict")
        );
        assert_eq!(
            register_error_parts(&RegisterRouteError::Host(
                crate::plugins::HostRegistrationError::Runtime(
                    crate::plugins::supervisor::RegistrationError::Incompatible { received: 2 }
                )
            )),
            (StatusCode::UPGRADE_REQUIRED, "incompatible_protocol")
        );
    }
}
