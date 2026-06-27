//! Хост плагинов питания: «Не спать» (☕) и «Крышка» (⌒).
//!
//! Подключаемость = тумблер plugins.<id>.enabled в ~/.jarvis/settings.json
//! (дефолт: включён). Состояния обоих плагинов живут здесь; секции трея
//! отдаются декларативным списком, который tray.rs превращает в меню.
//!
//! Сон/пробуждение мака детектится по разрыву секундного тика (> 90с без
//! тиков = спали): ноль unsafe-кода, та же семантика suspend/resume.

pub mod assertion;
pub mod clamshell;
pub mod keep_awake;

use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::daemon::Daemon;
use crate::model::{Session, Status};
use crate::util::{jarvis_dir, now_ms, one_line};
use assertion::IopmBlocker;
use keep_awake::{Engine, Event};

const SUGGEST_GAP_MS: i64 = 60 * 60 * 1000; // подсказка не чаще раза в час
const GUARD_EVERY_MS: i64 = 60 * 1000;
const WAKE_GAP_MS: i64 = 90 * 1000;

/// Декларативный пункт меню трея от плагина.
pub enum TrayItem {
    Label { text: String },
    Action { id: String, text: String },
    Check { id: String, text: String, checked: bool, enabled: bool },
    Submenu { text: String, items: Vec<TrayItem> },
    Separator,
}

#[derive(Default)]
struct Clam {
    /// Плагин «Крышка» включён (runtime-аналог p.active у Electron-хоста).
    active: bool,
    armed: bool,
    armed_by: Option<&'static str>, // 'manual' | 'auto'
    busy: bool,                     // arm/disarm в полёте — не наслаиваем
    last_guard_at: i64,
    lid_causes_sleep: Option<bool>, // кэш для статусной строки меню
}

pub struct Power {
    /// Some = плагин «Не спать» включён.
    engine: Mutex<Option<Engine<IopmBlocker>>>,
    clam: Mutex<Clam>,
    /// Кэш кандидатов «пока жив процесс» для сабменю трея.
    processes: Mutex<Vec<(i64, String)>>,
    is_air: AtomicBool,
    last_tick_at: AtomicI64,
    last_working: AtomicUsize,
    last_suggest_at: AtomicI64,
    /// Тикер «ещё 47м»: при активном ручном таймере обновляем UI раз в 30с.
    last_countdown_at: AtomicI64,
}

impl Power {
    pub fn new() -> Self {
        Self {
            engine: Mutex::new(None),
            clam: Mutex::new(Clam::default()),
            processes: Mutex::new(Vec::new()),
            is_air: AtomicBool::new(false),
            last_tick_at: AtomicI64::new(0),
            last_working: AtomicUsize::new(0),
            last_suggest_at: AtomicI64::new(0),
            last_countdown_at: AtomicI64::new(0),
        }
    }

    fn ka_settings(d: &Arc<Daemon>) -> Value {
        d.settings.plugin(
            "keep-awake",
            json!({ "enabled": true, "auto": false, "keepDisplayOn": false }),
        )
    }

    fn cs_settings(d: &Arc<Daemon>) -> Value {
        d.settings.plugin(
            "clamshell",
            json!({ "enabled": true, "suggest": true, "autoArm": false, "batteryFloor": 15 }),
        )
    }

    /* ================= жизненный цикл ================= */

    pub fn init(d: &Arc<Daemon>) {
        let p = &d.power;
        p.last_tick_at.store(now_ms(), Ordering::SeqCst);

        // Оба движка грузим ВСЕГДА. «Выключено» теперь = ассерт/флаг не держится,
        // а не «плагин выгружен» — это убирает путаницу хост-слоя в настройках.
        // Бонус для безопасности: activate_clamshell внутри запускает
        // restore_after_restart, который снимает повисший с прошлой жизни
        // disablesleep. Раньше он не вызывался, если режим был выключен, —
        // и закрытие крышки оставалось залипшим, снять его было нечем.
        Self::activate_keep_awake(d);
        Self::activate_clamshell(d);
        Self::refresh_processes(d);
    }

    fn activate_keep_awake(d: &Arc<Daemon>) {
        let s = Self::ka_settings(d);
        let mut engine = Engine::new(
            IopmBlocker,
            s["auto"].as_bool().unwrap_or(false),
            s["keepDisplayOn"].as_bool().unwrap_or(false),
        );
        // демон мог рестартовать посреди работы — подхватываем текущее состояние
        let working = working_count(&d.snapshot());
        let events = engine.set_working(working, now_ms());
        *d.power.engine.lock().unwrap() = Some(engine);
        println!("[jarvis:keep-awake] включён");
        handle_engine_events(d, events); // assertion взялась сразу → связка с «Крышкой»
    }

    fn deactivate_keep_awake(d: &Arc<Daemon>) {
        if let Some(mut e) = d.power.engine.lock().unwrap().take() {
            e.dispose();
        }
        println!("[jarvis:keep-awake] выключен");
    }

    fn activate_clamshell(d: &Arc<Daemon>) {
        {
            let mut clam = d.power.clam.lock().unwrap();
            if clam.active {
                return; // уже включён — повторный _enable не дёргает restore
            }
            clam.active = true;
        }
        let d2 = d.clone();
        tauri::async_runtime::spawn(async move {
            d2.power
                .is_air
                .store(clamshell::detect_is_air().await, Ordering::SeqCst);
            restore_after_restart(&d2).await;
            refresh_lid(&d2).await;
        });
        println!("[jarvis:clamshell] включён");
    }

    fn deactivate_clamshell(d: &Arc<Daemon>) {
        let armed = {
            let mut clam = d.power.clam.lock().unwrap();
            if !clam.active {
                return;
            }
            clam.active = false;
            let was = clam.armed;
            clam.armed = false;
            clam.armed_by = None;
            was
        };
        if armed && clamshell::pmset_quiet_sync(false) {
            clamshell::clear_marker();
        }
        println!("[jarvis:clamshell] выключен");
    }

    /// Выход из приложения: снять assertion, вернуть disablesleep.
    /// Квит не ждёт промисов — восстанавливаем синхронно и только тихо;
    /// без sudoers ручной armed переживёт квит, его поднимет restoreAfterRestart.
    pub fn dispose(d: &Arc<Daemon>) {
        Self::deactivate_keep_awake(d);
        Self::deactivate_clamshell(d);
    }

    fn ka_enabled(&self) -> bool {
        self.engine.lock().unwrap().is_some()
    }

    /* ================= снапшот сессий → авто-триггер ================= */

    pub fn on_sessions(&self, d: &Arc<Daemon>, list: &[Session]) {
        let events = {
            let mut engine = self.engine.lock().unwrap();
            match engine.as_mut() {
                Some(e) => e.set_working(working_count(list), now_ms()),
                None => vec![],
            }
        };
        // сам push уже обновит трей/панель; нам остаётся связка с «Крышкой»
        if !events.is_empty() {
            peer_sync(d);
        }
    }

    /* ================= статусы для панели и трея ================= */

    /// Удерживается ли ассерт «не спать» прямо сейчас (для снапшота планировщика).
    pub fn keep_awake_active(&self) -> bool {
        self.engine.lock().unwrap().as_ref().is_some_and(|e| e.active())
    }

    pub fn badges(&self) -> String {
        let mut s = String::new();
        if self.engine.lock().unwrap().as_ref().is_some_and(|e| e.active()) {
            s.push('☕');
        }
        if self.clam.lock().unwrap().armed {
            s.push('⌒');
        }
        s
    }

    pub fn statuses(&self, d: &Arc<Daemon>) -> Value {
        let now = now_ms();
        let ka_status = {
            let engine = self.engine.lock().unwrap();
            engine.as_ref().map(|e| {
                let mut st = e.state();
                let line = keep_awake::status_line(&st, now);
                let obj = st.as_object_mut().unwrap();
                obj.insert("line".into(), line.map(Value::from).unwrap_or(Value::Null));
                obj.insert(
                    "keepDisplayOn".into(),
                    Self::ka_settings(d)["keepDisplayOn"].clone(),
                );
                st
            })
        };
        let cs_enabled = self.clam.lock().unwrap().active;
        let cs_status = if cs_enabled {
            let clam = self.clam.lock().unwrap();
            let s = Self::cs_settings(d);
            Some(json!({
                "armed": clam.armed,
                "armedBy": clam.armed_by,
                "autoArm": s["autoArm"],
                "suggest": s["suggest"],
                "batteryFloor": s["batteryFloor"],
                "sudoers": clamshell::sudoers_installed(),
            }))
        } else {
            None
        };
        json!([
            {
                "id": "keep-awake",
                "name": "Не спать",
                "enabled": ka_status.is_some(),
                "status": ka_status,
            },
            {
                "id": "clamshell",
                "name": "Крышка",
                "enabled": cs_enabled,
                "status": cs_status,
            },
        ])
    }

    /* ================= команды из панели и трея ================= */

    pub async fn cmd(d: &Arc<Daemon>, id: &str, name: &str, args: &Value) -> Value {
        if name == "_enable" {
            let on = args.get("on").and_then(Value::as_bool).unwrap_or(false);
            let mut patch = Map::new();
            patch.insert("enabled".into(), Value::Bool(on));
            d.settings.set_plugin(id, patch);
            match (id, on) {
                ("keep-awake", true) => {
                    if !d.power.ka_enabled() {
                        Self::activate_keep_awake(d);
                    }
                }
                ("keep-awake", false) => Self::deactivate_keep_awake(d),
                ("clamshell", true) => Self::activate_clamshell(d),
                ("clamshell", false) => Self::deactivate_clamshell(d),
                _ => return json!({ "ok": false, "error": "плагин не найден" }),
            }
            changed(d);
            return json!({ "ok": true });
        }
        let res = match id {
            "keep-awake" => Self::ka_cmd(d, name, args),
            "clamshell" => Self::cs_cmd(d, name, args).await,
            _ => json!({ "ok": false, "error": "плагин не найден" }),
        };
        changed(d);
        res
    }

    fn ka_cmd(d: &Arc<Daemon>, name: &str, args: &Value) -> Value {
        let now = now_ms();
        let events = {
            let mut guard = d.power.engine.lock().unwrap();
            let Some(engine) = guard.as_mut() else {
                return json!({ "ok": false, "error": "плагин выключен" });
            };
            match name {
                "start-manual" => engine.start_manual(None),
                "start-timer" => {
                    let minutes = args.get("minutes").and_then(Value::as_i64).unwrap_or(0).max(1);
                    engine.start_timer(minutes * 60_000, format!("{minutes}м"), now)
                }
                "start-process" => {
                    let pid = args.get("pid").and_then(Value::as_i64).unwrap_or(0);
                    if pid <= 0 {
                        return json!({ "ok": false, "error": "кривой pid" });
                    }
                    let label = args
                        .get("label")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .unwrap_or_else(|| pid.to_string());
                    engine.start_process(pid, label)
                }
                "stop" => engine.stop_manual(),
                "off" => {
                    // Настоящий master-off: гасим И ручной слот, И авто. Авто при
                    // этом фиксируем выключенным в настройках — иначе ближайший
                    // working снова поднимет ассерт, и «выключить» не сработает
                    // (ровно тот баг, на который жаловались).
                    let mut events = engine.set_auto(false, now);
                    events.extend(engine.stop_manual());
                    drop(guard);
                    let mut patch = Map::new();
                    patch.insert("auto".into(), Value::Bool(false));
                    d.settings.set_plugin("keep-awake", patch);
                    handle_engine_events(d, events);
                    return json!({ "ok": true });
                }
                "set" => {
                    let mut patch = Map::new();
                    let mut events = Vec::new();
                    if let Some(auto) = args.get("auto").and_then(Value::as_bool) {
                        patch.insert("auto".into(), Value::Bool(auto));
                        events.extend(engine.set_auto(auto, now));
                    }
                    if let Some(kd) = args.get("keepDisplayOn").and_then(Value::as_bool) {
                        patch.insert("keepDisplayOn".into(), Value::Bool(kd));
                        events.extend(engine.set_display_pref(kd));
                    }
                    if patch.is_empty() {
                        return json!({ "ok": false, "error": "пустой set" });
                    }
                    drop(guard);
                    d.settings.set_plugin("keep-awake", patch);
                    handle_engine_events(d, events);
                    return json!({ "ok": true });
                }
                _ => return json!({ "ok": false, "error": format!("неизвестная команда: {name}") }),
            }
        };
        handle_engine_events(d, events);
        json!({ "ok": true })
    }

    async fn cs_cmd(d: &Arc<Daemon>, name: &str, args: &Value) -> Value {
        if !d.power.clam.lock().unwrap().active {
            return json!({ "ok": false, "error": "плагин выключен" });
        }
        match name {
            "arm" => arm(d, "manual").await,
            "disarm" => disarm(d).await,
            "install-sudoers" => install_sudoers(d).await,
            "set" => {
                let mut patch = Map::new();
                if let Some(v) = args.get("autoArm").and_then(Value::as_bool) {
                    patch.insert("autoArm".into(), Value::Bool(v));
                }
                if let Some(v) = args.get("suggest").and_then(Value::as_bool) {
                    patch.insert("suggest".into(), Value::Bool(v));
                }
                if let Some(v) = args.get("batteryFloor").and_then(Value::as_f64) {
                    patch.insert("batteryFloor".into(), json!((v.floor() as i64).clamp(5, 80)));
                }
                if patch.is_empty() {
                    return json!({ "ok": false, "error": "пустой set" });
                }
                let auto_on = patch.get("autoArm") == Some(&Value::Bool(true));
                d.settings.set_plugin("clamshell", patch);
                if auto_on {
                    peer_sync(d); // авто включили — сразу синхронизируемся с keep-awake
                }
                json!({ "ok": true })
            }
            _ => json!({ "ok": false, "error": format!("неизвестная команда: {name}") }),
        }
    }

    /* ================= секции меню трея ================= */

    pub fn tray_items(&self, d: &Arc<Daemon>) -> Vec<TrayItem> {
        let now = now_ms();
        let mut out = Vec::new();

        if let Some(engine) = self.engine.lock().unwrap().as_ref() {
            let st = engine.state();
            let s = Self::ka_settings(d);
            let line = keep_awake::status_line(&st, now);
            out.push(TrayItem::Label {
                text: match line {
                    Some(l) => format!("☕ Не спать: {l}"),
                    None => "☕ Не спать: выкл".into(),
                },
            });
            out.push(TrayItem::Action { id: "ka:start-manual".into(), text: "Бессрочно".into() });
            out.push(TrayItem::Submenu {
                text: "На время".into(),
                items: keep_awake::PRESETS_MIN
                    .iter()
                    .map(|m| TrayItem::Action {
                        id: format!("ka:timer:{m}"),
                        text: keep_awake::preset_label(*m),
                    })
                    .collect(),
            });
            let procs = self.processes.lock().unwrap().clone();
            out.push(TrayItem::Submenu {
                text: "Пока жив процесс".into(),
                items: if procs.is_empty() {
                    vec![TrayItem::Label { text: "процессы не нашлись".into() }]
                } else {
                    procs
                        .iter()
                        .take(24)
                        .enumerate()
                        .map(|(i, (_, label))| TrayItem::Action {
                            id: format!("ka:proc:{i}"),
                            text: label.clone(),
                        })
                        .collect()
                },
            });
            if !st["manual"].is_null() {
                out.push(TrayItem::Action { id: "ka:stop".into(), text: "Выключить ручной режим".into() });
            }
            out.push(TrayItem::Separator);
            out.push(TrayItem::Check {
                id: "ka:set-auto".into(),
                text: "Пока агенты работают (авто)".into(),
                checked: s["auto"].as_bool().unwrap_or(false),
                enabled: true,
            });
            out.push(TrayItem::Check {
                id: "ka:set-display".into(),
                text: "Не гасить экран".into(),
                checked: s["keepDisplayOn"].as_bool().unwrap_or(false),
                enabled: true,
            });
        }

        let cs_active = self.clam.lock().unwrap().active;
        if cs_active {
            let (armed, lid_causes_sleep) = {
                let clam = self.clam.lock().unwrap();
                (clam.armed, clam.lid_causes_sleep)
            };
            let s = Self::cs_settings(d);
            let sudoers = clamshell::sudoers_installed();
            if !out.is_empty() {
                out.push(TrayItem::Separator);
            }
            out.push(TrayItem::Label {
                text: if armed {
                    "⌒ Крышка: мак не уснёт даже закрытой".into()
                } else if lid_causes_sleep == Some(false) {
                    "⌒ Крышка: закрытие сейчас не усыпляет".into()
                } else {
                    "⌒ Крышка: закроешь — уснёт".into()
                },
            });
            out.push(TrayItem::Check {
                id: "cs:toggle".into(),
                text: "Closed-display mode".into(),
                checked: armed,
                enabled: true,
            });
            out.push(TrayItem::Check {
                id: "cs:set-autoarm".into(),
                text: if sudoers {
                    "Авто при работе агентов".into()
                } else {
                    "Авто при работе агентов (нужен тихий режим)".into()
                },
                checked: s["autoArm"].as_bool().unwrap_or(false),
                enabled: sudoers,
            });
            out.push(TrayItem::Check {
                id: "cs:set-suggest".into(),
                text: "Подсказывать после прерванного сна".into(),
                checked: s["suggest"].as_bool().unwrap_or(false),
                enabled: true,
            });
            if !sudoers {
                out.push(TrayItem::Action {
                    id: "cs:install-sudoers".into(),
                    text: "Настроить тихий режим (sudoers)…".into(),
                });
            }
        }
        out
    }

    /// Клик по пункту меню трея из секций плагинов.
    pub fn handle_menu(d: &Arc<Daemon>, id: &str) -> bool {
        let d = d.clone();
        let id = id.to_string();
        let known = id.starts_with("ka:") || id.starts_with("cs:");
        if !known {
            return false;
        }
        tauri::async_runtime::spawn(async move {
            let ka = Self::ka_settings(&d);
            let cs = Self::cs_settings(&d);
            let armed = d.power.clam.lock().unwrap().armed;
            let (plugin, name, args): (&str, &str, Value) = match id.as_str() {
                "ka:start-manual" => ("keep-awake", "start-manual", json!({})),
                "ka:stop" => ("keep-awake", "stop", json!({})),
                "ka:set-auto" => (
                    "keep-awake", "set",
                    json!({ "auto": !ka["auto"].as_bool().unwrap_or(false) }),
                ),
                "ka:set-display" => (
                    "keep-awake", "set",
                    json!({ "keepDisplayOn": !ka["keepDisplayOn"].as_bool().unwrap_or(false) }),
                ),
                "cs:toggle" => ("clamshell", if armed { "disarm" } else { "arm" }, json!({})),
                "cs:set-autoarm" => (
                    "clamshell", "set",
                    json!({ "autoArm": !cs["autoArm"].as_bool().unwrap_or(false) }),
                ),
                "cs:set-suggest" => (
                    "clamshell", "set",
                    json!({ "suggest": !cs["suggest"].as_bool().unwrap_or(false) }),
                ),
                "cs:install-sudoers" => ("clamshell", "install-sudoers", json!({})),
                other => {
                    if let Some(min) = other.strip_prefix("ka:timer:") {
                        ("keep-awake", "start-timer", json!({ "minutes": min.parse::<i64>().unwrap_or(15) }))
                    } else if let Some(idx) = other.strip_prefix("ka:proc:") {
                        let procs = d.power.processes.lock().unwrap().clone();
                        match idx.parse::<usize>().ok().and_then(|i| procs.get(i).cloned()) {
                            Some((pid, label)) => (
                                "keep-awake", "start-process",
                                json!({ "pid": pid, "label": label }),
                            ),
                            None => return,
                        }
                    } else {
                        return;
                    }
                }
            };
            Power::cmd(&d, plugin, name, &args).await;
        });
        true
    }

    /* ================= секундный тик ================= */

    pub async fn tick(d: &Arc<Daemon>) {
        let now = now_ms();
        let p = &d.power;
        let prev_tick = p.last_tick_at.swap(now, Ordering::SeqCst);

        // движок: таймеры, линджер, пульс процесса
        let (events, timer_running) = {
            let mut engine = p.engine.lock().unwrap();
            match engine.as_mut() {
                Some(e) => {
                    let events = e.tick(now);
                    let timer = e.state()["manual"]["kind"] == "timer";
                    (events, timer)
                }
                None => (vec![], false),
            }
        };
        handle_engine_events(d, events);

        // обратный отсчёт «ещё 47м» в трее/панели — раз в 30с, пока идёт таймер
        if timer_running && now - p.last_countdown_at.load(Ordering::SeqCst) >= 30_000 {
            p.last_countdown_at.store(now, Ordering::SeqCst);
            changed(d);
        }

        // пробуждение после сна: тиков не было дольше WAKE_GAP_MS
        if prev_tick > 0 && now - prev_tick > WAKE_GAP_MS {
            on_resume(d, p.last_working.load(Ordering::SeqCst)).await;
        }
        p.last_working
            .store(working_count(&d.snapshot()), Ordering::SeqCst);

        // батарейный сторож «Крышки»
        let needs_guard = {
            let mut clam = p.clam.lock().unwrap();
            if clam.armed && now - clam.last_guard_at >= GUARD_EVERY_MS {
                clam.last_guard_at = now;
                true
            } else {
                false
            }
        };
        if needs_guard {
            battery_guard(d).await;
        }
    }

    /// Освежить данные для меню трея: кандидатов «пока жив процесс» и
    /// состояние крышки (Electron собирал их в момент right-click; у Tauri
    /// меню статичное — обновляем на клик, меню пересобирается через changed).
    pub fn refresh_processes(d: &Arc<Daemon>) {
        let d = d.clone();
        tauri::async_runtime::spawn(async move {
            let mut dirty = false;
            if d.power.clam.lock().unwrap().active {
                refresh_lid(&d).await;
                dirty = true;
            }
            if d.power.ka_enabled() {
                let procs = list_processes(&d).await;
                *d.power.processes.lock().unwrap() = procs;
                dirty = true;
            }
            if dirty {
                changed(&d); // сигнатура меню изменилась → пересборка
            }
        });
    }
}

fn working_count(list: &[Session]) -> usize {
    list.iter().filter(|s| s.status == Status::Working).count()
}

/// Трей/панель обновить (аналог ctx.changed() БЕЗ broadcast: связка с
/// «Крышкой» дёргается только из событий keep-awake, иначе ручной disarm
/// крышки мгновенно ре-армился бы peer_sync'ом).
fn changed(d: &Arc<Daemon>) {
    crate::tray::update(d, &d.snapshot());
    crate::windows::emit_to_panel(&d.app, "plugins", &d.power.statuses(d));
}

fn handle_engine_events(d: &Arc<Daemon>, events: Vec<Event>) {
    for e in &events {
        match e {
            Event::TimerEnd => {
                d.notify("☕ Таймер вышел", "Мак снова может спать как обычно", None, "done");
            }
            Event::ProcessDied { label } => {
                d.notify("☕ Снимаю запрет сна", &format!("{label} завершился"), None, "done");
            }
            Event::Changed => {}
        }
    }
    if !events.is_empty() {
        changed(d);
        peer_sync(d); // источник — keep-awake: «Крышке» можно следовать
    }
}

/// Связка clamshell ↔ keep-awake: авто-режим «Крышки» повторяет assertion
/// (нужен sudoers — admin-диалог из фона недопустим).
fn peer_sync(d: &Arc<Daemon>) {
    let s = Power::cs_settings(d);
    let (active, busy, armed, armed_by) = {
        let clam = d.power.clam.lock().unwrap();
        (clam.active, clam.busy, clam.armed, clam.armed_by)
    };
    if !active || busy || s["autoArm"].as_bool() != Some(true) || !clamshell::sudoers_installed() {
        return;
    }
    let ka_active = d.power.engine.lock().unwrap().as_ref().is_some_and(|e| e.active());
    let d = d.clone();
    tauri::async_runtime::spawn(async move {
        if ka_active && !armed {
            arm(&d, "auto").await;
        } else if !ka_active && armed && armed_by == Some("auto") {
            disarm(&d).await;
        }
    });
}

async fn arm(d: &Arc<Daemon>, by: &'static str) -> Value {
    {
        let mut clam = d.power.clam.lock().unwrap();
        if clam.busy {
            return json!({ "ok": false, "error": "операция уже идёт" });
        }
        if clam.armed {
            return json!({ "ok": true });
        }
        clam.busy = true;
    }
    let ok = if by == "auto" {
        clamshell::pmset_quiet(true).await
    } else {
        clamshell::pmset_ask(true).await
    };
    let result = if ok {
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.armed = true;
            clam.armed_by = Some(by);
            clam.last_guard_at = 0;
        }
        clamshell::write_marker(by);
        if by == "manual" && !clamshell::sudoers_installed() {
            d.notify(
                "⌒ Closed-display включён",
                "Не забудь выключить: без тихого режима я не смогу снять его сам",
                None,
                "done",
            );
        }
        changed(d);
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "не получилось включить (пароль отменён?)" })
    };
    d.power.clam.lock().unwrap().busy = false;
    result
}

async fn disarm(d: &Arc<Daemon>) -> Value {
    {
        let mut clam = d.power.clam.lock().unwrap();
        if clam.busy {
            return json!({ "ok": false, "error": "операция уже идёт" });
        }
        if !clam.armed {
            return json!({ "ok": true });
        }
        clam.busy = true;
    }
    let ok = clamshell::pmset_ask(false).await;
    let result = if ok {
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.armed = false;
            clam.armed_by = None;
        }
        clamshell::clear_marker();
        changed(d);
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "не получилось выключить" })
    };
    d.power.clam.lock().unwrap().busy = false;
    result
}

/// Подвисший с прошлой жизни демона disablesleep — вернуть как было.
async fn restore_after_restart(d: &Arc<Daemon>) {
    if clamshell::read_marker().is_none() {
        return;
    }
    if clamshell::read_sleep_disabled().await != Some(true) {
        clamshell::clear_marker();
        return;
    }
    if clamshell::pmset_quiet(false).await {
        clamshell::clear_marker();
        println!("[jarvis:clamshell] демон перезапустился с поднятым disablesleep — восстановил нормальный сон");
        return;
    }
    // Тихо снять нельзя (нет sudoers). Усыновляем повисший флаг как armed —
    // тогда панель и трей честно покажут «не уснёт закрытым» и дадут кнопку
    // «Выключить» (спросит пароль). Иначе UI врал бы «норм. сон», а мак не спал
    // и снять его было бы нечем — ровно тот тупик, из-за которого «не отключается».
    {
        let mut clam = d.power.clam.lock().unwrap();
        clam.armed = true;
        clam.armed_by = Some("manual");
        clam.last_guard_at = 0;
    }
    changed(d);
    d.notify(
        "⌒ Мак не спит с прошлого запуска",
        "Остался запрет сна под крышкой — нажми «Выключить» в ◇ → Крышка (спросит пароль)",
        None,
        "done",
    );
}

async fn refresh_lid(d: &Arc<Daemon>) {
    let lid = clamshell::read_lid().await;
    d.power.clam.lock().unwrap().lid_causes_sleep = lid.causes_sleep;
}

/// Батарейный сторож: armed + батарея ≤ floor → тихий сброс или форс-сон.
async fn battery_guard(d: &Arc<Daemon>) {
    let floor = Power::cs_settings(d)["batteryFloor"].as_i64().unwrap_or(15) as u32;
    let batt = clamshell::read_battery().await;
    let (Some(pct), Some(true)) = (batt.pct, batt.on_battery) else { return };
    if pct > floor {
        return;
    }
    println!("[jarvis:clamshell] батарея {pct}% ≤ {floor}% — снимаю disablesleep");
    if clamshell::pmset_quiet(false).await {
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.armed = false;
            clam.armed_by = None;
        }
        clamshell::clear_marker();
        d.notify(
            "⌒ Крышка: батарея садится",
            &format!("Осталось {pct}% — вернул нормальный сон"),
            None,
            "done",
        );
        changed(d);
    } else {
        // тихо не получилось, диалог под закрытой крышкой бессмыслен —
        // форс-сон (root не нужен) спасает батарею и температуру
        d.notify(
            "⌒ Крышка: батарея садится",
            &format!("Осталось {pct}% — усыпляю мак"),
            None,
            "done",
        );
        clamshell::force_sleep_now().await;
    }
}

/// Проснулись после сна, который прервал работу → подсказка про closed-display.
async fn on_resume(d: &Arc<Daemon>, working_at_sleep: usize) {
    refresh_lid(d).await;
    let (active, armed) = {
        let clam = d.power.clam.lock().unwrap();
        (clam.active, clam.armed)
    };
    if !active || Power::cs_settings(d)["suggest"].as_bool() != Some(true) {
        return;
    }
    let now = now_ms();
    let decision = clamshell::decide_suggest(
        working_at_sleep,
        armed,
        clamshell::external_display_present(),
        d.power.last_suggest_at.load(Ordering::SeqCst),
        now,
        SUGGEST_GAP_MS,
    );
    if decision == clamshell::Suggest::No {
        return;
    }
    d.power.last_suggest_at.store(now, Ordering::SeqCst);
    let n = working_at_sleep;
    let head = format!(
        "Сон прервал {n} {}",
        if n == 1 { "работающую сессию" } else { "работающие сессии" }
    );
    match decision {
        clamshell::Suggest::Native => d.notify(
            &head,
            "Есть внешний дисплей: держи мак на питании — родной clamshell-режим не даст ему уснуть с закрытой крышкой",
            None,
            "done",
        ),
        _ => d.notify(
            &head,
            &format!(
                "Включи closed-display mode (меню ◇ → Крышка), чтобы мак не засыпал под крышкой{}",
                if d.power.is_air.load(Ordering::SeqCst) {
                    ". Air без вентилятора — под крышкой возможен троттлинг"
                } else {
                    ""
                }
            ),
            None,
            "done",
        ),
    };
}

/// Установка sudoers-правила: visudo -c валидирует ДО установки;
/// всё одним admin-скриптом = один пароль.
async fn install_sudoers(d: &Arc<Daemon>) -> Value {
    let user = std::env::var("USER").unwrap_or_default();
    let content = match clamshell::sudoers_content(&user) {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let tmp = jarvis_dir().join("sudoers-pmset");
    let _ = std::fs::create_dir_all(jarvis_dir());
    if std::fs::write(&tmp, content).is_err() {
        return json!({ "ok": false, "error": "не смог записать временный файл" });
    }
    let tmp_str = tmp.to_string_lossy();
    let script = format!(
        "do shell script \"/usr/sbin/visudo -c -q -f '{tmp_str}' && /usr/bin/install -m 0440 -o root -g wheel '{tmp_str}' '{}'\" with administrator privileges with prompt \"Jarvis настраивает тихое переключение closed-display mode\"",
        clamshell::SUDOERS,
    );
    let ok = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::process::Command::new("osascript").args(["-e", &script]).output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some_and(|o| o.status.success());
    let _ = std::fs::remove_file(&tmp);
    if !ok {
        return json!({ "ok": false, "error": "установка отменена" });
    }
    d.notify("⌒ Тихий режим настроен", "Теперь closed-display переключается без пароля", None, "done");
    changed(d);
    json!({ "ok": true })
}

/// Кандидаты «пока жив процесс»: claude-сессии Jarvis + GUI-приложения.
/// GUI — два ОТДЕЛЬНЫХ AppleScript-вызова, как у Raycast Coffee: несколько
/// -e в одном osascript — это один скрипт, печатается только последний результат.
async fn list_processes(d: &Arc<Daemon>) -> Vec<(i64, String)> {
    let mut own = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in d.snapshot() {
        if let Some(pid) = s.pid {
            if seen.insert(pid) {
                own.push((
                    pid,
                    format!("claude · {}", s.project.as_deref().unwrap_or("?")),
                ));
            }
        }
    }
    let osa = |line: &'static str| async move {
        let out = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            tokio::process::Command::new("osascript").args(["-e", line]).output(),
        )
        .await
        .ok()?
        .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let (ids_line, names_line) = tokio::join!(
        osa("tell application \"System Events\" to get the unix id of every process whose background only is false"),
        osa("tell application \"System Events\" to get the name of every process whose background only is false"),
    );
    // нет пермишена Automation — покажем хотя бы claude-сессии
    if let (Some(ids_line), Some(names_line)) = (ids_line, names_line) {
        let ids: Vec<i64> = ids_line.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let names: Vec<&str> = names_line.split(',').map(str::trim).collect();
        let me = std::process::id() as i64;
        let mut apps: Vec<(i64, String)> = ids
            .iter()
            .zip(names.iter())
            .filter(|(pid, name)| **pid != me && !seen.contains(*pid) && !name.is_empty())
            .map(|(pid, name)| (*pid, one_line(name)))
            .collect();
        apps.sort_by_key(|(_, name)| name.to_lowercase()); // ≈ localeCompare('ru')
        own.extend(apps);
    }
    own
}
