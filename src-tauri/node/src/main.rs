//! jarvis-node — узел Jarvis на удалённой машине (VPS, рабочая станция).
//!
//! Дизайн: docs/superpowers/specs/2026-08-05-remote-agents-design.md.
//!
//! Здесь только запуск: вся логика — в `src/node/`. Отдельный бинарь, а не
//! режим приложения, потому что узлу не нужны ни панель, ни голос, ни
//! уведомления (человек сидит за ноутом), а жить он должен на Linux-VPS —
//! то есть без Tauri и без `AppHandle` в коде вообще.
//!
//! Наружу узел не слушает ничего: только `<JARVIS_DIR>/node.sock` с правами
//! 0600, а ноут приходит через `ssh -L`.
//!
//! Переменные окружения:
//!   JARVIS_DIR          каталог данных (по умолчанию ~/.jarvis)
//!   JARVIS_NODE_SOCK    путь сокета целиком (перекрывает JARVIS_DIR)
//!   JARVIS_NODE_BUFFER  ёмкость кольца событий (по умолчанию 2000)
//!   CODEX_HOME          корень транскриптов Codex (по умолчанию ~/.codex)

mod node;

const USAGE: &str = "\
jarvis-node — узел Jarvis для удалённых агентов.

  jarvis-node            слушать <JARVIS_DIR>/node.sock (0600)
  jarvis-node --version  версия
  jarvis-node --help     эта справка

Наружу не слушает: ноут ходит через `ssh -L 127.0.0.1:PORT:<сокет>`.
";

#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => println!("jarvis-node {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h") => print!("{USAGE}"),
        _ => node::run().await,
    }
}
