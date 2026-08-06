//! `jarvis-node` — узел Jarvis на удалённой машине (VPS, рабочая станция).
//!
//! Дизайн: docs/superpowers/specs/2026-08-05-remote-agents-design.md.
//!
//! Узел — не второй Jarvis, а тонкий транспорт: принимает хуки в свой
//! unix-сокет, копит их в кольцевом буфере с монотонным курсором и умеет три
//! вещи наружу — отдать события, отдать кусок транскрипта, вставить текст в
//! tmux. Ни реестра, ни статусов, ни уведомлений здесь нет: за ноутом сидит
//! человек, и всю интерпретацию делает ноут своим существующим кодом.
//!
//! Отсюда и отсутствие связей с остальным крейтом. `Daemon` сцеплен с
//! `AppHandle` (74 места — headless-режим означал бы рефактор всего ядра),
//! `util`/`tmux` тянут за собой модель и реестр. Узел — отдельный бинарь, в
//! коде которого нет ни Tauri, ни `AppHandle`: он обязан жить на Linux-VPS.
//! Три десятка строк помощников продублированы сознательно: сцепка ради них
//! обошлась бы дороже дубля.
//!
//! Наружу узел не слушает ничего. Единственный вход — сокет 0600, а к нему
//! ноут пробрасывается через `ssh -L`: аутентификация — обычная SSH,
//! собственных секретов Jarvis не заводит.

pub mod files;
pub mod http;
pub mod projects;
pub mod ring;
pub mod tmux;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;

use ring::{Ring, Slice, Stats};

/// Ёмкость кольца по умолчанию (дизайн). На болтливой машине запас стоит
/// поднять через JARVIS_NODE_BUFFER: память здесь дешевле, чем дырка в ленте,
/// из-за которой ноут перечитывает транскрипты целиком.
const DEFAULT_CAPACITY: usize = 2000;

/// Всё, что узел помнит между запросами.
pub struct Node {
    events: Mutex<Ring>,
    /// «Звонок» для long-poll: значение — курсор последнего события. watch, а
    /// не Notify: подписаться можно ДО чтения буфера, поэтому событие,
    /// пришедшее в зазор между чтением и ожиданием, не пролежит все 25 секунд.
    bell: watch::Sender<u64>,
    started: Instant,
    host: String,
    roots: Vec<PathBuf>,
}

impl Node {
    pub fn new(capacity: usize, roots: Vec<PathBuf>, host: String) -> Node {
        let (bell, _) = watch::channel(0u64);
        Node {
            events: Mutex::new(Ring::new(capacity)),
            bell,
            started: Instant::now(),
            host,
            roots,
        }
    }

    pub fn push(&self, envelope: Value) -> u64 {
        let cursor = self.events.lock().unwrap().push(envelope, now_ms());
        // send_replace, а не send: подписчиков может не быть вовсе (ноут спит),
        // и это штатная ситуация, а не ошибка отправки
        self.bell.send_replace(cursor);
        cursor
    }

    pub fn slice(&self, since: u64) -> Slice {
        self.events.lock().unwrap().since(since)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.bell.subscribe()
    }

    pub fn stats(&self) -> Stats {
        self.events.lock().unwrap().stats()
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// Каталог данных Jarvis на этой машине: $JARVIS_DIR или ~/.jarvis. Логика
/// повторяет демона и jarvis-hook не случайно: хук вычисляет сокет от своего
/// расположения, и узел обязан слушать ровно там, куда хук будет стучаться.
pub fn jarvis_dir() -> PathBuf {
    match std::env::var("JARVIS_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => home_dir().join(".jarvis"),
    }
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// Сокет узла — `node.sock`, а не `run.sock`: на одной машине узел и демон
/// могут стоять рядом, и делить один путь им нельзя.
pub fn sock_path() -> PathBuf {
    match std::env::var("JARVIS_NODE_SOCK") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => jarvis_dir().join("node.sock"),
    }
}

fn capacity_from_env() -> usize {
    capacity_of(std::env::var("JARVIS_NODE_BUFFER").ok().as_deref())
}

/// Разбор ёмкости отдельно от env — иначе его нельзя проверить тестом, не
/// затрагивая переменные всего процесса. Мусор и ноль молча откатываем к
/// дефолту: узел не должен не запуститься из-за опечатки в юните systemd.
fn capacity_of(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_CAPACITY)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Имя хоста — единственное, чем узел представляется ноуту. Спрашиваем один раз
/// у `hostname`: ради одной строки тащить libc-обвязку не за чем.
fn hostname() -> String {
    if let Ok(out) = std::process::Command::new("hostname").output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Поднять сокет и слушать до сигнала завершения.
pub async fn run() {
    let sock = sock_path();
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
        // 0700 на каталоге закрывает окно между bind и chmod ниже: пока сокет
        // ещё с правами по umask, до него всё равно не дойти чужому.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    // прошлый сокет мог остаться от узла, убитого -9: bind по занятому пути
    // падает, хотя слушателя за ним давно нет
    let _ = std::fs::remove_file(&sock);

    let listener = match tokio::net::UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("[jarvis-node] не смог открыть сокет {}: {err}", sock.display());
            std::process::exit(1);
        }
    };
    // 0600 — вся защита узла: наружу он не слушает ничего, а внутри машины
    // пускает только владельца. Аутентификация — SSH (дизайн, §«Транспорт»).
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));

    let node = Arc::new(Node::new(
        capacity_from_env(),
        files::transcript_roots(&home_dir()),
        hostname(),
    ));
    println!(
        "[jarvis-node] {} слушаю {} (буфер {})",
        node.host(),
        sock.display(),
        node.stats().capacity
    );

    if let Err(err) = axum::serve(listener, http::router(node))
        .with_graceful_shutdown(terminate())
        .await
    {
        eprintln!("[jarvis-node] сервер остановлен: {err}");
    }
    // за собой убираем: jarvis-hook проверяет `[ -S socket ]` и на осиротевшем
    // файле тратил бы таймаут curl на каждое событие Claude Code
    let _ = std::fs::remove_file(&sock);
    println!("[jarvis-node] остановлен");
}

/// Узел живёт под systemd/launchd, и штатное «стоп» приходит SIGTERM — на нём
/// надо успеть убрать сокет, иначе следующий старт наткнётся на чужой файл.
async fn terminate() {
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        // без обработчика SIGTERM остаёмся хотя бы на Ctrl-C
        Err(err) => {
            eprintln!("[jarvis-node] нет обработчика SIGTERM: {err}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node() -> Node {
        Node::new(3, vec![PathBuf::from("/nowhere")], "vps".into())
    }

    // Курсор растёт монотонно и переживает вытеснение.
    #[test]
    fn push_returns_growing_cursor() {
        let n = node();
        assert_eq!(n.push(json!({ "event": "prompt" })), 0);
        assert_eq!(n.push(json!({ "event": "stop" })), 1);
        assert_eq!(n.stats().cursor, 2);
    }

    // Ёмкость из env: дизайн разрешает её крутить, дефолт — 2000.
    #[test]
    fn capacity_reads_env_and_survives_garbage() {
        assert_eq!(DEFAULT_CAPACITY, 2000);
        assert_eq!(capacity_of(Some(" 50 ")), 50);
        assert_eq!(capacity_of(None), 2000);
        assert_eq!(capacity_of(Some("")), 2000);
        assert_eq!(capacity_of(Some("nope")), 2000);
        assert_eq!(capacity_of(Some("0")), 2000, "ноль = «теряем всё молча»");
    }

    // Long-poll обязан просыпаться на событие, а не досиживать окно.
    #[tokio::test(flavor = "current_thread")]
    async fn bell_wakes_a_waiting_poller() {
        let n = Arc::new(node());
        let mut bell = n.subscribe();
        let writer = n.clone();
        tokio::spawn(async move {
            writer.push(json!({ "event": "notification" }));
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), bell.changed())
            .await
            .expect("звонок должен разбудить ожидающего")
            .expect("отправитель жив, пока жив узел");
        assert_eq!(n.stats().buffered, 1);
    }
}
