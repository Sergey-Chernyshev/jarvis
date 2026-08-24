//! Хост плагинов: реестр, тумблеры, жизненный цикл, статусы, трей
//! (спека `2026-08-19-everything-is-plugin-design.md` §3, §4).
//!
//! **INV-KERNEL** (§2.2): здесь нет ни одного ветвления по идентификатору
//! конкретного плагина. Всё, что хост делает с плагином, он делает по данным
//! его манифеста и через его рантайм. Нарушение этого правила возвращает нас
//! к хардкоду, ради ухода от которого хост и написан.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::contract::{HostEnv, RunState, Shared, Status, TrayItem};
use super::manifest::{Manifest, PluginKind, RESERVED_COMMANDS};
use crate::util::now_ms;

/// Сколько ждём ответа команды рантайма. Скилы по голосовой спеке — 10с,
/// команды из UI живут по тому же бюджету: дольше — это уже не «нажал кнопку».
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Зарегистрированный плагин: рантайм + то, что ядро знает о его здоровье.
pub struct Registered<C: HostEnv> {
    pub plugin: Shared<C>,
    pub run: Mutex<RunState>,
}

impl<C: HostEnv> Registered<C> {
    pub fn manifest(&self) -> &Manifest {
        self.plugin.manifest()
    }
    pub fn id(&self) -> &str {
        &self.plugin.manifest().id
    }
    pub fn status(&self) -> Status {
        self.run.lock().unwrap().status
    }
}

pub struct PluginHost<C: HostEnv> {
    /// Порядок регистрации = порядок в UI и в трее (детерминированный).
    entries: RwLock<Vec<Arc<Registered<C>>>>,
    /// Бюджет одной команды рантайма (тесты укорачивают его до миллисекунд).
    call_timeout: Duration,
    /// Куда ведёт пункт меню: id пункта → (плагин, команда, аргументы).
    /// Заполняется при сборке трея — ядру не нужно знать, что значит клик.
    routes: RwLock<HashMap<String, (String, String, Value)>>,
}

impl<C: HostEnv> Default for PluginHost<C> {
    fn default() -> Self {
        PluginHost {
            entries: RwLock::new(Vec::new()),
            call_timeout: CALL_TIMEOUT,
            routes: RwLock::new(HashMap::new()),
        }
    }
}

impl<C: HostEnv> PluginHost<C> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Укоротить бюджет команды (тестам — чтобы не ждать секунды впустую).
    #[cfg(test)]
    pub fn with_call_timeout(mut self, t: Duration) -> Self {
        self.call_timeout = t;
        self
    }

    /// Добавить плагин в реестр. Дубль id — отказ: два плагина с одним id
    /// делят блок настроек и токен, это тихая порча, а не мелочь.
    pub fn register(&self, plugin: Shared<C>) -> Result<(), String> {
        let id = plugin.manifest().id.clone();
        let mut list = self.entries.write().unwrap();
        if list.iter().any(|e| e.id() == id) {
            return Err(format!("плагин '{id}' уже зарегистрирован"));
        }
        list.push(Arc::new(Registered { plugin, run: Mutex::new(RunState::default()) }));
        Ok(())
    }

    /// Зарегистрировать плагин, который не смог загрузиться (битый манифест
    /// внешнего каталога): в реестре его нет, но пользователь должен видеть,
    /// что каталог есть и почему он не работает.
    pub fn register_broken(&self, plugin: Shared<C>, why: String) -> Result<(), String> {
        let id = plugin.manifest().id.clone();
        self.register(plugin)?;
        if let Some(e) = self.find(&id) {
            let mut run = e.run.lock().unwrap();
            run.status = Status::Error;
            run.error = Some(why);
            run.broken = true;
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<Arc<Registered<C>>> {
        self.entries.read().unwrap().clone()
    }

    pub fn find(&self, id: &str) -> Option<Arc<Registered<C>>> {
        self.entries.read().unwrap().iter().find(|e| e.id() == id).cloned()
    }

    /// Включён ли плагин: `settings.plugins.<id>.enabled`, дефолт из манифеста.
    pub fn is_enabled(&self, env: &C, m: &Manifest) -> bool {
        env.plugin_settings(&m.id, m.setting_defaults())["enabled"]
            .as_bool()
            .unwrap_or(m.default_enabled)
    }

    /// Поднять всё, что включено. Зовётся один раз на старте демона.
    pub fn init(&self, env: &C) {
        for e in self.list() {
            if e.run.lock().unwrap().broken {
                continue; // манифест не принят — стартовать нечего
            }
            if self.is_enabled(env, e.manifest()) {
                self.start_one(env, &e);
            }
        }
    }

    /// Погасить всё при выходе (снять ассерты, убить сайдкары).
    pub fn dispose(&self, env: &C) {
        for e in self.list() {
            if e.status() == Status::Running {
                self.stop_one(env, &e);
            }
        }
    }

    fn start_one(&self, env: &C, e: &Arc<Registered<C>>) {
        let m = e.manifest();
        // Токен получает только внешний плагин: он ходит в ядро по сокету и
        // проходит гейт. Встроенный — наш код, гранта не получает (§6).
        let token = match m.kind {
            PluginKind::External => env.issue_token(&m.id, &m.risk_classes()),
            PluginKind::Builtin => None,
        };
        let res = e.plugin.start(env, token.as_deref());
        let mut run = e.run.lock().unwrap();
        match res {
            Ok(()) => {
                run.status = Status::Running;
                run.error = None;
                run.started_at = now_ms();
                env.log(&format!("[plugin:{}] включён", m.id));
            }
            Err(fail) => {
                run.status =
                    if fail.incompatible { Status::Incompatible } else { Status::Error };
                run.error = Some(fail.message.clone());
                run.started_at = 0;
                drop(run);
                if m.kind == PluginKind::External {
                    env.revoke_token(&m.id);
                }
                env.log(&format!("[plugin:{}] не стартовал: {}", m.id, fail.message));
            }
        }
    }

    fn stop_one(&self, env: &C, e: &Arc<Registered<C>>) {
        let m = e.manifest();
        e.plugin.stop(env);
        if m.kind == PluginKind::External {
            // Отзыв токена — часть выключения, а не забота рантайма: иначе
            // упавший плагин оставил бы за собой действующий грант.
            env.revoke_token(&m.id);
        }
        let mut run = e.run.lock().unwrap();
        run.status = Status::Stopped;
        run.started_at = 0;
        run.pid = None;
        env.log(&format!("[plugin:{}] выключен", m.id));
    }

    /// Тумблер плагина: пишет настройку и приводит рантайм в соответствие.
    pub fn set_enabled(&self, env: &C, id: &str, on: bool) -> Value {
        let Some(e) = self.find(id) else {
            return json!({ "ok": false, "error": "плагин не найден" });
        };
        if on && matches!(e.status(), Status::Incompatible) {
            let why = e.run.lock().unwrap().error.clone().unwrap_or_default();
            return json!({ "ok": false, "error": why });
        }
        if on && e.run.lock().unwrap().broken {
            // Битый манифест включить нечем — но настройку всё равно пишем,
            // чтобы после починки плагин поднялся сам.
            let mut patch = Map::new();
            patch.insert("enabled".into(), Value::Bool(true));
            env.set_plugin_settings(id, patch);
            let why = e.run.lock().unwrap().error.clone().unwrap_or_default();
            return json!({ "ok": false, "error": why });
        }
        let mut patch = Map::new();
        patch.insert("enabled".into(), Value::Bool(on));
        env.set_plugin_settings(id, patch);

        let running = e.status() == Status::Running;
        if on && !running {
            self.start_one(env, &e);
        } else if !on && running {
            self.stop_one(env, &e);
        }
        env.plugins_changed();
        let run = e.run.lock().unwrap();
        match (&run.status, &run.error) {
            (Status::Error, Some(err)) => json!({ "ok": false, "error": err }),
            _ => json!({ "ok": true }),
        }
    }

    /// Команда плагину. `_enable` исполняет ядро; остальное уходит рантайму
    /// под таймаутом. Не ветвится по id плагина — только по имени команды.
    pub async fn cmd(&self, env: &C, id: &str, name: &str, args: Value) -> Value {
        if name == "_enable" {
            let on = args.get("on").and_then(Value::as_bool).unwrap_or(false);
            return self.set_enabled(env, id, on);
        }
        let Some(e) = self.find(id) else {
            return json!({ "ok": false, "error": "плагин не найден" });
        };
        if RESERVED_COMMANDS.contains(&name) {
            return json!({ "ok": false, "error": format!("команда '{name}' — внутренняя") });
        }
        if e.status() != Status::Running {
            return json!({ "ok": false, "error": "плагин выключен" });
        }
        if !e.manifest().commands.iter().any(|c| c.name == name) {
            return json!({ "ok": false, "error": format!("неизвестная команда: {name}") });
        }
        let fut = e.plugin.call(env.clone(), name.to_string(), args);
        let out = match tokio::time::timeout(self.call_timeout, fut).await {
            Ok(Ok(v)) => json!({ "ok": true, "value": v }),
            Ok(Err(err)) => json!({ "ok": false, "error": err }),
            Err(_) => json!({ "ok": false, "error": "плагин не ответил вовремя" }),
        };
        env.plugins_changed();
        out
    }

    /// Статусы для панели и вкладки «Плагины».
    ///
    /// Форма поля `status` сохранена от `power::statuses()` (там это
    /// произвольный объект способности) — иначе вкладка «Бодрость», которая
    /// читает `p.status.armed`, сломалась бы при переезде на хост.
    pub fn status_json(&self, env: &C) -> Value {
        let now = now_ms();
        let items: Vec<Value> = self
            .list()
            .iter()
            .map(|e| {
                let m = e.manifest();
                let enabled = self.is_enabled(env, m);
                let running = e.status() == Status::Running;
                let state = if running { e.plugin.status(env) } else { Value::Null };
                let health = e
                    .plugin
                    .health(env)
                    .unwrap_or_else(|| e.run.lock().unwrap().clone())
                    .to_json(now);
                json!({
                    "id": m.id,
                    "name": m.name,
                    "version": m.version,
                    "description": m.description,
                    "icon": m.icon,
                    "kind": m.kind.as_str(),
                    "builtin": m.kind == PluginKind::Builtin,
                    "pane": m.pane,
                    "tray": m.tray,
                    "enabled": enabled,
                    "status": state,
                    "health": health,
                    "settingsSchema": serde_json::to_value(&m.settings).unwrap_or(Value::Null),
                    "settingsValues": env.plugin_settings(&m.id, m.setting_defaults()),
                    "commands": serde_json::to_value(&m.commands).unwrap_or(Value::Null),
                    "capabilities": m.capabilities,
                    "uses": m.uses,
                    "provides": serde_json::to_value(&m.provides).unwrap_or(Value::Null),
                    "consumes": serde_json::to_value(&m.consumes).unwrap_or(Value::Null),
                    "skills": serde_json::to_value(&m.skills).unwrap_or(Value::Null),
                })
            })
            .collect();
        Value::Array(items)
    }

    /// Записать значение настройки плагина. Ключ обязан быть в схеме манифеста:
    /// иначе UI/плагин смогли бы писать в чужой блок что угодно.
    pub fn set_setting(&self, env: &C, id: &str, key: &str, value: Value) -> Value {
        let Some(e) = self.find(id) else {
            return json!({ "ok": false, "error": "плагин не найден" });
        };
        if !e.manifest().settings.iter().any(|s| s.key == key) {
            return json!({ "ok": false, "error": format!("нет такой настройки: {key}") });
        }
        let mut patch = Map::new();
        patch.insert(key.to_string(), value);
        env.set_plugin_settings(id, patch);
        env.plugins_changed();
        json!({ "ok": true })
    }

    /// Секции трея всех включённых плагинов, склеенные в порядке регистрации.
    /// Идентификаторы действий префиксуются владельцем (`plug:<id>:<action>`),
    /// чтобы клик вернулся в свой плагин без таблицы имён в ядре.
    pub fn tray_items(&self, env: &C) -> Vec<TrayItem> {
        let mut out = Vec::new();
        let mut routes = HashMap::new();
        for e in self.list() {
            let m = e.manifest();
            if !m.tray || e.status() != Status::Running {
                continue;
            }
            let items = e.plugin.tray(env);
            if items.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(TrayItem::Separator);
            }
            out.extend(items.into_iter().map(|i| prefix_item(&m.id, i, &mut routes)));
        }
        *self.routes.write().unwrap() = routes;
        out
    }

    /// Куда ведёт клик по пункту меню. `None` — пункт не наш.
    pub fn route_tray(&self, menu_id: &str) -> Option<(String, String, Value)> {
        self.routes.read().unwrap().get(menu_id).cloned()
    }

}

fn prefix_item(
    owner: &str,
    item: TrayItem,
    routes: &mut HashMap<String, (String, String, Value)>,
) -> TrayItem {
    match item {
        TrayItem::Action { id, text, cmd, args } => {
            let menu_id = format!("plug:{owner}:{id}");
            routes.insert(menu_id.clone(), (owner.to_string(), cmd.clone(), args.clone()));
            TrayItem::Action { id: menu_id, text, cmd, args }
        }
        TrayItem::Check { id, text, checked, enabled, cmd, args } => {
            let menu_id = format!("plug:{owner}:{id}");
            routes.insert(menu_id.clone(), (owner.to_string(), cmd.clone(), args.clone()));
            TrayItem::Check { id: menu_id, text, checked, enabled, cmd, args }
        }
        TrayItem::Submenu { text, items } => TrayItem::Submenu {
            text,
            items: items.into_iter().map(|i| prefix_item(owner, i, routes)).collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::contract::RiskClass;
    use crate::plugin::contract::{CallFut, Plugin};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Фейковое ядро: настройки в памяти, токены в памяти, лог в вектор.
    #[derive(Clone, Default)]
    struct Env(Arc<EnvInner>);

    #[derive(Default)]
    struct EnvInner {
        settings: Mutex<serde_json::Map<String, Value>>,
        tokens: Mutex<Vec<(String, Vec<RiskClass>)>>,
        changed: AtomicUsize,
        log: Mutex<Vec<String>>,
    }

    impl Env {
        fn token_of(&self, id: &str) -> Option<Vec<RiskClass>> {
            self.0.tokens.lock().unwrap().iter().find(|(i, _)| i == id).map(|(_, c)| c.clone())
        }
    }

    impl HostEnv for Env {
        fn plugin_settings(&self, id: &str, defaults: Value) -> Value {
            let mut out = defaults;
            if let Some(saved) = self.0.settings.lock().unwrap().get(id).and_then(|v| v.as_object())
            {
                if let Some(dst) = out.as_object_mut() {
                    for (k, v) in saved {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
            out
        }
        fn set_plugin_settings(&self, id: &str, patch: serde_json::Map<String, Value>) {
            let mut s = self.0.settings.lock().unwrap();
            let block = s.entry(id.to_string()).or_insert_with(|| json!({}));
            let obj = block.as_object_mut().unwrap();
            for (k, v) in patch {
                obj.insert(k, v);
            }
        }
        fn issue_token(&self, id: &str, classes: &[RiskClass]) -> Option<String> {
            let mut t = self.0.tokens.lock().unwrap();
            t.retain(|(i, _)| i != id);
            t.push((id.to_string(), classes.to_vec()));
            Some(format!("token-{id}"))
        }
        fn revoke_token(&self, id: &str) {
            self.0.tokens.lock().unwrap().retain(|(i, _)| i != id);
        }
        fn plugins_changed(&self) {
            self.0.changed.fetch_add(1, Ordering::SeqCst);
        }
        fn log(&self, line: &str) {
            self.0.log.lock().unwrap().push(line.to_string());
        }
    }

    /// Тестовый плагин: считает старты/стопы, помнит выданный токен.
    struct Fake {
        manifest: Manifest,
        starts: AtomicUsize,
        stops: AtomicUsize,
        token: Mutex<Option<String>>,
        fail_start: bool,
        slow: bool,
    }

    impl Fake {
        fn new(v: Value) -> Arc<Self> {
            Arc::new(Fake {
                manifest: Manifest::parse(v).expect("валидный манифест в тесте"),
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                token: Mutex::new(None),
                fail_start: false,
                slow: false,
            })
        }
    }

    impl Plugin<Env> for Fake {
        fn manifest(&self) -> &Manifest {
            &self.manifest
        }
        fn start(&self, _env: &Env, token: Option<&str>) -> Result<(), crate::plugin::contract::StartFail> {
            if self.fail_start {
                return Err("не завёлся".into());
            }
            self.starts.fetch_add(1, Ordering::SeqCst);
            *self.token.lock().unwrap() = token.map(|t| t.to_string());
            Ok(())
        }
        fn stop(&self, _env: &Env) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            *self.token.lock().unwrap() = None;
        }
        fn status(&self, _env: &Env) -> Value {
            json!({ "starts": self.starts.load(Ordering::SeqCst) })
        }
        fn tray(&self, _env: &Env) -> Vec<TrayItem> {
            vec![
                TrayItem::Label { text: "фейк".into() },
                TrayItem::Action {
                    id: "go".into(),
                    text: "Поехали".into(),
                    cmd: "go".into(),
                    args: json!({ "n": 1 }),
                },
                TrayItem::Submenu {
                    text: "Ещё".into(),
                    items: vec![TrayItem::action("deep", "Глубже")],
                },
            ]
        }
        fn call(&self, _env: Env, name: String, args: Value) -> CallFut {
            let slow = self.slow;
            Box::pin(async move {
                if slow {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                Ok(json!({ "did": name, "args": args }))
            })
        }
    }

    fn builtin(id: &str, default_enabled: bool) -> Value {
        json!({
            "id": id, "name": id, "version": "1.0.0", "kind": "builtin",
            "defaultEnabled": default_enabled,
            "tray": true,
            "settings": [{ "key": "auto", "type": "toggle", "title": "Авто", "default": false }],
            "commands": [{ "name": "go", "title": "Поехали" }]
        })
    }

    fn external(id: &str) -> Value {
        json!({
            "id": id, "name": id, "version": "1.0.0", "kind": "external",
            "entry": { "path": "bin/plug" },
            "capabilities": ["read", "control", "admin"],
            "commands": [{ "name": "go" }]
        })
    }

    #[test]
    fn duplicate_id_rejected() {
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", false))).unwrap();
        assert!(host.register(Fake::new(builtin("a", false))).is_err());
    }

    #[test]
    fn init_starts_only_enabled_plugins() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let on = Fake::new(builtin("on", true));
        let off = Fake::new(builtin("off", false));
        host.register(on.clone()).unwrap();
        host.register(off.clone()).unwrap();
        host.init(&env);
        assert_eq!(on.starts.load(Ordering::SeqCst), 1);
        assert_eq!(off.starts.load(Ordering::SeqCst), 0);
        assert_eq!(host.find("on").unwrap().status(), Status::Running);
        assert_eq!(host.find("off").unwrap().status(), Status::Stopped);
    }

    #[test]
    fn toggle_persists_and_drives_runtime() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let p = Fake::new(builtin("a", false));
        host.register(p.clone()).unwrap();
        host.init(&env);

        assert_eq!(host.set_enabled(&env, "a", true)["ok"], json!(true));
        assert_eq!(p.starts.load(Ordering::SeqCst), 1);
        assert!(host.is_enabled(&env, p.manifest()), "настройка записана");

        // повторное включение не стартует второй раз
        host.set_enabled(&env, "a", true);
        assert_eq!(p.starts.load(Ordering::SeqCst), 1);

        host.set_enabled(&env, "a", false);
        assert_eq!(p.stops.load(Ordering::SeqCst), 1);
        assert!(!host.is_enabled(&env, p.manifest()));
    }

    #[test]
    fn external_gets_token_on_enable_and_loses_it_on_disable() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let p = Fake::new(external("ext"));
        host.register(p.clone()).unwrap();

        host.set_enabled(&env, "ext", true);
        assert_eq!(p.token.lock().unwrap().as_deref(), Some("token-ext"));
        let classes = env.token_of("ext").expect("токен выпущен");
        assert!(classes.contains(&RiskClass::Read) && classes.contains(&RiskClass::Control));
        assert!(!classes.contains(&RiskClass::Admin), "admin не выдаётся никогда");

        host.set_enabled(&env, "ext", false);
        assert!(env.token_of("ext").is_none(), "токен отозван вместе с выключением");
    }

    #[test]
    fn builtin_never_gets_a_token() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let p = Fake::new(builtin("a", false));
        host.register(p.clone()).unwrap();
        host.set_enabled(&env, "a", true);
        assert!(p.token.lock().unwrap().is_none(), "встроенный ходит в ядро напрямую");
        assert!(env.token_of("a").is_none());
    }

    #[test]
    fn failed_start_is_error_and_revokes_token() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let mut f = Fake::new(external("ext"));
        Arc::get_mut(&mut f).unwrap().fail_start = true;
        host.register(f).unwrap();

        let res = host.set_enabled(&env, "ext", true);
        assert_eq!(res["ok"], json!(false));
        assert_eq!(host.find("ext").unwrap().status(), Status::Error);
        assert!(env.token_of("ext").is_none(), "не стартовал — грант не остаётся");
    }

    #[tokio::test]
    async fn cmd_routes_to_runtime_and_checks_manifest() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", true))).unwrap();
        host.init(&env);

        let out = host.cmd(&env, "a", "go", json!({ "x": 1 })).await;
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["value"]["did"], json!("go"));

        let out = host.cmd(&env, "a", "выдумка", json!({})).await;
        assert_eq!(out["ok"], json!(false), "команды нет в манифесте");

        let out = host.cmd(&env, "a", "_status", json!({})).await;
        assert_eq!(out["ok"], json!(false), "внутренние команды не вызываются снаружи");

        let out = host.cmd(&env, "нет-такого", "go", json!({})).await;
        assert_eq!(out["ok"], json!(false));
    }

    #[tokio::test]
    async fn cmd_on_disabled_plugin_refuses() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", false))).unwrap();
        host.init(&env);
        let out = host.cmd(&env, "a", "go", json!({})).await;
        assert_eq!(out["error"], json!("плагин выключен"));
    }

    #[tokio::test]
    async fn slow_command_times_out_without_killing_plugin() {
        let env = Env::default();
        let host: PluginHost<Env> =
            PluginHost::new().with_call_timeout(Duration::from_millis(50));
        let mut f = Fake::new(builtin("a", true));
        Arc::get_mut(&mut f).unwrap().slow = true;
        host.register(f).unwrap();
        host.init(&env);

        let out = host.cmd(&env, "a", "go", json!({})).await;
        assert_eq!(out["ok"], json!(false));
        assert_eq!(host.find("a").unwrap().status(), Status::Running, "таймаут ≠ смерть");
    }

    #[test]
    fn enable_toggle_goes_through_cmd_too() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        let p = Fake::new(builtin("a", false));
        host.register(p.clone()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let out = rt.block_on(host.cmd(&env, "a", "_enable", json!({ "on": true })));
        assert_eq!(out["ok"], json!(true));
        assert_eq!(p.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn status_json_keeps_power_shape_and_adds_health() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", true))).unwrap();
        host.init(&env);
        let v = host.status_json(&env);
        let p = &v[0];
        // форма, на которую опирается вкладка «Бодрость»
        assert_eq!(p["id"], json!("a"));
        assert_eq!(p["enabled"], json!(true));
        assert_eq!(p["status"]["starts"], json!(1));
        // и новое от ядра
        assert_eq!(p["health"]["status"], json!("running"));
        assert_eq!(p["builtin"], json!(true));
        assert_eq!(p["settingsSchema"][0]["key"], json!("auto"));
        assert_eq!(p["settingsValues"]["auto"], json!(false));
    }

    #[test]
    fn settings_write_is_limited_to_declared_keys() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", true))).unwrap();
        assert_eq!(host.set_setting(&env, "a", "auto", json!(true))["ok"], json!(true));
        assert_eq!(host.set_setting(&env, "a", "чужое", json!(1))["ok"], json!(false));
        let vals = env.plugin_settings("a", json!({}));
        assert_eq!(vals["auto"], json!(true));
        assert!(vals.get("чужое").is_none());
    }

    #[test]
    fn tray_sections_are_prefixed_by_owner_and_routable() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", true))).unwrap();
        host.register(Fake::new(builtin("b", true))).unwrap();
        host.init(&env);
        let items = host.tray_items(&env);

        let mut actions = Vec::new();
        fn collect(items: &[TrayItem], out: &mut Vec<String>) {
            for i in items {
                match i {
                    TrayItem::Action { id, .. } | TrayItem::Check { id, .. } => out.push(id.clone()),
                    TrayItem::Submenu { items, .. } => collect(items, out),
                    _ => {}
                }
            }
        }
        collect(&items, &mut actions);
        assert!(actions.contains(&"plug:a:go".to_string()));
        assert!(actions.contains(&"plug:b:deep".to_string()), "вложенные тоже префиксуются");
        let (plugin, cmd, cmd_args) = host.route_tray("plug:a:go").expect("маршрут известен");
        assert_eq!((plugin.as_str(), cmd.as_str()), ("a", "go"));
        assert_eq!(cmd_args, json!({ "n": 1 }), "аргументы клика несёт сам пункт");
        assert!(host.route_tray("что-то-чужое").is_none());
    }

    #[test]
    fn disabled_plugin_contributes_no_tray() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register(Fake::new(builtin("a", false))).unwrap();
        host.init(&env);
        assert!(host.tray_items(&env).is_empty());
    }

    #[test]
    fn broken_manifest_is_visible_but_not_started() {
        let env = Env::default();
        let host: PluginHost<Env> = PluginHost::new();
        host.register_broken(Fake::new(builtin("bad", true)), "кривой JSON".into()).unwrap();
        host.init(&env);
        assert_eq!(host.find("bad").unwrap().status(), Status::Error);
        let v = host.status_json(&env);
        assert_eq!(v[0]["health"]["error"], json!("кривой JSON"));
        assert_eq!(host.set_enabled(&env, "bad", true)["ok"], json!(false));
    }
}
