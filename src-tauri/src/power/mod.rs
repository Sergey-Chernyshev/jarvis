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
pub(crate) mod helper;
pub mod keep_awake;
pub mod ownership;
pub mod ownership_store;

use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::model::{Session, Status};
use crate::util::{jarvis_dir, now_ms, one_line};
use assertion::IopmBlocker;
use keep_awake::{Engine, Event};

const SUGGEST_GAP_MS: i64 = 60 * 60 * 1000; // подсказка не чаще раза в час
const GUARD_EVERY_MS: i64 = 60 * 1000;
const WAKE_GAP_MS: i64 = 90 * 1000;
const CLAMSHELL_LEASE_TTL_MS: i64 = 5 * 60 * 1000;
const POWER_OPERATION_BARRIER_TIMEOUT: Duration = Duration::from_secs(90);
const POWER_REPAIR_ACTION: &str = "Открой раздел Power и запусти явный repair";
static NEXT_OWNER_GENERATION: AtomicU64 = AtomicU64::new(0);
static STARTUP_RECOVERY_HEALTH: OnceLock<RwLock<StartupRecoveryHealth>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StartupRecoveryHealth {
    #[default]
    NotChecked,
    Ready(clamshell::RecoveryOutcome),
    Blocked { message: String },
}

impl StartupRecoveryHealth {
    pub fn summary(&self) -> String {
        match self {
            Self::NotChecked => "not checked".into(),
            Self::Ready(outcome) => format!("ready: {outcome:?}"),
            Self::Blocked { message } => format!("blocked: {message}"),
        }
    }
}

fn recovery_health_cell() -> &'static RwLock<StartupRecoveryHealth> {
    STARTUP_RECOVERY_HEALTH.get_or_init(|| RwLock::new(StartupRecoveryHealth::NotChecked))
}

pub fn startup_recovery_health() -> StartupRecoveryHealth {
    recovery_health_cell()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn store_startup_recovery_health(health: StartupRecoveryHealth) {
    *recovery_health_cell()
        .write()
        .unwrap_or_else(|error| error.into_inner()) = health;
}

fn health_from_recovery(
    result: Result<clamshell::RecoveryOutcome, clamshell::PowerError>,
) -> StartupRecoveryHealth {
    match result {
        Ok(clamshell::RecoveryOutcome::BlockedExpiredLiveLease) => {
            StartupRecoveryHealth::Blocked {
                message: "expired ownership lease still matches a live process; renewal helper is not active yet".into(),
            }
        }
        Ok(clamshell::RecoveryOutcome::BlockedLegacyRepair) => StartupRecoveryHealth::Blocked {
            message: "legacy clamshell marker has no trustworthy baseline".into(),
        },
        Ok(outcome) => StartupRecoveryHealth::Ready(outcome),
        Err(error) => StartupRecoveryHealth::Blocked {
            message: error.to_string(),
        },
    }
}

/// Synchronous and bounded startup recovery. It never propagates failure into
/// Tauri setup: a blocked result is persisted and later arm attempts fail
/// closed with an explicit repair action.
pub fn recover_on_startup() -> StartupRecoveryHealth {
    let result = match clamshell::legacy_marker_state() {
        clamshell::LegacyMarkerState::Missing => clamshell::recover_with(
            &clamshell::SystemPmset,
            &ownership_store::OwnershipStore::global(),
            &clamshell::SystemProcesses,
            now_ms(),
        ),
        clamshell::LegacyMarkerState::Present(_) => {
            Ok(clamshell::RecoveryOutcome::BlockedLegacyRepair)
        }
        clamshell::LegacyMarkerState::Corrupt => Err(clamshell::PowerError::InvalidState(
            "legacy clamshell marker is corrupt and requires manual repair".into(),
        )),
    };
    let health = health_from_recovery(result);
    store_startup_recovery_health(health.clone());
    health
}

fn arm_recovery_error(health: &StartupRecoveryHealth) -> Option<String> {
    match health {
        StartupRecoveryHealth::Ready(_) => None,
        StartupRecoveryHealth::NotChecked => Some(format!(
            "startup power recovery was not checked; {POWER_REPAIR_ACTION}"
        )),
        StartupRecoveryHealth::Blocked { message } => {
            Some(format!("{message}; {POWER_REPAIR_ACTION}"))
        }
    }
}

fn recovery_health_json() -> Value {
    match startup_recovery_health() {
        StartupRecoveryHealth::Ready(outcome) => json!({
            "state": "ready",
            "outcome": format!("{outcome:?}"),
        }),
        StartupRecoveryHealth::NotChecked => json!({
            "state": "blocked",
            "message": "startup power recovery was not checked",
            "repairAction": POWER_REPAIR_ACTION,
        }),
        StartupRecoveryHealth::Blocked { message } => json!({
            "state": "blocked",
            "message": message,
            "repairAction": POWER_REPAIR_ACTION,
        }),
    }
}

#[derive(Default)]
struct ShutdownGate(AtomicBool);

impl ShutdownGate {
    fn accepting(&self) -> bool {
        !self.0.load(Ordering::Acquire)
    }

    fn close(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }
}

#[derive(Default)]
struct OperationBarrier {
    active: Mutex<bool>,
    idle: Condvar,
}

impl OperationBarrier {
    fn try_enter(self: &Arc<Self>, epoch: u64) -> Option<PowerOperation> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *active {
            return None;
        }
        *active = true;
        Some(PowerOperation {
            epoch,
            barrier: self.clone(),
        })
    }

    fn wait_for_idle(&self, timeout: Duration) -> bool {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*active {
            return true;
        }
        let waited = self
            .idle
            .wait_timeout_while(active, timeout, |active| *active);
        match waited {
            Ok((active, _)) => !*active,
            Err(_) => false,
        }
    }
}

struct PowerOperation {
    epoch: u64,
    barrier: Arc<OperationBarrier>,
}

impl PowerOperation {
    fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for PowerOperation {
    fn drop(&mut self) {
        *self
            .barrier
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
        self.barrier.idle.notify_all();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AcquireDisposition {
    Committed,
    RolledBack,
}

#[derive(Default)]
struct PowerOperations {
    gate: ShutdownGate,
    epoch: AtomicU64,
    barrier: Arc<OperationBarrier>,
}

impl PowerOperations {
    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    fn admitted_epoch(&self) -> Option<u64> {
        let epoch = self.epoch();
        (self.gate.accepting() && self.epoch() == epoch).then_some(epoch)
    }

    fn accepts(&self, epoch: u64) -> bool {
        self.gate.accepting() && self.epoch() == epoch
    }

    fn begin(&self) -> Option<PowerOperation> {
        let epoch = self.admitted_epoch()?;
        let operation = self.barrier.try_enter(epoch)?;
        if self.accepts(epoch) {
            Some(operation)
        } else {
            None
        }
    }

    fn begin_wait(&self, timeout: Duration) -> Option<PowerOperation> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(operation) = self.begin() {
                return Some(operation);
            }
            if !self.gate.accepting() {
                return None;
            }
            let now = std::time::Instant::now();
            if now >= deadline || !self.barrier.wait_for_idle(deadline - now) {
                return None;
            }
        }
    }

    fn accepting(&self) -> bool {
        self.gate.accepting()
    }

    fn advance(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn close(&self) -> bool {
        if !self.gate.close() {
            return false;
        }
        self.advance();
        true
    }

    fn wait_for_idle(&self, timeout: Duration) -> bool {
        self.barrier.wait_for_idle(timeout)
    }

    fn finish_acquire<T, E>(
        &self,
        operation: PowerOperation,
        acquired: Result<T, E>,
        commit: impl FnOnce(T),
        rollback: impl FnOnce() -> Result<(), E>,
    ) -> Result<AcquireDisposition, E> {
        let acquired = acquired?;
        let disposition = if self.accepts(operation.epoch()) {
            commit(acquired);
            AcquireDisposition::Committed
        } else {
            rollback()?;
            AcquireDisposition::RolledBack
        };
        drop(operation);
        Ok(disposition)
    }
}

/// Декларативный пункт меню трея от плагина.
pub enum TrayItem {
    Label {
        text: String,
    },
    Action {
        id: String,
        text: String,
    },
    Check {
        id: String,
        text: String,
        checked: bool,
        enabled: bool,
    },
    Submenu {
        text: String,
        items: Vec<TrayItem>,
    },
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClamshellDisposeOutcome {
    Idle,
    Released(clamshell::ReleaseOutcome),
    BarrierTimeout,
    ReleaseFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerDisposeReport {
    pub clamshell: ClamshellDisposeOutcome,
}

impl PowerDisposeReport {
    pub fn released(&self) -> bool {
        match self.clamshell {
            ClamshellDisposeOutcome::Idle => true,
            ClamshellDisposeOutcome::Released(outcome) => release_was_confirmed(outcome),
            ClamshellDisposeOutcome::BarrierTimeout | ClamshellDisposeOutcome::ReleaseFailed(_) => {
                false
            }
        }
    }
}

fn release_was_confirmed(outcome: clamshell::ReleaseOutcome) -> bool {
    !matches!(outcome, clamshell::ReleaseOutcome::NotOwned)
}

fn release_resolves_obligation(
    obligation: clamshell::AcquireObligation,
    outcome: clamshell::ReleaseOutcome,
) -> bool {
    match obligation {
        clamshell::AcquireObligation::None | clamshell::AcquireObligation::LeaseMayExist => true,
        clamshell::AcquireObligation::MutationMayRemain => release_was_confirmed(outcome),
    }
}

fn battery_release_confirms_normal_sleep(outcome: clamshell::ReleaseOutcome) -> bool {
    outcome.sleep_disabled() == Some(false)
}

fn failed_acquire_needs_owner_retry(
    failure: &clamshell::AcquireFailure,
    cleanup: &Result<clamshell::ReleaseOutcome, clamshell::PowerError>,
) -> bool {
    match failure.obligation {
        clamshell::AcquireObligation::None => false,
        clamshell::AcquireObligation::LeaseMayExist => cleanup.is_err(),
        clamshell::AcquireObligation::MutationMayRemain => {
            !matches!(cleanup, Ok(outcome) if release_was_confirmed(*outcome))
        }
    }
}

#[derive(Default)]
struct Clam {
    /// Плагин «Крышка» включён (runtime-аналог p.active у Electron-хоста).
    active: bool,
    armed: bool,
    armed_by: Option<&'static str>, // 'manual' | 'auto'
    owner_generation: Option<String>,
    owner_obligation: Option<clamshell::AcquireObligation>,
    busy: bool, // arm/disarm в полёте — не наслаиваем
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
    operations: PowerOperations,
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
            operations: PowerOperations::default(),
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
        // Durable clamshell recovery уже синхронно завершился до Daemon::new,
        // включая headless startup; runtime activation больше не мутирует
        // legacy marker или pmset по недоказанному ownership.
        Self::activate_keep_awake(d);
        Self::activate_clamshell(d);
        Self::refresh_processes(d);
    }

    fn activate_keep_awake(d: &Arc<Daemon>) {
        if !d.power.operations.accepting() {
            return;
        }
        let s = Self::ka_settings(d);
        let mut engine = Engine::new(
            IopmBlocker,
            s["auto"].as_bool().unwrap_or(false),
            s["keepDisplayOn"].as_bool().unwrap_or(false),
        );
        // демон мог рестартовать посреди работы — подхватываем текущее состояние
        let working = working_count(&d.snapshot());
        let events = engine.set_working(working, now_ms());
        let mut slot = d.power.engine.lock().unwrap();
        if !d.power.operations.accepting() || slot.is_some() {
            engine.dispose();
            return;
        }
        *slot = Some(engine);
        drop(slot);
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
        if !d.power.operations.accepting() {
            return;
        }
        {
            let mut clam = d.power.clam.lock().unwrap();
            if !d.power.operations.accepting() || clam.active {
                return; // уже включён — повторный _enable не дёргает restore
            }
            clam.active = true;
        }
        let d2 = d.clone();
        tauri::async_runtime::spawn(async move {
            if !d2.power.operations.accepting() {
                return;
            }
            d2.power
                .is_air
                .store(clamshell::detect_is_air().await, Ordering::SeqCst);
            if !d2.power.operations.accepting() {
                return;
            }
            refresh_lid(&d2).await;
        });
        println!("[jarvis:clamshell] включён");
    }

    fn deactivate_clamshell(d: &Arc<Daemon>) {
        let Some(_operation) = d
            .power
            .operations
            .begin_wait(POWER_OPERATION_BARRIER_TIMEOUT)
        else {
            eprintln!("[jarvis:clamshell] release admission timed out or shutdown started");
            return;
        };
        let _ = Self::deactivate_clamshell_inner(d);
    }

    fn deactivate_clamshell_inner(d: &Arc<Daemon>) -> ClamshellDisposeOutcome {
        let (was_active, owner_generation, owner_obligation) = {
            let mut clam = d.power.clam.lock().unwrap();
            let was_active = clam.active;
            if !was_active && clam.owner_generation.is_none() {
                return ClamshellDisposeOutcome::Idle;
            }
            clam.active = false;
            (
                was_active,
                clam.owner_generation.clone(),
                clam.owner_obligation,
            )
        };
        let mut outcome = ClamshellDisposeOutcome::Idle;
        if let Some(owner_generation) = owner_generation {
            let obligation =
                owner_obligation.unwrap_or(clamshell::AcquireObligation::MutationMayRemain);
            match clamshell::release_with(
                &clamshell::SystemPmset,
                &crate::power::ownership_store::OwnershipStore::global(),
                &power_profile_id(),
                &owner_generation,
            ) {
                Ok(release) => {
                    if release_resolves_obligation(obligation, release) {
                        let mut clam = d.power.clam.lock().unwrap();
                        if clam.owner_generation.as_deref() == Some(owner_generation.as_str()) {
                            clam.armed = false;
                            clam.armed_by = None;
                            clam.owner_generation = None;
                            clam.owner_obligation = None;
                        }
                        outcome = if release == clamshell::ReleaseOutcome::NotOwned {
                            ClamshellDisposeOutcome::Idle
                        } else {
                            ClamshellDisposeOutcome::Released(release)
                        };
                    } else {
                        let error =
                            "ownership registry no longer proves the in-memory clamshell lease";
                        eprintln!("[jarvis:clamshell] release on deactivate ambiguous: {error}");
                        outcome = ClamshellDisposeOutcome::ReleaseFailed(error.into());
                    }
                }
                Err(error) => {
                    // Keep the exact identity for a later shutdown/startup
                    // retry; losing it could strand SleepDisabled=1.
                    eprintln!("[jarvis:clamshell] release on deactivate failed: {error}");
                    outcome = ClamshellDisposeOutcome::ReleaseFailed(error.to_string());
                }
            }
        }
        if was_active {
            println!("[jarvis:clamshell] выключен");
        }
        outcome
    }

    /// Выход из приложения: снять assertion и синхронно освободить только
    /// доказанную Jarvis-owned clamshell lease через non-interactive backend.
    pub fn dispose(d: &Arc<Daemon>) -> PowerDisposeReport {
        d.power.operations.close();
        // IOKit assertions are process-local and cheap to release, so do this
        // before waiting for a potentially blocked cross-process transaction.
        Self::deactivate_keep_awake(d);
        let clamshell = if d
            .power
            .operations
            .wait_for_idle(POWER_OPERATION_BARRIER_TIMEOUT)
        {
            // Admission is closed, so no new operation can race this retry.
            Self::deactivate_clamshell_inner(d)
        } else {
            eprintln!("[jarvis:power] timed out waiting for in-flight clamshell rollback");
            ClamshellDisposeOutcome::BarrierTimeout
        };
        PowerDisposeReport { clamshell }
    }

    fn ka_enabled(&self) -> bool {
        self.engine.lock().unwrap().is_some()
    }

    /* ================= снапшот сессий → авто-триггер ================= */

    pub fn on_sessions(&self, d: &Arc<Daemon>, list: &[Session]) {
        if !self.operations.accepting() {
            return;
        }
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
        self.engine
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|e| e.active())
    }

    pub fn badges(&self) -> String {
        let mut s = String::new();
        if self
            .engine
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|e| e.active())
        {
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
                "health": recovery_health_json(),
                "status": cs_status,
            },
        ])
    }

    /* ================= команды из панели и трея ================= */

    pub async fn cmd(d: &Arc<Daemon>, id: &str, name: &str, args: &Value) -> Value {
        if !d.power.operations.accepting() {
            return json!({ "ok": false, "error": "Jarvis завершает работу" });
        }
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
                    let minutes = args
                        .get("minutes")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        .max(1);
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
                    patch.insert(
                        "batteryFloor".into(),
                        json!((v.floor() as i64).clamp(5, 80)),
                    );
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
            out.push(TrayItem::Action {
                id: "ka:start-manual".into(),
                text: "Бессрочно".into(),
            });
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
                    vec![TrayItem::Label {
                        text: "процессы не нашлись".into(),
                    }]
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
                out.push(TrayItem::Action {
                    id: "ka:stop".into(),
                    text: "Выключить ручной режим".into(),
                });
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
        if !d.power.operations.accepting() {
            return true;
        }
        tauri::async_runtime::spawn(async move {
            if !d.power.operations.accepting() {
                return;
            }
            let ka = Self::ka_settings(&d);
            let cs = Self::cs_settings(&d);
            let armed = d.power.clam.lock().unwrap().armed;
            let (plugin, name, args): (&str, &str, Value) = match id.as_str() {
                "ka:start-manual" => ("keep-awake", "start-manual", json!({})),
                "ka:stop" => ("keep-awake", "stop", json!({})),
                "ka:set-auto" => (
                    "keep-awake",
                    "set",
                    json!({ "auto": !ka["auto"].as_bool().unwrap_or(false) }),
                ),
                "ka:set-display" => (
                    "keep-awake",
                    "set",
                    json!({ "keepDisplayOn": !ka["keepDisplayOn"].as_bool().unwrap_or(false) }),
                ),
                "cs:toggle" => ("clamshell", if armed { "disarm" } else { "arm" }, json!({})),
                "cs:set-autoarm" => (
                    "clamshell",
                    "set",
                    json!({ "autoArm": !cs["autoArm"].as_bool().unwrap_or(false) }),
                ),
                "cs:set-suggest" => (
                    "clamshell",
                    "set",
                    json!({ "suggest": !cs["suggest"].as_bool().unwrap_or(false) }),
                ),
                "cs:install-sudoers" => ("clamshell", "install-sudoers", json!({})),
                other => {
                    if let Some(min) = other.strip_prefix("ka:timer:") {
                        (
                            "keep-awake",
                            "start-timer",
                            json!({ "minutes": min.parse::<i64>().unwrap_or(15) }),
                        )
                    } else if let Some(idx) = other.strip_prefix("ka:proc:") {
                        let procs = d.power.processes.lock().unwrap().clone();
                        match idx
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| procs.get(i).cloned())
                        {
                            Some((pid, label)) => (
                                "keep-awake",
                                "start-process",
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
        if !d.power.operations.accepting() {
            return;
        }
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
        if !d.power.operations.accepting() {
            return;
        }
        let d = d.clone();
        tauri::async_runtime::spawn(async move {
            if !d.power.operations.accepting() {
                return;
            }
            let mut dirty = false;
            if d.power.clam.lock().unwrap().active {
                refresh_lid(&d).await;
                dirty = true;
            }
            if d.power.ka_enabled() {
                let procs = list_processes(&d).await;
                if !d.power.operations.accepting() {
                    return;
                }
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

fn power_profile_id() -> String {
    let path = jarvis_dir();
    std::fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn new_owner_generation() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_ms(),
        NEXT_OWNER_GENERATION.fetch_add(1, Ordering::Relaxed)
    )
}

fn power_lease_with<P: clamshell::ProcessInspector>(
    processes: &P,
    profile: &str,
    pid: u32,
    owner_generation: &str,
    acquired_at_ms: i64,
) -> Result<ownership::Lease, clamshell::PowerError> {
    let process_identity = processes.start_identity(pid)?.ok_or_else(|| {
        clamshell::PowerError::InvalidState(format!(
            "cannot prove process start identity for PID {pid}"
        ))
    })?;
    if !clamshell::valid_process_identity(&process_identity) {
        return Err(clamshell::PowerError::InvalidState(format!(
            "unsupported process identity for PID {pid}"
        )));
    }
    let expires_at_ms = acquired_at_ms
        .checked_add(CLAMSHELL_LEASE_TTL_MS)
        .ok_or_else(|| clamshell::PowerError::InvalidState("lease timestamp overflow".into()))?;
    Ok(ownership::Lease {
        profile: profile.into(),
        pid,
        process_identity,
        owner_generation: owner_generation.into(),
        acquired_at_ms,
        expires_at_ms,
    })
}

fn power_lease(owner_generation: &str) -> Result<ownership::Lease, clamshell::PowerError> {
    power_lease_with(
        &clamshell::SystemProcesses,
        &power_profile_id(),
        std::process::id(),
        owner_generation,
        now_ms(),
    )
}

/// Трей/панель обновить (аналог ctx.changed() БЕЗ broadcast: связка с
/// «Крышкой» дёргается только из событий keep-awake, иначе ручной disarm
/// крышки мгновенно ре-армился бы peer_sync'ом).
fn changed(d: &Arc<Daemon>) {
    if !d.power.operations.accepting() {
        return;
    }
    crate::tray::update(d, &d.snapshot());
    crate::plugins::emit_statuses(d);
}

fn handle_engine_events(d: &Arc<Daemon>, events: Vec<Event>) {
    if !d.power.operations.accepting() {
        return;
    }
    for e in &events {
        match e {
            Event::TimerEnd => {
                d.notify(
                    "☕ Таймер вышел",
                    "Мак снова может спать как обычно",
                    None,
                    "done",
                );
            }
            Event::ProcessDied { label } => {
                d.notify(
                    "☕ Снимаю запрет сна",
                    &format!("{label} завершился"),
                    None,
                    "done",
                );
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
    if !d.power.operations.accepting() {
        return;
    }
    let s = Power::cs_settings(d);
    let (active, busy, armed, armed_by) = {
        let clam = d.power.clam.lock().unwrap();
        (clam.active, clam.busy, clam.armed, clam.armed_by)
    };
    if !active || busy || s["autoArm"].as_bool() != Some(true) || !clamshell::sudoers_installed() {
        return;
    }
    let ka_active = d
        .power
        .engine
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|e| e.active());
    let d = d.clone();
    tauri::async_runtime::spawn(async move {
        if !d.power.operations.accepting() {
            return;
        }
        if ka_active && !armed {
            arm(&d, "auto").await;
        } else if !ka_active && armed && armed_by == Some("auto") {
            disarm(&d).await;
        }
    });
}

async fn arm(d: &Arc<Daemon>, by: &'static str) -> Value {
    if let Some(error) = arm_recovery_error(&startup_recovery_health()) {
        return json!({
            "ok": false,
            "error": error,
            "repairable": true,
            "repairAction": POWER_REPAIR_ACTION,
        });
    }
    let Some(operation) = d.power.operations.begin() else {
        return json!({ "ok": false, "error": "Jarvis завершает работу или power занят" });
    };
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
    let owner_generation = new_owner_generation();
    let lease = match power_lease(&owner_generation) {
        Ok(lease) => lease,
        Err(error) => {
            d.power.clam.lock().unwrap().busy = false;
            return json!({
                "ok": false,
                "error": format!("{error}; {POWER_REPAIR_ACTION}"),
                "repairable": true,
                "repairAction": POWER_REPAIR_ACTION,
            });
        }
    };
    let profile = lease.profile.clone();
    let worker_daemon = d.clone();
    let commit_daemon = d.clone();
    let retry_daemon = d.clone();
    let commit_generation = owner_generation.clone();
    let rollback_generation = owner_generation.clone();
    let retry_generation = owner_generation.clone();
    let acquired = tauri::async_runtime::spawn_blocking(move || {
        let worker_operations = &worker_daemon.power.operations;
        if !worker_operations.accepts(operation.epoch()) {
            return Ok(AcquireDisposition::RolledBack);
        }
        let store = crate::power::ownership_store::OwnershipStore::global();
        let acquire_result = match clamshell::acquire_with(&clamshell::SystemPmset, &store, lease) {
            Ok(outcome) => Ok(outcome),
            Err(failure) => {
                let cleanup = clamshell::release_with(
                    &clamshell::SystemPmset,
                    &store,
                    &profile,
                    &retry_generation,
                );
                if failed_acquire_needs_owner_retry(&failure, &cleanup) {
                    let mut clam = retry_daemon.power.clam.lock().unwrap();
                    clam.armed = true;
                    clam.armed_by = Some(by);
                    clam.owner_generation = Some(retry_generation.clone());
                    clam.owner_obligation = Some(failure.obligation);
                }
                let message = match cleanup {
                    Ok(release) if release_resolves_obligation(failure.obligation, release) => {
                        failure.error.to_string()
                    }
                    Ok(release) => format!(
                        "{}; cleanup retry did not resolve {:?}: {release:?}",
                        failure.error, failure.obligation
                    ),
                    Err(cleanup) => {
                        format!("{}; cleanup retry failed: {cleanup}", failure.error)
                    }
                };
                Err(message)
            }
        };
        let rollback_daemon = worker_daemon.clone();
        let rollback_profile = profile.clone();
        worker_operations.finish_acquire(
            operation,
            acquire_result,
            move |_| {
                let mut clam = commit_daemon.power.clam.lock().unwrap();
                clam.armed = true;
                clam.armed_by = Some(by);
                clam.owner_generation = Some(commit_generation);
                clam.owner_obligation = Some(clamshell::AcquireObligation::MutationMayRemain);
                clam.last_guard_at = 0;
            },
            move || match clamshell::release_with(
                &clamshell::SystemPmset,
                &store,
                &rollback_profile,
                &rollback_generation,
            ) {
                Ok(release) if release_was_confirmed(release) => Ok(()),
                Ok(release) => {
                    let mut clam = rollback_daemon.power.clam.lock().unwrap();
                    clam.armed = true;
                    clam.armed_by = Some(by);
                    clam.owner_generation = Some(rollback_generation);
                    clam.owner_obligation = Some(clamshell::AcquireObligation::MutationMayRemain);
                    Err(format!(
                        "late rollback was not ownership-confirmed: {release:?}"
                    ))
                }
                Err(error) => {
                    let mut clam = rollback_daemon.power.clam.lock().unwrap();
                    clam.armed = true;
                    clam.armed_by = Some(by);
                    clam.owner_generation = Some(rollback_generation);
                    clam.owner_obligation = Some(clamshell::AcquireObligation::MutationMayRemain);
                    Err(error.to_string())
                }
            },
        )
    })
    .await;
    let result = if matches!(&acquired, Ok(Ok(AcquireDisposition::Committed)))
        && d.power.operations.accepting()
    {
        changed(d);
        json!({ "ok": true })
    } else {
        let error = match acquired {
            Ok(Ok(AcquireDisposition::RolledBack)) => "Jarvis завершает работу".into(),
            Ok(Ok(AcquireDisposition::Committed)) => "Jarvis завершает работу".into(),
            Ok(Err(error)) => error.to_string(),
            Err(error) => format!("power worker failed: {error}"),
        };
        json!({ "ok": false, "error": error })
    };
    d.power.clam.lock().unwrap().busy = false;
    result
}

async fn disarm(d: &Arc<Daemon>) -> Value {
    let Some(operation) = d.power.operations.begin() else {
        return json!({ "ok": false, "error": "Jarvis завершает работу или power занят" });
    };
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
    let (owner_generation, owner_obligation) = {
        let clam = d.power.clam.lock().unwrap();
        (clam.owner_generation.clone(), clam.owner_obligation)
    };
    let Some(owner_generation) = owner_generation else {
        d.power.clam.lock().unwrap().busy = false;
        return json!({
            "ok": false,
            "error": "старый marker не доказывает владение; нужен явный repair"
        });
    };
    let owner_obligation =
        owner_obligation.unwrap_or(clamshell::AcquireObligation::MutationMayRemain);
    let profile = power_profile_id();
    let released = tauri::async_runtime::spawn_blocking(move || {
        let outcome = clamshell::release_with(
            &clamshell::SystemPmset,
            &crate::power::ownership_store::OwnershipStore::global(),
            &profile,
            &owner_generation,
        );
        (operation, outcome)
    })
    .await;
    let (returned_operation, release) = match released {
        Ok(result) => result,
        Err(error) => {
            d.power.clam.lock().unwrap().busy = false;
            return json!({
                "ok": false,
                "error": format!("power worker failed: {error}")
            });
        }
    };
    let result = match release {
        Ok(outcome) if release_resolves_obligation(owner_obligation, outcome) => {
            let mut clam = d.power.clam.lock().unwrap();
            clam.armed = false;
            clam.armed_by = None;
            clam.owner_generation = None;
            clam.owner_obligation = None;
            drop(clam);
            changed(d);
            json!({ "ok": true })
        }
        Ok(outcome) => json!({
            "ok": false,
            "error": format!("release was not ownership-confirmed: {outcome:?}")
        }),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    };
    d.power.clam.lock().unwrap().busy = false;
    drop(returned_operation);
    result
}

async fn refresh_lid(d: &Arc<Daemon>) {
    if !d.power.operations.accepting() {
        return;
    }
    let lid = clamshell::read_lid().await;
    if !d.power.operations.accepting() {
        return;
    }
    d.power.clam.lock().unwrap().lid_causes_sleep = lid.causes_sleep;
}

/// Батарейный сторож: armed + батарея ≤ floor → тихий сброс или форс-сон.
async fn battery_guard(d: &Arc<Daemon>) {
    let floor = Power::cs_settings(d)["batteryFloor"].as_i64().unwrap_or(15) as u32;
    let batt = clamshell::read_battery().await;
    let (Some(pct), Some(true)) = (batt.pct, batt.on_battery) else {
        return;
    };
    if pct > floor {
        return;
    }
    let Some(operation) = d.power.operations.begin() else {
        return;
    };
    println!("[jarvis:clamshell] батарея {pct}% ≤ {floor}% — освобождаю ownership lease");
    let (owner_generation, owner_obligation) = {
        let clam = d.power.clam.lock().unwrap();
        (
            clam.owner_generation.clone(),
            clam.owner_obligation
                .unwrap_or(clamshell::AcquireObligation::MutationMayRemain),
        )
    };
    let mut operation = Some(operation);
    let release_outcome = if let Some(owner_generation) = owner_generation {
        let profile = power_profile_id();
        let worker_operation = operation.take().unwrap();
        tauri::async_runtime::spawn_blocking(move || {
            let outcome = clamshell::release_with(
                &clamshell::SystemPmset,
                &crate::power::ownership_store::OwnershipStore::global(),
                &profile,
                &owner_generation,
            );
            (worker_operation, outcome)
        })
        .await
        .ok()
        .and_then(|(worker_operation, outcome)| {
            operation = Some(worker_operation);
            outcome
                .ok()
                .filter(|outcome| release_resolves_obligation(owner_obligation, *outcome))
        })
    } else {
        None
    };
    if let Some(outcome) = release_outcome {
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.armed = false;
            clam.armed_by = None;
            clam.owner_generation = None;
            clam.owner_obligation = None;
        }
        changed(d);
        if !d.power.operations.accepting() {
            return;
        }
        if battery_release_confirms_normal_sleep(outcome) {
            d.notify(
                "⌒ Крышка: батарея садится",
                &format!("Осталось {pct}% — вернул нормальный сон"),
                None,
                "done",
            );
        } else {
            // Другая profile lease или внешний baseline всё ещё запрещает
            // автоматический сон. Не мутируем чужой state, но форсируем один
            // безопасный sleepnow ради батареи.
            d.notify(
                "⌒ Крышка: батарея садится",
                &format!("Осталось {pct}% — другой режим ещё активен, усыпляю мак"),
                None,
                "done",
            );
            clamshell::force_sleep_now().await;
        }
    } else {
        if !d.power.operations.accepting() {
            return;
        }
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
    drop(operation);
}

/// Проснулись после сна, который прервал работу → подсказка про closed-display.
async fn on_resume(d: &Arc<Daemon>, working_at_sleep: usize) {
    if !d.power.operations.accepting() {
        return;
    }
    refresh_lid(d).await;
    if !d.power.operations.accepting() {
        return;
    }
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
        if n == 1 {
            "работающую сессию"
        } else {
            "работающие сессии"
        }
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
    if !d.power.operations.accepting() {
        return json!({ "ok": false, "error": "Jarvis завершает работу" });
    }
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
        tokio::process::Command::new("osascript")
            .args(["-e", &script])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some_and(|o| o.status.success());
    let _ = std::fs::remove_file(&tmp);
    if !d.power.operations.accepting() {
        return json!({ "ok": false, "error": "Jarvis завершает работу" });
    }
    if !ok {
        return json!({ "ok": false, "error": "установка отменена" });
    }
    d.notify(
        "⌒ Тихий режим настроен",
        "Теперь closed-display переключается без пароля",
        None,
        "done",
    );
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
            tokio::process::Command::new("osascript")
                .args(["-e", line])
                .output(),
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
        let ids: Vec<i64> = ids_line
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Duration;

    struct FakeProcesses {
        identities: HashMap<u32, Result<Option<String>, String>>,
    }

    impl FakeProcesses {
        fn identity(pid: u32, identity: &str) -> Self {
            Self {
                identities: HashMap::from([(pid, Ok(Some(identity.into())))]),
            }
        }

        fn missing(pid: u32) -> Self {
            Self {
                identities: HashMap::from([(pid, Ok(None))]),
            }
        }
    }

    impl clamshell::ProcessInspector for FakeProcesses {
        fn start_identity(&self, pid: u32) -> Result<Option<String>, clamshell::PowerError> {
            match self.identities.get(&pid) {
                Some(Ok(identity)) => Ok(identity.clone()),
                Some(Err(error)) => Err(clamshell::PowerError::InvalidState(error.clone())),
                None => Ok(None),
            }
        }
    }

    #[test]
    fn blocking_startup_recovery_health_rejects_arm_with_repair_action() {
        let health = StartupRecoveryHealth::Blocked {
            message: "ownership registry is corrupt".into(),
        };

        let error = arm_recovery_error(&health).unwrap();
        assert!(error.contains("ownership registry is corrupt"));
        assert!(error.contains("repair"));
    }

    #[test]
    fn startup_recovery_outcomes_become_process_health_without_panicking_startup() {
        assert_eq!(
            health_from_recovery(Ok(clamshell::RecoveryOutcome::NoRegistry)),
            StartupRecoveryHealth::Ready(clamshell::RecoveryOutcome::NoRegistry)
        );
        assert!(matches!(
            health_from_recovery(Ok(clamshell::RecoveryOutcome::BlockedExpiredLiveLease)),
            StartupRecoveryHealth::Blocked { .. }
        ));
        assert!(matches!(
            health_from_recovery(Err(clamshell::PowerError::RollbackUnavailable)),
            StartupRecoveryHealth::Blocked { .. }
        ));
    }

    #[test]
    fn power_lease_requires_exact_versioned_process_identity() {
        let exact = "darwin-v1:uid=501:start=100.7";
        let lease = power_lease_with(
            &FakeProcesses::identity(42, exact),
            "profile",
            42,
            "generation",
            100,
        )
        .unwrap();
        assert_eq!(lease.process_identity, exact);

        assert!(matches!(
            power_lease_with(
                &FakeProcesses::missing(42),
                "profile",
                42,
                "generation",
                100,
            ),
            Err(clamshell::PowerError::InvalidState(_))
        ));
        assert!(matches!(
            power_lease_with(
                &FakeProcesses::identity(42, "42:100"),
                "profile",
                42,
                "generation",
                100,
            ),
            Err(clamshell::PowerError::InvalidState(_))
        ));
    }

    #[test]
    fn missing_registry_does_not_confirm_an_in_memory_release() {
        assert!(!release_was_confirmed(clamshell::ReleaseOutcome::NotOwned));
        assert!(!PowerDisposeReport {
            clamshell: ClamshellDisposeOutcome::Released(clamshell::ReleaseOutcome::NotOwned),
        }
        .released());
        assert!(release_was_confirmed(
            clamshell::ReleaseOutcome::KeptApplied
        ));
        assert!(release_was_confirmed(
            clamshell::ReleaseOutcome::BaselineUnchanged(false)
        ));
        assert!(release_was_confirmed(clamshell::ReleaseOutcome::Restored(
            false
        )));
    }

    #[test]
    fn failed_acquire_retention_uses_explicit_obligation() {
        let no_obligation = clamshell::AcquireFailure {
            error: clamshell::PowerError::RollbackUnavailable,
            obligation: clamshell::AcquireObligation::None,
        };
        assert!(!failed_acquire_needs_owner_retry(
            &no_obligation,
            &Ok(clamshell::ReleaseOutcome::NotOwned),
        ));
        assert!(!failed_acquire_needs_owner_retry(
            &no_obligation,
            &Err(clamshell::PowerError::Command("cleanup failed".into())),
        ));
        let lease_may_exist = clamshell::AcquireFailure {
            error: clamshell::PowerError::Store(crate::power::ownership_store::StoreError::Io(
                std::io::Error::other("post-rename fsync failed"),
            )),
            obligation: clamshell::AcquireObligation::LeaseMayExist,
        };
        assert!(!failed_acquire_needs_owner_retry(
            &lease_may_exist,
            &Ok(clamshell::ReleaseOutcome::NotOwned),
        ));
        assert!(failed_acquire_needs_owner_retry(
            &lease_may_exist,
            &Err(clamshell::PowerError::Command("cleanup failed".into())),
        ));
        let mutation_may_remain = clamshell::AcquireFailure {
            error: clamshell::PowerError::RollbackFailed("rollback unknown".into()),
            obligation: clamshell::AcquireObligation::MutationMayRemain,
        };
        assert!(failed_acquire_needs_owner_retry(
            &mutation_may_remain,
            &Ok(clamshell::ReleaseOutcome::NotOwned),
        ));
        assert!(failed_acquire_needs_owner_retry(
            &mutation_may_remain,
            &Err(clamshell::PowerError::Command("cleanup failed".into())),
        ));
        assert!(!failed_acquire_needs_owner_retry(
            &mutation_may_remain,
            &Ok(clamshell::ReleaseOutcome::Restored(false)),
        ));
    }

    #[test]
    fn resolved_lease_uncertainty_does_not_claim_normal_sleep() {
        let outcome = clamshell::ReleaseOutcome::NotOwned;

        assert!(release_resolves_obligation(
            clamshell::AcquireObligation::LeaseMayExist,
            outcome,
        ));
        assert!(!battery_release_confirms_normal_sleep(outcome));
        assert!(!battery_release_confirms_normal_sleep(
            clamshell::ReleaseOutcome::BaselineUnchanged(true),
        ));
        assert!(battery_release_confirms_normal_sleep(
            clamshell::ReleaseOutcome::Restored(false),
        ));
    }

    #[test]
    fn shutdown_gate_is_one_way_and_idempotent() {
        let gate = ShutdownGate::default();
        assert!(gate.accepting());
        assert!(gate.close());
        assert!(!gate.accepting());
        assert!(!gate.close());
    }

    #[test]
    fn shutdown_gate_advances_epoch_and_rejects_new_operations() {
        let operations = PowerOperations::default();
        let admitted_epoch = operations.begin().unwrap().epoch();

        assert!(operations.close());
        assert_ne!(operations.epoch(), admitted_epoch);
        assert!(operations.begin().is_none());
        assert!(!operations.close());
    }

    #[test]
    fn acquire_that_finishes_after_close_is_rolled_back_before_barrier_opens() {
        let operations = Arc::new(PowerOperations::default());
        let operation = operations.begin().unwrap();
        let (acquire_started_tx, acquire_started_rx) = mpsc::channel();
        let (finish_acquire_tx, finish_acquire_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let lease_present = Arc::new(AtomicBool::new(false));
        let sleep_disabled = Arc::new(AtomicBool::new(false));

        let worker_operations = operations.clone();
        let worker_lease = lease_present.clone();
        let worker_sleep = sleep_disabled.clone();
        let worker = std::thread::spawn(move || {
            acquire_started_tx.send(()).unwrap();
            finish_acquire_rx.recv().unwrap();
            worker_lease.store(true, Ordering::SeqCst);
            worker_sleep.store(true, Ordering::SeqCst);

            let disposition = worker_operations
                .finish_acquire(
                    operation,
                    Ok::<_, ()>(()),
                    |_| panic!("closed acquire must not commit"),
                    || {
                        assert!(worker_lease.swap(false, Ordering::SeqCst));
                        worker_sleep.store(false, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .unwrap();
            finished_tx.send(disposition).unwrap();
        });

        acquire_started_rx.recv().unwrap();
        assert!(operations.close());
        assert!(!operations.wait_for_idle(Duration::ZERO));
        finish_acquire_tx.send(()).unwrap();

        assert_eq!(finished_rx.recv().unwrap(), AcquireDisposition::RolledBack);
        assert!(operations.wait_for_idle(Duration::ZERO));
        assert!(!lease_present.load(Ordering::SeqCst));
        assert!(!sleep_disabled.load(Ordering::SeqCst));
        worker.join().unwrap();
    }

    #[test]
    fn acquire_commit_finishes_before_operation_barrier_opens() {
        let operations = PowerOperations::default();
        let operation = operations.begin().unwrap();
        let mut committed = false;

        let disposition = operations
            .finish_acquire(
                operation,
                Ok::<_, ()>(()),
                |_| {
                    assert!(!operations.wait_for_idle(Duration::ZERO));
                    committed = true;
                },
                || panic!("accepted acquire must not roll back"),
            )
            .unwrap();

        assert_eq!(disposition, AcquireDisposition::Committed);
        assert!(committed);
        assert!(operations.wait_for_idle(Duration::ZERO));
    }

    #[test]
    fn failed_late_rollback_still_opens_operation_barrier() {
        let operations = PowerOperations::default();
        let operation = operations.begin().unwrap();
        assert!(operations.close());

        let result = operations.finish_acquire(
            operation,
            Ok::<_, &'static str>(()),
            |_| panic!("closed acquire must not commit"),
            || Err("rollback failed"),
        );

        assert_eq!(result, Err("rollback failed"));
        assert!(operations.wait_for_idle(Duration::ZERO));
    }

    #[test]
    fn worker_returned_operation_keeps_barrier_closed_for_async_bookkeeping() {
        let operations = PowerOperations::default();
        let operation = operations.begin().unwrap();
        let returned_operation = std::thread::spawn(move || operation).join().unwrap();

        assert!(operations.close());
        assert!(!operations.wait_for_idle(Duration::ZERO));
        drop(returned_operation);
        assert!(operations.wait_for_idle(Duration::ZERO));
    }

    #[test]
    fn disarm_operation_stays_held_through_result_and_busy_bookkeeping() {
        let operations = Arc::new(PowerOperations::default());
        let operation = operations.begin().unwrap();
        let worker = std::thread::spawn(move || {
            (
                operation,
                Ok::<_, clamshell::PowerError>(clamshell::ReleaseOutcome::Restored(false)),
            )
        });
        let (returned_operation, release) = worker.join().unwrap();

        assert!(operations.close());
        assert!(!operations.wait_for_idle(Duration::ZERO));
        assert!(matches!(
            release,
            Ok(clamshell::ReleaseOutcome::Restored(false))
        ));
        let mut busy = true;
        assert!(busy);
        busy = false;
        assert!(!busy);
        assert!(!operations.wait_for_idle(Duration::ZERO));

        drop(returned_operation);
        assert!(operations.wait_for_idle(Duration::ZERO));
    }
}
