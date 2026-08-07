//! CLI-обёртка над общей install-логикой (она в src/install/mod.rs).
//!
//!   jarvis-setup install     — вшить хуки + транспорт + Silero
//!   jarvis-setup uninstall   — вычистить интеграцию
//!   jarvis-setup status      — показать, что установлено
//!   jarvis-setup repair      — только интеграция агентов (хуки + шим)
//!   jarvis-setup remote add <имя> <ssh-хост> [--dir <путь>]
//!                            — поставить узел на удалённую машину (docs/remote.md)
//!   jarvis-setup remote status <имя>
//!                            — жив ли узел: процесс, сокет, версия, паны tmux
//!
//! Та же логика используется приложением (онбординг первого запуска).

#[path = "../install/mod.rs"]
mod install;

use install::{Step, StepState};

const USAGE: &str = "\
Использование:
  jarvis-setup install                    вшить хуки + транспорт + Silero
  jarvis-setup uninstall                  вычистить интеграцию
  jarvis-setup status                     показать, что установлено
  jarvis-setup repair                     починить интеграцию агентов (хуки + шим)
  jarvis-setup remote add <имя> <ssh-хост> [--dir <путь>] [--no-tcp]
                                          поставить узел на удалённую машину
  jarvis-setup remote status <имя>        жив ли узел на той стороне
";

const REMOTE_USAGE: &str = "\
Удалённые узлы (подробности — docs/remote.md):
  jarvis-setup remote add <имя> <ssh-хост> [--dir <путь>]
      Ставит jarvis-node на ту машину, прописывает хуки агентов, настраивает
      автозапуск и добавляет узел в настройки Jarvis.
      <имя>      как узел будет называться в списке сессий (латиница/цифры/.-_)
      <ssh-хост> то же, что пишешь в ssh: алиас из ~/.ssh/config или user@адрес
      --dir      каталог Jarvis на той стороне (по умолчанию ~/.jarvis)
      --tcp=N    порт узла на петле для мобильного клиента (по умолчанию 7717)
      --no-tcp   не поднимать этот порт (машиной пользуешься не только ты)

  jarvis-setup remote status <имя>
      Процесс узла, сокет, версия, состояние юнита и живые паны tmux.
";

/// Печать шага установки для терминала.
fn print_step(s: Step) {
    match s.state {
        StepState::Start => println!("▸ {}", s.phase),
        StepState::Done => println!("  ✓ {}", s.msg),
        StepState::Warn => println!("  ⚠ {}", s.msg),
        StepState::Info => println!("  • {}", s.msg),
    }
}

/// Итог команды: ошибка — это внятная строка в stderr и ненулевой код, а не
/// паника со стектрейсом. Установка узла ходит по чужой машине, и половина
/// причин отказа (ключи, архитектура, systemd) требует текста, а не бэктрейса.
fn finish(res: Result<(), String>) {
    if let Err(e) = res {
        eprintln!("✗ {e}");
        std::process::exit(1);
    }
}

fn die(msg: &str) -> ! {
    eprintln!("✗ {msg}\n\n{REMOTE_USAGE}");
    std::process::exit(1);
}

/// `jarvis-setup remote …` — узлы на других машинах.
fn remote(args: &[String]) {
    let mut positional: Vec<&str> = Vec::new();
    let mut dir: Option<String> = None;
    let mut tcp: Option<u16> = Some(install::remote::DEFAULT_TCP_PORT);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--dir" => match rest.next() {
                Some(value) => dir = Some(value.clone()),
                None => die("--dir без пути"),
            },
            // Порт на петле для мобильного клиента ставится по умолчанию;
            // выключаем там, где машиной пользуется не только владелец.
            "--no-tcp" => tcp = None,
            other if other.starts_with("--tcp=") => {
                match other.trim_start_matches("--tcp=").parse::<u16>() {
                    Ok(p) if p > 0 => tcp = Some(p),
                    _ => die("--tcp= ждёт номер порта"),
                }
            }
            other if other.starts_with("--dir=") => {
                dir = Some(other.trim_start_matches("--dir=").to_string())
            }
            other if other.starts_with('-') => die(&format!("не знаю ключ {other}")),
            other => positional.push(other),
        }
    }
    match positional.split_first() {
        Some((&"add", [name, host])) => {
            finish(install::remote::add(&print_step, name, host, dir.as_deref(), tcp))
        }
        Some((&"status", [name])) => finish(install::remote::status(&print_step, name)),
        Some((&"add", _)) => die("remote add ждёт ровно два аргумента: <имя> <ssh-хост>"),
        Some((&"status", _)) => die("remote status ждёт одно имя узла"),
        _ => {
            eprint!("{REMOTE_USAGE}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => {
            // прокси для скачивания моделей — из env (HTTPS_PROXY) или флага
            let proxy = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("HTTP_PROXY").ok());
            install::install(&print_step, proxy.as_deref());
            println!("\nГотово. Активные сессии Claude Code перезапусти — хуки берутся");
            println!("снапшотом на старте сессии. Шим в текущем шелле: exec zsh (или новая вкладка).");
        }
        Some("uninstall") => install::uninstall(&print_step),
        Some("status") => print!("{}", install::status_report()),
        Some("repair") => {
            // Только интеграция агентов (хуки + шим), без Silero/STT/моделей.
            install::repair(&print_step);
            println!("\nИнтеграция починена. Если codex-шим доустановлен — перезапусти");
            println!("шелл (exec zsh) или открой новую вкладку, чтобы `codex` пошёл через Jarvis.");
        }
        Some("remote") => remote(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            std::process::exit(1);
        }
    }
}
