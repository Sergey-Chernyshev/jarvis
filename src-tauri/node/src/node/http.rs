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
//! | `GET /agents?pids=` | живые агенты, паны `-L jarvis` и живость спрошенных pid |
//! | `POST /launch` | `{cwd, cmd}` → создать каталог и поднять сессию в tmux |
//! | `GET /screen?pane=` | видимый экран паны — «что там на самом деле» |
//! | `GET /usage` | лимиты аккаунта: текст `claude /usage` как есть |
//! | `GET /panes` | живые паны `tmux -L jarvis` |
//! | `POST <прочее>` | конверт от jarvis-hook |
//!
//! Аутентификации здесь нет и быть не должно: сокет 0600, наружу узел не
//! слушает ничего, а через SSH-туннель приходит уже доверенный владелец.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::ring::{Recorded, Slice};
use super::{agent, files, live, projects, sources, tmux, Node};

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
        .route("/kill", post(kill))
        .route("/projects", get(projects))
        .route("/sources", get(provider_sources))
        .route("/sources/repair", post(repair_source))
        .route("/sessions", get(sessions))
        .route("/agents", get(agents))
        .route("/launch", post(launch))
        .route("/screen", get(screen))
        .route("/terminal/{action}", post(terminal).layer(DefaultBodyLimit::max(5 * 1024 * 1024)))
        .route("/usage", get(usage))
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
        "protocol": 2,
        "capabilities": ["events.instance", "events.timestamp", "files.bounded", "sources", "sources.trustRepair", "sessions", "projects.multiSource", "terminal.reply", "terminal.keys", "terminal.screen", "terminal.kill", "terminal.stream.v1"],
        "sources": sources::discover(&super::home_dir(), &super::jarvis_dir()).iter().map(|source| source.json()).collect::<Vec<_>>(),
        "instance": node.instance(),
        "version": env!("CARGO_PKG_VERSION"),
        "host": node.host(),
        "uptime_ms": node.uptime_ms(),
        "cursor": s.cursor,
        "buffered": s.buffered,
        "oldest": s.oldest,
        "capacity": s.capacity,
    }))
}

/// Control-mode streams stay inside the authenticated Unix-socket transport.
async fn terminal(Path(action): Path<String>, body: Bytes) -> Response {
    let payload = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        _ => return json_err(StatusCode::BAD_REQUEST, "terminal request must be a JSON object"),
    };
    json_ok(&crate::terminal_stream::dispatch(&action, &payload).await)
}

/// GET /events?since=N — события с курсора; ждём до 25с, если ничего нет.
async fn events(State(node): State<Arc<Node>>, req: Request) -> Response {
    let q = params(req.uri().query());
    let since = q.get("since").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    // A restarted node may already have passed the old numerical cursor.
    // Explicit instance identity prevents silently skipping its early events.
    if q.get("instance").is_some_and(|id| !id.is_empty() && id != node.instance()) {
        return json_ok(&json!({ "gap": true, "cursor": 0, "instance": node.instance() }));
    }
    let deadline = tokio::time::Instant::now() + POLL_WINDOW;
    // Подписываемся ДО первого чтения буфера: иначе событие, пришедшее в зазор
    // между чтением и ожиданием, пролежало бы у нас все 25 секунд.
    let mut bell = node.subscribe();
    loop {
        match node.slice(since) {
            // честная дырка: ноут перечитает транскрипты целиком
            Slice::Gap { cursor } => return json_ok(&json!({ "gap": true, "cursor": cursor, "instance": node.instance() })),
            Slice::Events { cursor, events } => {
                if !events.is_empty() {
                    let events: Vec<Value> = events.iter().map(Recorded::to_json).collect();
                    return json_ok(&json!({ "cursor": cursor, "events": events, "instance": node.instance() }));
                }
                match tokio::time::timeout_at(deadline, bell.changed()).await {
                    Ok(Ok(())) => {} // звонок — перечитываем буфер
                    // окно вышло (или звонок сломался) — отдаём пустой ответ с
                    // тем же курсором, ноут тут же придёт снова
                    _ => return json_ok(&json!({ "cursor": cursor, "events": [], "instance": node.instance() })),
                }
            }
        }
    }
}

/// GET /file?path=P&from=OFF — кусок транскрипта. `next` в ответе — смещение
/// для следующего запроса; `from` меньше запрошенного означает, что файл
/// переписали и читать надо заново.
async fn file(req: Request) -> Response {
    let q = params(req.uri().query());
    let path = q.get("path").map(String::as_str).unwrap_or("");
    let from = q.get("from").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    // Корни транскриптов плюс рабочие каталоги ЖИВЫХ пан. Второе нужно, чтобы
    // показать артефакты работы агента — файлы, которые он только что правил.
    //
    // Это по-прежнему проверка на стороне узла, а не доверие клиенту: «каталог,
    // где прямо сейчас работает агент» узел выясняет сам у tmux. Пропадёт
    // пана — пропадёт и доступ.
    let mut roots = files::transcript_roots(&super::home_dir());
    roots.extend(tmux::live_cwds().await);
    let real = match files::resolve(path, &roots) {
        Ok(p) => p,
        // 404 — «свежая сессия, транскрипта ещё нет», это ожидание, а не отказ
        Err(files::Denial::Missing) => {
            return json_err(StatusCode::NOT_FOUND, "файла ещё нет")
        }
        Err(files::Denial::Outside) => {
            return json_err(
                StatusCode::FORBIDDEN,
                "путь вне корней: узел отдаёт транскрипты и файлы из каталогов, где работает агент",
            )
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

/// POST /kill — {pane}: закрыть пану вместе с агентом.
///
/// Отдельно от `/control`: там слэш-команда внутрь живого агента, здесь —
/// конец сессии. Уже мёртвая пана отвечает ошибкой tmux, и это нормальный
/// ответ: ноут по нему поймёт, что убирать нечего, и просто забудет сессию.
async fn kill(body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "ожидаю {pane}");
    };
    let pane = v.get("pane").and_then(Value::as_str).unwrap_or_default().trim();
    if pane.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "пустая пана");
    }
    tmux_result(tmux::kill(pane).await)
}

/// GET /agents?pids=1,2,3 — кто работает на этой машине ПРЯМО СЕЙЧАС.
///
/// Один ответ на три вопроса сверки, потому что все три задаются вместе, раз в
/// полминуты, и делить их на три круга по ssh незачем:
///   * `agents` — живые агенты: pid, пана, рабочий каталог, транскрипт;
///   * `panes`  — паны `-L jarvis` (инвариант «одна пана — одна сессия»);
///   * `alive`  — какие из спрошенных pid ещё живы.
///
/// Интерпретация — по-прежнему на ноуте: узел не знает ни статусов, ни того,
/// какие сессии тот уже видел.
async fn agents(req: Request) -> Response {
    let q = params(req.uri().query());
    let pids: Vec<i64> = q
        .get("pids")
        .map(|s| s.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).collect())
        .unwrap_or_default();
    let s = live::snapshot(&super::home_dir(), &pids).await;
    json_ok(&json!({
        "agents": s.agents,
        "panes": s.panes.iter().map(|p| p.pane.clone()).collect::<Vec<_>>(),
        "alive": s.alive,
        // tmux не установлен или сервер не поднят — состояние машины, а не сбой
        "error": s.error,
    }))
}

/// GET /projects — где на этой машине работали. Только оглавление: ноут сам
/// решит, что показать и что из этого прочитать через `/file`.
async fn projects() -> Response {
    let roots = sources::discover(&super::home_dir(), &super::jarvis_dir());
    match tokio::task::spawn_blocking(move || sources::projects(&roots)).await {
        Ok(value) => json_ok(&value), Err(_) => json_err(StatusCode::INTERNAL_SERVER_ERROR, "Не удалось прочитать каталог проектов")
    }
}

async fn provider_sources() -> Response {
    json_ok(&json!({"sources": sources::discover(&super::home_dir(), &super::jarvis_dir()).iter().map(|source| source.json()).collect::<Vec<_>>(), "protocol":2}))
}
async fn repair_source(body: Bytes) -> Response {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else { return json_err(StatusCode::BAD_REQUEST, "Ожидаю sourceId"); };
    let Some(id) = value["sourceId"].as_str().filter(|id| !id.is_empty() && id.len() <= 128) else { return json_err(StatusCode::BAD_REQUEST, "Ожидаю sourceId"); };
    match super::hooks::repair_source(id).await {
        Ok(value) => json_ok(&value),
        Err(error) => json_err(StatusCode::BAD_REQUEST, &error),
    }
}
async fn sessions() -> Response {
    let roots = sources::discover(&super::home_dir(), &super::jarvis_dir());
    match tokio::task::spawn_blocking(move || sources::sessions(&roots)).await {
        Ok(value) => json_ok(&value), Err(_) => json_err(StatusCode::INTERNAL_SERVER_ERROR, "Не удалось прочитать каталог сессий")
    }
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
    if cmd.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "нужен cwd и непустая команда");
    }
    let command;
    let cmd = if let Some(id) = v.get("sourceId").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        let roots = sources::discover(&super::home_dir(), &super::jarvis_dir());
        let Some(source) = roots.iter().find(|source| source.id == id) else {
            return json_err(StatusCode::BAD_REQUEST, "Источник агента не найден на узле");
        };
        let env = if source.agent == "codex" { "CODEX_HOME" } else { "CLAUDE_CONFIG_DIR" };
        command = format!("export {env}={} JARVIS_PROVIDER_INSTANCE_ID={}\n{cmd}", tmux::sh_quote(&source.home.to_string_lossy()), tmux::sh_quote(&source.id));
        command.as_str()
    } else { cmd };
    // Тильду раскрывает узел, а не ноут: домашний каталог ЭТОЙ машины знает
    // только он. Панель предлагает писать путь ровно так (`~/projects/…`), и
    // отказ на нём означал, что новый проект на узле не заводится вовсе.
    let Some(cwd) = expand_home(cwd, &super::home_dir()) else {
        return json_err(
            StatusCode::BAD_REQUEST,
            "нужен абсолютный путь или путь от ~: относительный не от чего считать",
        );
    };
    match tmux::launch(&cwd, cmd, v.get("name").and_then(Value::as_str)).await {
        // Пану возвращаем сразу: сессия агента ещё не зарегистрирована, и это
        // единственная ниточка, по которой запустивший может увидеть, что там
        // происходит, и ответить на первый вопрос.
        Ok((session, pane)) => json_ok(&json!({ "ok": true, "session": session, "pane": pane })),
        Err(msg) => json_err(StatusCode::BAD_GATEWAY, &msg),
    }
}

/// Путь запуска к абсолютному: `/…` как есть, `~` и `~/…` — от `$HOME`.
/// Относительный отвергаем: рабочего каталога у узла нет, и «считать от того,
/// откуда его запустил systemd» — значит завести проект неизвестно где.
fn expand_home(cwd: &str, home: &std::path::Path) -> Option<String> {
    if cwd.starts_with('/') {
        return Some(cwd.to_string());
    }
    let rest = cwd.strip_prefix('~')?.trim_start_matches('/');
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    Some(if rest.is_empty() { home.to_string() } else { format!("{home}/{rest}") })
}

/// GET /usage — лимиты аккаунта. `?fresh=1` минует кэш.
async fn usage(req: Request) -> Response {
    let query = params(req.uri().query());
    let fresh = query.get("fresh").is_some_and(|v| v == "1");
    if let Some(id) = query.get("sourceId").filter(|id| !id.is_empty()) {
        let roots = sources::discover(&super::home_dir(), &super::jarvis_dir());
        let Some(source) = roots.iter().find(|source| &source.id == id) else { return json_err(StatusCode::BAD_REQUEST,"Источник агента не найден"); };
        json_ok(&agent::usage_for(fresh, Some(source)).await)
    } else { json_ok(&agent::usage(fresh).await) }
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

    #[test]
    fn launch_path_accepts_tilde_and_rejects_relative() {
        let home = std::path::Path::new("/home/bob");
        assert_eq!(expand_home("/srv/x", home).as_deref(), Some("/srv/x"));
        // ровно то, что панель предлагает набрать в «Новом проекте»
        assert_eq!(expand_home("~/projects/app", home).as_deref(), Some("/home/bob/projects/app"));
        assert_eq!(expand_home("~", home).as_deref(), Some("/home/bob"));
        // рабочего каталога у узла нет — считать относительный путь не от чего
        assert_eq!(expand_home("projects/app", home), None);
        assert_eq!(expand_home("", home), None);
    }
}
