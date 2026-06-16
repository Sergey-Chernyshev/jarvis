//! CLI-обёртка над общей install-логикой (она в src/install/mod.rs).
//!
//!   jarvis-setup install     — вшить хуки + транспорт + Silero
//!   jarvis-setup uninstall   — вычистить интеграцию
//!   jarvis-setup status      — показать, что установлено
//!
//! Та же логика используется приложением (онбординг первого запуска).

#[path = "../install/mod.rs"]
mod install;

use install::{Step, StepState};

/// Печать шага установки для терминала.
fn print_step(s: Step) {
    match s.state {
        StepState::Start => println!("▸ {}", s.phase),
        StepState::Done => println!("  ✓ {}", s.msg),
        StepState::Warn => println!("  ⚠ {}", s.msg),
        StepState::Info => println!("  • {}", s.msg),
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("install") => {
            // прокси для скачивания моделей — из env (HTTPS_PROXY) или флага
            let proxy = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("HTTP_PROXY").ok());
            install::install(&print_step, proxy.as_deref());
            println!("\nГотово. Активные сессии Claude Code перезапусти — хуки берутся");
            println!("снапшотом на старте сессии. Шим в текущем шелле: exec zsh (или новая вкладка).");
        }
        Some("uninstall") => install::uninstall(&print_step),
        Some("status") => print!("{}", install::status_report()),
        _ => {
            eprintln!("Использование: jarvis-setup <install|uninstall|status>");
            std::process::exit(1);
        }
    }
}
