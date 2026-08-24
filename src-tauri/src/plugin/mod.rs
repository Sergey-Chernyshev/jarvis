//! Плагинное ядро десктопа: «всё есть плагин»
//! (спека `2026-08-19-everything-is-plugin-design.md`).
//!
//! Здесь живёт граница между ядром и способностями: манифест (данные), контракт
//! рантайма (трейт `Plugin`), хост (жизненный цикл) и два рантайма — встроенный
//! (наш код в процессе) и внешний (`sidecar`, отдельный процесс на трубе).
//!
//! Модуль намеренно ничего не знает про конкретные способности: связь с демоном
//! — ровно один `impl HostEnv for Arc<Daemon>` ниже.

pub mod builtin;
pub mod contract;
pub mod host;
pub mod manifest;
pub mod sidecar;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};

pub use contract::{HostEnv, TrayItem};
pub use host::PluginHost;
pub use manifest::Manifest;

use crate::capability::contract::RiskClass;
use crate::daemon::Daemon;
use crate::util::jarvis_dir;

/// Боевой хост: контекст плагинов — демон.
pub type Host = PluginHost<Arc<Daemon>>;

/// Каталог установленных внешних плагинов: `~/.jarvis/plugins/<id>/`.
pub fn plugins_dir() -> PathBuf {
    jarvis_dir().join("plugins")
}

impl HostEnv for Arc<Daemon> {
    fn plugin_settings(&self, id: &str, defaults: Value) -> Value {
        self.settings.plugin(id, defaults)
    }

    fn set_plugin_settings(&self, id: &str, patch: Map<String, Value>) {
        self.settings.set_plugin(id, patch);
    }

    fn issue_token(&self, id: &str, classes: &[RiskClass]) -> Option<String> {
        Some(self.tokens.issue_plugin(id, classes))
    }

    fn revoke_token(&self, id: &str) {
        self.tokens.revoke_plugin(id);
    }

    fn plugins_changed(&self) {
        crate::tray::update(self, &self.snapshot());
        crate::windows::emit_to_panel(&self.app, "plugins", &self.plugins.status_json(self));
    }

    fn log(&self, line: &str) {
        // log::line сам дублирует в stdout, но молчит с выключенной
        // диагностикой — а жизненный цикл плагина должен быть виден всегда.
        if crate::log::enabled() {
            crate::log::line(line);
        } else {
            println!("{line}");
        }
    }
}

/// Собрать боевой хост: встроенные способности + внешние плагины из каталога.
pub fn build_host() -> Host {
    let host = Host::new();
    builtin::register_all(&host);
    discover_external(&host, &plugins_dir());
    host
}

/// Найти внешние плагины: каждый подкаталог с `manifest.json`. Битый манифест
/// — не молчаливый пропуск: плагин виден в списке со статусом `error`, иначе
/// пользователь ищет причину «почему ничего не появилось» вслепую.
pub fn discover_external(host: &Host, dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // каталога нет — это норма, а не ошибка
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort(); // детерминированный порядок в UI
    for d in dirs {
        match Manifest::load_dir(&d) {
            Ok(m) => {
                let id = m.id.clone();
                if let Err(err) = host.register(Arc::new(sidecar::Sidecar::new(m))) {
                    eprintln!("[plugin:{id}] {err}");
                }
            }
            Err(err) => {
                let name = d.file_name().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                eprintln!("[plugin:{name}] манифест не принят: {err}");
                if let Some(stub) = broken_stub(&name) {
                    let _ = host.register_broken(Arc::new(stub), err);
                }
            }
        }
    }
}

/// Заглушка для каталога с битым манифестом: показать имя и причину.
fn broken_stub(dir_name: &str) -> Option<sidecar::Sidecar> {
    let id: String = dir_name
        .chars()
        .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    let m = Manifest::parse(serde_json::json!({
        "id": if id.is_empty() { "unknown".to_string() } else { id },
        "name": dir_name,
        "version": "0.0.0",
        "kind": "external",
        "entry": { "path": "нет" },
        "description": "манифест не принят",
    }))
    .ok()?;
    Some(sidecar::Sidecar::new(m))
}
