//! Контракт плагина: что ядро умеет спросить у рантайма и что рантайм вправе
//! спросить у ядра (спека `2026-08-19-everything-is-plugin-design.md` §4).
//!
//! Трейты параметризованы контекстом `C` по тому же принципу, что `Registry<C>`
//! в слое капабилити: боевой контекст — `Arc<Daemon>`, тестовый — фейк. Ядро
//! хоста от Daemon не зависит и потому тестируется без Tauri.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};

use super::manifest::Manifest;
use crate::capability::contract::RiskClass;

/// Декларативный пункт меню трея от плагина. Ядро склеивает секции всех
/// включённых плагинов и маршрутизирует клик владельцу по префиксу id.
pub enum TrayItem {
    Label { text: String },
    /// `id` — уникальный внутри плагина идентификатор пункта; `cmd`+`args` —
    /// то, что нужно позвать по клику. Ядро клик не переводит: пункт меню сам
    /// несёт своё действие, поэтому в ядре нет таблицы «что значит эта кнопка».
    Action { id: String, text: String, cmd: String, args: Value },
    Check { id: String, text: String, checked: bool, enabled: bool, cmd: String, args: Value },
    Submenu { text: String, items: Vec<TrayItem> },
    Separator,
}

impl TrayItem {
    /// Пункт-действие «id = имя команды, аргументов нет» (частый случай).
    pub fn action(id: impl Into<String>, text: impl Into<String>) -> TrayItem {
        let id = id.into();
        TrayItem::Action {
            cmd: id.clone(),
            id,
            text: text.into(),
            args: serde_json::json!({}),
        }
    }

    /// Разобрать секцию трея, присланную внешним плагином. Кривой пункт
    /// пропускается молча: сломанное меню — не повод гасить плагин.
    pub fn from_json_list(v: &Value) -> Vec<TrayItem> {
        v.as_array().map(|a| a.iter().filter_map(TrayItem::from_json).collect()).unwrap_or_default()
    }

    fn from_json(v: &Value) -> Option<TrayItem> {
        let text = || v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let id = || v.get("id").and_then(|t| t.as_str()).map(|s| s.to_string());
        let args = || v.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        // cmd по умолчанию = id: у простых пунктов имя команды и есть их id
        let cmd = |v: &Value| {
            v.get("cmd")
                .or_else(|| v.get("id"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        };
        match v.get("type").and_then(|t| t.as_str())? {
            "label" => Some(TrayItem::Label { text: text() }),
            "action" => Some(TrayItem::Action {
                id: id()?,
                text: text(),
                cmd: cmd(v)?,
                args: args(),
            }),
            "check" => Some(TrayItem::Check {
                id: id()?,
                text: text(),
                checked: v.get("checked").and_then(|b| b.as_bool()).unwrap_or(false),
                enabled: v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true),
                cmd: cmd(v)?,
                args: args(),
            }),
            "submenu" => Some(TrayItem::Submenu {
                text: text(),
                items: v.get("items").map(TrayItem::from_json_list).unwrap_or_default(),
            }),
            "separator" => Some(TrayItem::Separator),
            _ => None,
        }
    }
}

/// Состояние рантайма плагина в глазах ядра.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Выключен тумблером или ещё не стартовал.
    #[default]
    Stopped,
    /// Работает: builtin активен, external ответил на handshake.
    Running,
    /// Сломался: не стартовал, упал сверх лимита рестартов, битый манифест.
    Error,
    /// Версия протокола не наша — грузить нельзя, но показать нужно.
    Incompatible,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Stopped => "stopped",
            Status::Running => "running",
            Status::Error => "error",
            Status::Incompatible => "incompatible",
        }
    }
}

/// Здоровье плагина — то, что видно в блоке «Здоровье» детали.
#[derive(Clone, Debug, Default)]
pub struct RunState {
    pub status: Status,
    pub error: Option<String>,
    /// Момент старта, мс (0 — не стартовал).
    pub started_at: i64,
    pub pid: Option<i64>,
    pub restarts: u32,
    /// Плагин непригоден в принципе (битый манифест): включать нечего.
    pub broken: bool,
}

impl RunState {
    pub fn to_json(&self, now: i64) -> Value {
        serde_json::json!({
            "status": self.status.as_str(),
            "error": self.error,
            "pid": self.pid,
            "restarts": self.restarts,
            "uptimeMs": if self.started_at > 0 && self.status == Status::Running {
                now - self.started_at
            } else { 0 },
        })
    }
}

pub type CallFut = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

/// Отказ старта. Отдельный тип нужен ровно ради одного различия: «не завёлся»
/// (чинится перезапуском) против «протокол не наш» (перезапуск бессмыслен).
#[derive(Clone, Debug)]
pub struct StartFail {
    pub message: String,
    pub incompatible: bool,
}

impl StartFail {
    pub fn incompatible(message: impl Into<String>) -> Self {
        StartFail { message: message.into(), incompatible: true }
    }
}

impl From<String> for StartFail {
    fn from(s: String) -> Self {
        StartFail { message: s, incompatible: false }
    }
}

impl From<&str> for StartFail {
    fn from(s: &str) -> Self {
        StartFail { message: s.to_string(), incompatible: false }
    }
}

/// Рантайм плагина. Реализуют: встроенные способности (напрямую) и
/// `sidecar::Sidecar` (труба к дочернему процессу).
///
/// `start`/`stop` синхронны намеренно: у встроенных это «взять/отпустить
/// ресурс» (так уже устроен `power::activate_*`), у сайдкара — spawn/kill,
/// а ожидание handshake живёт внутри рантайма, не в жизненном цикле ядра.
pub trait Plugin<C>: Send + Sync {
    fn manifest(&self) -> &Manifest;

    /// `token` — выпущенный ядром токен для вызовов капабилити по сокету:
    /// у внешнего плагина `Some`, у встроенного всегда `None` (наш код ходит
    /// в сервисы напрямую и гранта не получает — §6 спеки).
    fn start(&self, ctx: &C, token: Option<&str>) -> Result<(), StartFail> {
        let (_, _) = (ctx, token);
        Ok(())
    }

    fn stop(&self, ctx: &C) {
        let _ = ctx;
    }

    /// Свободная форма для UI: то, что плагин хочет показать о себе.
    fn status(&self, ctx: &C) -> Value {
        let _ = ctx;
        Value::Null
    }

    /// Здоровье, известное самому рантайму (сайдкар знает про свои падения и
    /// рестарты больше, чем хост). `None` — верить бухгалтерии хоста.
    fn health(&self, ctx: &C) -> Option<RunState> {
        let _ = ctx;
        None
    }

    /// Секция трея. Идентификаторы действий ядро само префиксует владельцем.
    fn tray(&self, ctx: &C) -> Vec<TrayItem> {
        let _ = ctx;
        Vec::new()
    }

    /// Выполнить команду из `commands[]` манифеста.
    fn call(&self, ctx: C, name: String, args: Value) -> CallFut;
}

/// Услуги ядра, нужные хосту. Отдельный трейт, а не `Arc<Daemon>` напрямую,
/// чтобы `plugin/` не знал про демона (и тестировался без него).
pub trait HostEnv: Clone + Send + Sync + 'static {
    /// Настройки плагина: дефолты ⊕ `settings.plugins.<id>`.
    fn plugin_settings(&self, id: &str, defaults: Value) -> Value;
    fn set_plugin_settings(&self, id: &str, patch: Map<String, Value>);
    /// Выпустить токен внешнему плагину (грант least-privilege из манифеста).
    fn issue_token(&self, id: &str, classes: &[RiskClass]) -> Option<String>;
    fn revoke_token(&self, id: &str);
    /// Состав/статусы плагинов изменились: обновить трей и панель.
    fn plugins_changed(&self);
    /// Строка в общий лог (у сайдкаров туда же уходит stderr).
    fn log(&self, line: &str);
}

/// Удобный алиас: рантайм в реестре хоста.
pub type Shared<C> = Arc<dyn Plugin<C>>;
