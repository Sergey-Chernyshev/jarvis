//! Протокол узла: минимальный HTTP поверх unix-сокета, тем же axum, что и у
//! демона (дизайн 2026-08-05, §«Протокол узла»).
//!
//! | метод | зачем |
//! | --- | --- |
//! | `GET /hello` | версия, хост, uptime, состояние буфера — проверка связи |
//! | `GET /events?since=` | события с курсора, long-poll до 25с |
//! | `GET /file?path=&from=` | кусок транскрипта (только из `~/.claude`/`~/.codex`) |
//! | `POST /reply` | `{pane, text}` → вставка в tmux |
//! | `POST /control` | `{pane, cmd}` → слэш-команда в пану |
//! | `POST /keys` | `{pane, keys}` → план клавиш в пикер вопроса |
//! | `GET /projects` | оглавление проектов машины (каталоги, сессии, время) |
//! | `POST /launch` | `{cwd, cmd}` → создать каталог и поднять сессию в tmux |
//! | `GET /screen?pane=` | видимый экран паны — «что там на самом деле» |
//! | `GET /panes` | живые паны `tmux -L jarvis` |
//! | `POST <прочее>` | конверт от jarvis-hook |
//!
//! Аутентификации здесь нет и быть не должно: сокет 0600, наружу узел не
//! слушает ничего, а через SSH-туннель приходит уже доверенный владелец.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::ring::{Recorded, Slice};
use super::{files, projects, tmux, Node};

/// Потолок long-poll. 25с, а не «до последнего»: SSH-туннель и NAT рвут
/// молчащее соединение без предупреждения, и лучше отдать пустой ответ, чем
/// узнать о разрыве на первом же важном событии.
const POLL_WINDOW: Duration = Duration::from_secs(25);

/// Тот же лимит тела, что у демона: диффы Edit в конвертах бывают жирными.
const MAX_BODY: usize = 4 * 1024 * 1024;

pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/hello", get(hello))
        .route("/events", get(events))
        .route("/file", get(file))
        .route("/panes", get(panes))
        .route("/reply", post(reply))
        .route("/control", post(control))
        .route("/keys", post(keys))
        .route("/projects", get(projects))
        .route("/launch", post(launch))
        .route("/screen", get(screen))
        // POST на любой прочий путь — конверт от хука. jarvis-hook бьёт в
        // /event, но привязываться к одному пути не за что: у демона ровно так же.
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(node)
}

/// GET /hello — узел жив, вот кто он и что у него в буфере.
async fn hello(State(node): State<Arc<Node>>) -> Response {
    let s = node.stats();
    json_ok(&json!({
        "node": "jarvis-node",
        "version": env!("CARGO_PKG_VERSION"),
        "host": node.host(),
        "uptime_ms": node.uptime_ms(),
        "cursor": s.cursor,
        "buffered": s.buffered,
        "oldest": s.oldest,
        "capacity": s.capacity,
    }))
}

/// GET /events?since=N — события с курсора; ждём до 25с, если ничего нет.
async fn events(State(node): State<Arc<Node>>, req: Request) -> Response {
    let q = params(req.uri().query());
    let since = q.get("since").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let deadline = tokio::time::Instant::now() + POLL_WINDOW;
    // Подписываемся ДО первого чтения буфера: иначе событие, пришедшее в зазор
    // между чтением и ожиданием, пролежало бы у нас все 25 секунд.
    let mut bell = node.subscribe();
    loop {
        match node.slice(since) {
            // честная дырка: ноут перечитает транскрипты целиком
            Slice::Gap { cursor } => return json_ok(&json!({ "gap": true, "cursor": cursor })),
            Slice::Events { cursor, events } => {
                if !events.is_empty() {
                    let events: Vec<Value> = events.iter().map(Recorded::to_json).collect();
                    return json_ok(&json!({ "cursor": cursor, "events": events }));
                }
                match tokio::time::timeout_at(deadline, bell.changed()).await {
                    Ok(Ok(())) => {} // звонок — перечитываем буфер
                    // окно вышло (или звонок сломался) — отдаём пустой ответ с
                    // тем же курсором, ноут тут же придёт снова
                    _ => return json_ok(&json!({ "cursor": cursor, "events": [] })),
                }
            }
        }
    }
}

/// GET /file?path=P&from=OFF — кусок транскрипта. `next` в ответе — смещение
/// для следующего запроса; `from` меньше запрошенного означает, что файл
/// переписали и читать надо заново.
async fn file(State(node): State<Arc<Node>>, req: Request) -> Response {
    let q = params(req.uri().query());
    let path = q.get("path").map(String::as_str).unwrap_or("");
    let from = q.get("from").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let real = match files::resolve(path, node.roots()) {
        Ok(p) => p,
        // 404 — «свежая сессия, транскрипта ещё нет», это ожидание, а не отказ
        Err(files::Denial::Missing) => {
            return json_err(StatusCode::NOT_FOUND, "транскрипта ещё нет")
        }
        Err(files::Denial::Outside) => {
            return json_err(StatusCode::FORBIDDEN, "путь вне корней транскриптов")
        }
    };
    match files::read_chunk(&real, from) {
        Ok(c) => json_ok(&json!({
            "path": real.to_string_lossy(),
            "from": c.from,
            "next": c.next,
            "size": c.size,
            "eof": c.next >= c.size,
            "data": c.data,
        })),
        Err(msg) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &msg),
    }
}

/// GET /panes — живые паны сервера `-L jarvis`.
async fn panes() -> Response {
    match tmux::list_panes().await {
        Ok(panes) => {
            let panes: Vec<Value> = panes.iter().map(tmux::Pane::to_json).collect();
            json_ok(&json!({ "panes": panes }))
        }
        // tmux не установлен или сервер не поднят — это состояние машины, а не
        // поломка узла: отдаём пустой список и причину, ноут решит сам
        Err(msg) => json_ok(&json!({ "panes": [], "error": msg })),
    }
}

/// POST /reply — {pane, text}.
async fn reply(body: Bytes) -> Response {
    let Some((pane, text)) = pane_and(&body, "text") else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю {pane, text}");
    };
    tmux_result(tmux::reply(&pane, &text).await)
}

/// POST /control — {pane, cmd}: слэш-команда пульта (модель/effort).
async fn control(body: Bytes) -> Response {
    let Some((pane, cmd)) = pane_and(&body, "cmd") else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю {pane, cmd}");
    };
    tmux_result(tmux::slash(&pane, &cmd).await)
}

/// POST /keys — {pane, keys}: ответ на вопрос агента. План клавиш считает ноут,
/// узел его только проигрывает (см. `tmux::Key`).
async fn keys(body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю {pane, keys}");
    };
    let pane = v.get("pane").and_then(Value::as_str).unwrap_or_default().trim();
    let Some(plan) = v.get("keys").and_then(tmux::parse_keys) else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю keys:[{key|text}]");
    };
    if pane.is_empty() || plan.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "пустая пана или пустой план");
    }
    tmux_result(tmux::play_keys(pane, &plan).await)
}

/// GET /projects — где на этой машине работали. Только оглавление: ноут сам
/// решит, что показать и что из этого прочитать через `/file`.
async fn projects() -> Response {
    let home = super::home_dir();
    json_ok(&json!({ "projects": projects::list(&home) }))
}

/// POST /launch — {cwd, cmd}: поднять сессию агента в `tmux -L jarvis`.
///
/// Каталог создаётся рекурсивно: человек заводит проект там, где его ещё нет,
/// и требовать от него сначала сходить туда по ssh — значит не сделать работу.
/// Команду собирает ноут (агент, флаги, прокси — его настройки), узел только
/// исполняет: та же граница, что у `/keys`.
async fn launch(body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю {cwd, cmd}");
    };
    let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or_default().trim();
    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or_default().trim();
    if !cwd.starts_with('/') || cmd.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "нужен абсолютный cwd и непустая команда");
    }
    match tmux::launch(cwd, cmd, v.get("name").and_then(Value::as_str)).await {
        // Пану возвращаем сразу: сессия агента ещё не зарегистрирована, и это
        // единственная ниточка, по которой запустивший может увидеть, что там
        // происходит, и ответить на первый вопрос.
        Ok((session, pane)) => json_ok(&json!({ "ok": true, "session": session, "pane": pane })),
        Err(msg) => json_err(StatusCode::BAD_GATEWAY, &msg),
    }
}

/// GET /screen?pane=%N — что видно в пане прямо сейчас.
async fn screen(req: Request) -> Response {
    let pane = params(req.uri().query())
        .get("pane")
        .cloned()
        .unwrap_or_default();
    if pane.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "нужен pane");
    }
    match tmux::screen(&pane).await {
        Ok(text) => json_ok(&json!({ "pane": pane, "screen": text })),
        Err(msg) => json_ok(&json!({ "pane": pane, "screen": "", "error": msg })),
    }
}

/// POST <прочее> — конверт от jarvis-hook; GET <прочее> — признак жизни.
async fn fallback(State(node): State<Arc<Node>>, req: Request) -> Response {
    match *req.method() {
        Method::GET => "jarvis-node ok\n".into_response(),
        Method::POST => {
            let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_BODY).await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            match serde_json::from_slice::<Value>(&body) {
                // конверт кладём как есть: интерпретация — дело ноута, узел не
                // знает ни статусов, ни ходов (дизайн, «Чего узел НЕ делает»)
                Ok(envelope) => {
                    node.push(envelope);
                    // 204, как у демона: хук всё равно не читает ответ
                    StatusCode::NO_CONTENT.into_response()
                }
                Err(_) => StatusCode::BAD_REQUEST.into_response(),
            }
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// tmux не ответил — это 502: узел работает, не работает то, к чему он ходил.
fn tmux_result(res: Result<(), String>) -> Response {
    match res {
        Ok(()) => json_ok(&json!({ "ok": true })),
        Err(msg) => json_err(StatusCode::BAD_GATEWAY, &msg),
    }
}

/// `{pane, <field>}` из тела. Пустые значения отбраковываем здесь: пустая пана
/// для tmux означает «активная», а угадывать, куда писать на чужой машине,
/// узел не вправе.
fn pane_and(body: &[u8], field: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let pane = v.get("pane")?.as_str()?.trim().to_string();
    let text = v.get(field)?.as_str()?.to_string();
    if pane.is_empty() || text.is_empty() {
        return None;
    }
    Some((pane, text))
}

/// Разбор query-строки. Свой, а не `axum::extract::Query`: узлу нужны две
/// строки и число, а percent-decode всё равно пришлось бы описывать — путь
/// транскрипта приезжает закодированным.
fn params(query: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.unwrap_or("").split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

/// %XX и `+` → байты. Собираем именно байты, а не символы: UTF-8 в пути
/// кодируется по байту, и посимвольный разбор ломал бы кириллицу в именах
/// проектов.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    // «%» не начало escape-последовательности — значит, это «%»
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_ok(v: &Value) -> Response {
    let body = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    ([("content-type", "application/json")], body).into_response()
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    let body = serde_json::to_string(&json!({ "error": msg })).unwrap_or_else(|_| "{}".into());
    (code, [("content-type", "application/json")], body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_decode_path_and_offset() {
        let q = params(Some("path=%2Fhome%2Fme%2F.claude%2Fa.jsonl&from=1024"));
        assert_eq!(q.get("path").unwrap(), "/home/me/.claude/a.jsonl");
        assert_eq!(q.get("from").unwrap(), "1024");
        assert!(params(None).is_empty());
        assert!(params(Some("")).is_empty());
    }

    // Кириллица в имени проекта — обычное дело: декодируем побайтово.
    #[test]
    fn percent_decode_keeps_utf8_and_spaces() {
        assert_eq!(percent_decode("%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82"), "проект");
        assert_eq!(percent_decode("my+project"), "my project");
        assert_eq!(percent_decode("100%"), "100%", "хвостовой %% не escape");
        assert_eq!(percent_decode("a%zz"), "a%zz", "битый escape отдаём как есть");
    }

    // Ключ без значения не должен ронять разбор (и не должен подставлять мусор).
    #[test]
    fn params_tolerate_flags_without_value() {
        let q = params(Some("since=&junk&from=7"));
        assert_eq!(q.get("since").unwrap(), "");
        assert_eq!(q.get("junk").unwrap(), "");
        assert_eq!(q.get("from").unwrap(), "7");
    }

    #[test]
    fn pane_and_requires_both_fields_nonempty() {
        // байтовый литерал не держит кириллицу — берём обычную строку
        let ok = r#"{"pane":"%3","text":"привет"}"#;
        assert_eq!(
            pane_and(ok.as_bytes(), "text"),
            Some(("%3".to_string(), "привет".to_string()))
        );
        assert_eq!(pane_and(br#"{"pane":"  ","text":"x"}"#, "text"), None);
        assert_eq!(pane_and(br#"{"pane":"%3","text":""}"#, "text"), None);
        assert_eq!(pane_and(br#"{"pane":"%3"}"#, "text"), None);
        assert_eq!(pane_and(b"not json", "text"), None);
    }
}
