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
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::Duration;

use crate::daemon::Daemon;
use crate::model::{Session, Status};
use crate::util::{jarvis_dir, now_ms, one_line};
use assertion::IopmBlocker;
use helper::renewal::{
    run_shutdown_sequence, ExactReleaseOutcome, LeaseClient, LeaseReceipt, RenewalExit,
    RenewalHandle,
};
use keep_awake::{Engine, Event};

const SUGGEST_GAP_MS: i64 = 60 * 60 * 1000; // подсказка не чаще раза в час
const GUARD_EVERY_MS: i64 = 60 * 1000;
const WAKE_GAP_MS: i64 = 90 * 1000;
#[cfg(test)]
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
    Blocked {
        message: String,
    },
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
        commit: impl FnOnce(T) -> Result<(), T>,
        rollback: impl FnOnce(T) -> Result<(), E>,
    ) -> Result<AcquireDisposition, E> {
        let acquired = acquired?;
        let disposition = match self.accepts(operation.epoch()) {
            true => match commit(acquired) {
                Ok(()) => AcquireDisposition::Committed,
                Err(acquired) => {
                    rollback(acquired)?;
                    AcquireDisposition::RolledBack
                }
            },
            false => {
                rollback(acquired)?;
                AcquireDisposition::RolledBack
            }
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
    Released,
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
            ClamshellDisposeOutcome::Idle | ClamshellDisposeOutcome::Released => true,
            ClamshellDisposeOutcome::BarrierTimeout | ClamshellDisposeOutcome::ReleaseFailed(_) => {
                false
            }
        }
    }
}

fn clamshell_disable_response(outcome: ClamshellDisposeOutcome) -> Value {
    match outcome {
        ClamshellDisposeOutcome::Idle | ClamshellDisposeOutcome::Released => {
            json!({ "ok": true, "pendingCleanup": false })
        }
        ClamshellDisposeOutcome::BarrierTimeout => json!({
            "ok": false,
            "pendingCleanup": true,
            "error": "clamshell cleanup timed out; exact helper lease may still be pending",
        }),
        ClamshellDisposeOutcome::ReleaseFailed(error) => json!({
            "ok": false,
            "pendingCleanup": true,
            "error": format!("clamshell cleanup failed: {error}"),
        }),
    }
}

struct BatteryGuardDecision {
    force_sleep: bool,
    message: String,
}

fn battery_guard_decision(pct: u32, outcome: &ExactReleaseOutcome) -> BatteryGuardDecision {
    let detail = match outcome {
        ExactReleaseOutcome::Confirmed => "helper lease освобождена",
        ExactReleaseOutcome::AlreadyAbsent(_) => "helper lease уже завершена",
        ExactReleaseOutcome::Retryable(_) => "точный helper release не подтверждён",
    };
    BatteryGuardDecision {
        // A successful exact release does not prove the global baseline is
        // sleep-enabled: another Jarvis profile may still own a helper lease.
        force_sleep: true,
        message: format!("Осталось {pct}% — {detail}; принудительно усыпляю мак"),
    }
}

#[cfg(test)]
fn release_was_confirmed(outcome: clamshell::ReleaseOutcome) -> bool {
    !matches!(outcome, clamshell::ReleaseOutcome::NotOwned)
}

#[cfg(test)]
fn release_resolves_obligation(
    obligation: clamshell::AcquireObligation,
    outcome: clamshell::ReleaseOutcome,
) -> bool {
    match obligation {
        clamshell::AcquireObligation::None | clamshell::AcquireObligation::LeaseMayExist => true,
        clamshell::AcquireObligation::MutationMayRemain => release_was_confirmed(outcome),
    }
}

#[cfg(test)]
fn battery_release_confirms_normal_sleep(outcome: clamshell::ReleaseOutcome) -> bool {
    outcome.sleep_disabled() == Some(false)
}

#[cfg(test)]
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
    lease: Option<LeaseReceipt>,
    renewal: Option<RenewalHandle>,
    renewal_error: Option<String>,
    busy: bool, // arm/disarm в полёте — не наслаиваем
    last_guard_at: i64,
    lid_causes_sleep: Option<bool>, // кэш для статусной строки меню
}

impl Clam {
    fn commit_acquired_if_active(
        &mut self,
        receipt: LeaseReceipt,
        by: &'static str,
    ) -> Result<(), LeaseReceipt> {
        if !self.active
            || !self.busy
            || self.armed
            || self.lease.is_some()
            || self.renewal.is_some()
        {
            return Err(receipt);
        }
        self.armed = true;
        self.armed_by = Some(by);
        self.lease = Some(receipt);
        self.renewal_error = None;
        self.last_guard_at = 0;
        Ok(())
    }

    fn retain_lease_debt(&mut self, receipt: LeaseReceipt, error: impl Into<String>) {
        if self.lease.is_none() || self.lease.as_ref() == Some(&receipt) {
            self.lease = Some(receipt);
        }
        self.armed = false;
        self.armed_by = None;
        self.renewal_error = Some(error.into());
    }

    fn mark_lease_unrenewable(&mut self, receipt: &LeaseReceipt, error: impl Into<String>) {
        if self.lease.as_ref() != Some(receipt) {
            return;
        }
        self.armed = false;
        self.armed_by = None;
        self.renewal_error = Some(error.into());
    }

    fn has_visible_status(&self) -> bool {
        self.active || self.busy || self.lease.is_some() || self.renewal_error.is_some()
    }
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
    lease_client: LeaseClient,
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
            lease_client: LeaseClient::production(),
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

    fn deactivate_clamshell(d: &Arc<Daemon>) -> ClamshellDisposeOutcome {
        let was_active = {
            let mut clam = d.power.clam.lock().unwrap();
            let was_active = clam.active;
            clam.active = false;
            was_active
        };
        Self::stop_clamshell_renewal(d);
        let Some(_operation) = d
            .power
            .operations
            .begin_wait(POWER_OPERATION_BARRIER_TIMEOUT)
        else {
            let message = "release admission timed out or shutdown started";
            eprintln!("[jarvis:clamshell] {message}");
            let receipt = d.power.clam.lock().unwrap().lease.clone();
            if let Some(receipt) = receipt {
                d.power
                    .clam
                    .lock()
                    .unwrap()
                    .mark_lease_unrenewable(&receipt, message);
            }
            return ClamshellDisposeOutcome::BarrierTimeout;
        };
        let outcome = Self::deactivate_clamshell_inner(d);
        if was_active {
            println!("[jarvis:clamshell] выключен");
        }
        outcome
    }

    fn deactivate_clamshell_inner(d: &Arc<Daemon>) -> ClamshellDisposeOutcome {
        let (was_active, receipt) = {
            let mut clam = d.power.clam.lock().unwrap();
            let was_active = clam.active;
            if !was_active && clam.lease.is_none() {
                return ClamshellDisposeOutcome::Idle;
            }
            clam.active = false;
            clam.armed = false;
            clam.armed_by = None;
            (was_active, clam.lease.clone())
        };
        let outcome = match receipt {
            Some(receipt) => {
                match ExactReleaseOutcome::from_result(d.power.lease_client.release(&receipt)) {
                    ExactReleaseOutcome::Confirmed | ExactReleaseOutcome::AlreadyAbsent(_) => {
                        let mut clam = d.power.clam.lock().unwrap();
                        if clam.lease.as_ref() == Some(&receipt) {
                            clam.lease = None;
                            clam.renewal_error = None;
                        }
                        ClamshellDisposeOutcome::Released
                    }
                    ExactReleaseOutcome::Retryable(error) => {
                        // Retain the exact receipt for a later shutdown retry.
                        // The helper TTL remains the final fail-closed boundary.
                        eprintln!(
                            "[jarvis:clamshell] helper release on deactivate failed: {error}"
                        );
                        d.power
                            .clam
                            .lock()
                            .unwrap()
                            .retain_lease_debt(receipt, error.to_string());
                        ClamshellDisposeOutcome::ReleaseFailed(error.to_string())
                    }
                }
            }
            None => ClamshellDisposeOutcome::Idle,
        };
        if was_active {
            println!("[jarvis:clamshell] выключен");
        }
        outcome
    }

    fn stop_clamshell_renewal(d: &Arc<Daemon>) {
        let renewal = d.power.clam.lock().unwrap().renewal.take();
        if let Some(renewal) = renewal {
            renewal.stop();
        }
    }

    fn reap_finished_clamshell_renewal(d: &Arc<Daemon>) {
        let renewal = {
            let mut clam = d.power.clam.lock().unwrap();
            if clam
                .renewal
                .as_ref()
                .is_some_and(RenewalHandle::is_finished)
            {
                clam.renewal.take()
            } else {
                None
            }
        };
        if let Some(renewal) = renewal {
            renewal.stop();
        }
    }

    fn start_clamshell_renewal(d: &Arc<Daemon>) -> Result<(), String> {
        let receipt = {
            let clam = d.power.clam.lock().unwrap();
            if !d.power.operations.accepting()
                || !clam.active
                || clam.busy
                || !clam.armed
                || clam.renewal.is_some()
            {
                return Err("clamshell renewal is no longer admissible".into());
            }
            let Some(receipt) = clam.lease.clone() else {
                return Err("helper lease receipt is missing".into());
            };
            receipt
        };
        let weak_daemon = Arc::downgrade(d);
        let worker_receipt = receipt.clone();
        let exit_daemon = Arc::downgrade(d);
        let exit_receipt = receipt.clone();
        let renewal = RenewalHandle::try_start_with_exit(
            Duration::from_millis(jarvis_power_core::protocol::RENEW_EVERY_MS),
            move || {
                let Some(d) = weak_daemon.upgrade() else {
                    return false;
                };
                let Some(operation) = d.power.operations.begin() else {
                    return d.power.operations.accepting();
                };
                let epoch = operation.epoch();
                let result = d.power.lease_client.renew(&worker_receipt);
                let renew_succeeded = result.is_ok();
                let accepted = d.power.operations.accepts(epoch);
                let mut state_changed = false;
                if accepted {
                    let mut clam = d.power.clam.lock().unwrap();
                    if clam.lease.as_ref() == Some(&worker_receipt) {
                        match result {
                            Ok(()) => clam.renewal_error = None,
                            Err(error) => {
                                match ExactReleaseOutcome::from_result(Err(error)) {
                                    ExactReleaseOutcome::AlreadyAbsent(_) => {
                                        // The helper proved this exact receipt
                                        // has expired or disappeared. Clear only
                                        // this local debt; never auto-reacquire.
                                        clam.armed = false;
                                        clam.armed_by = None;
                                        clam.lease = None;
                                        clam.renewal_error =
                                            Some(format!("helper lease ended: {error}"));
                                    }
                                    ExactReleaseOutcome::Retryable(_) => {
                                        clam.mark_lease_unrenewable(
                                            &worker_receipt,
                                            error.to_string(),
                                        );
                                    }
                                    ExactReleaseOutcome::Confirmed => unreachable!(),
                                }
                                state_changed = true;
                            }
                        }
                    }
                }
                drop(operation);
                if accepted && state_changed {
                    changed(&d);
                }
                accepted && renew_succeeded
            },
            move |exit| {
                if !matches!(exit, RenewalExit::Panicked | RenewalExit::ControlFailed) {
                    return;
                }
                let Some(d) = exit_daemon.upgrade() else {
                    return;
                };
                if !d.power.operations.accepting() {
                    return;
                }
                let message = match exit {
                    RenewalExit::Panicked => "renewal worker panicked",
                    RenewalExit::ControlFailed => "renewal worker control failed",
                    RenewalExit::Cancelled | RenewalExit::AttemptStopped => return,
                };
                let state_changed = {
                    let mut clam = d.power.clam.lock().unwrap();
                    if clam.lease.as_ref() == Some(&exit_receipt) {
                        clam.mark_lease_unrenewable(&exit_receipt, message);
                        true
                    } else {
                        false
                    }
                };
                if state_changed {
                    changed(&d);
                }
            },
        )
        .map_err(|error| format!("cannot start renewal worker: {error}"))?;
        let mut clam = d.power.clam.lock().unwrap();
        if d.power.operations.accepting()
            && clam.armed
            && clam.active
            && !clam.busy
            && clam.lease.as_ref() == Some(&receipt)
            && clam.renewal.is_none()
        {
            clam.renewal = Some(renewal);
            Ok(())
        } else {
            drop(clam);
            renewal.stop();
            Err("clamshell renewal lost admission before installation".into())
        }
    }

    fn release_after_renewal_start_failure(d: &Arc<Daemon>, reason: &str) -> String {
        let receipt = d.power.clam.lock().unwrap().lease.clone();
        let Some(receipt) = receipt else {
            return reason.into();
        };

        let outcome = ExactReleaseOutcome::from_result(d.power.lease_client.release(&receipt));
        let message = match outcome {
            ExactReleaseOutcome::Confirmed | ExactReleaseOutcome::AlreadyAbsent(_) => {
                let mut clam = d.power.clam.lock().unwrap();
                if clam.lease.as_ref() == Some(&receipt) {
                    clam.armed = false;
                    clam.armed_by = None;
                    clam.lease = None;
                    clam.renewal_error = Some(reason.into());
                }
                reason.to_owned()
            }
            ExactReleaseOutcome::Retryable(error) => {
                let message = format!("{reason}; exact release failed: {error}");
                d.power
                    .clam
                    .lock()
                    .unwrap()
                    .retain_lease_debt(receipt, message.clone());
                message
            }
        };
        if d.power.operations.accepting() {
            changed(d);
        }
        message
    }

    /// Выход из приложения: сначала закрыть admission и остановить renewal,
    /// затем освободить точную helper lease, и только потом снять IOKit.
    pub fn dispose(d: &Arc<Daemon>) -> PowerDisposeReport {
        let clamshell = run_shutdown_sequence(
            || {
                d.power.operations.close();
            },
            || Self::stop_clamshell_renewal(d),
            || {
                if d.power
                    .operations
                    .wait_for_idle(POWER_OPERATION_BARRIER_TIMEOUT)
                {
                    // Admission is closed, so no new operation can race this retry.
                    Self::deactivate_clamshell_inner(d)
                } else {
                    eprintln!("[jarvis:power] timed out waiting for in-flight helper operation");
                    ClamshellDisposeOutcome::BarrierTimeout
                }
            },
            || Self::deactivate_keep_awake(d),
        );
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
        let cs_settings = Self::cs_settings(d);
        let (cs_enabled, cs_status) = {
            let clam = self.clam.lock().unwrap();
            let enabled = clam.active;
            let status = clam.has_visible_status().then(|| {
                json!({
                    "armed": clam.armed,
                    "armedBy": clam.armed_by,
                    "autoArm": cs_settings["autoArm"],
                    "suggest": cs_settings["suggest"],
                    "batteryFloor": cs_settings["batteryFloor"],
                    "helper": "xpc",
                    "helperLease": clam.lease.is_some(),
                    "pendingCleanup": !clam.active && clam.lease.is_some(),
                    "renewalError": clam.renewal_error.clone(),
                })
            });
            (enabled, status)
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
            if !matches!(id, "keep-awake" | "clamshell") {
                return json!({ "ok": false, "error": "плагин не найден" });
            }
            let mut patch = Map::new();
            patch.insert("enabled".into(), Value::Bool(on));
            match (id, on) {
                ("keep-awake", true) => {
                    d.settings.set_plugin(id, patch);
                    if !d.power.ka_enabled() {
                        Self::activate_keep_awake(d);
                    }
                }
                ("keep-awake", false) => {
                    Self::deactivate_keep_awake(d);
                    d.settings.set_plugin(id, patch);
                }
                ("clamshell", true) => {
                    d.settings.set_plugin(id, patch);
                    Self::activate_clamshell(d);
                }
                ("clamshell", false) => {
                    // Close runtime admission and reconcile the exact helper
                    // lease before persisting the disabled setting. A failed
                    // cleanup remains visible and is returned to the caller.
                    let outcome = Self::deactivate_clamshell(d);
                    d.settings.set_plugin(id, patch);
                    changed(d);
                    return clamshell_disable_response(outcome);
                }
                _ => unreachable!(),
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
                text: "Авто при работе агентов".into(),
                checked: s["autoArm"].as_bool().unwrap_or(false),
                enabled: true,
            });
            out.push(TrayItem::Check {
                id: "cs:set-suggest".into(),
                text: "Подсказывать после прерванного сна".into(),
                checked: s["suggest"].as_bool().unwrap_or(false),
                enabled: true,
            });
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

fn power_profile_id_for(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Stable FNV-1a/128 keeps arbitrary filesystem bytes outside the closed
    // helper identifier grammar while making profile collisions negligible.
    let mut hash = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d_u128;
    for byte in canonical.as_os_str().as_bytes() {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    format!("jarvis-profile-{hash:032x}")
}

fn power_profile_id() -> String {
    power_profile_id_for(&jarvis_dir())
}

fn new_owner_generation() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_ms(),
        NEXT_OWNER_GENERATION.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
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

/// Связка clamshell ↔ keep-awake: авто-режим «Крышки» повторяет assertion.
/// Helper admission is checked by `arm`; an unavailable helper fails closed.
fn peer_sync(d: &Arc<Daemon>) {
    if !d.power.operations.accepting() {
        return;
    }
    let s = Power::cs_settings(d);
    let (active, busy, armed, armed_by, lease_pending, renewal_failed) = {
        let clam = d.power.clam.lock().unwrap();
        (
            clam.active,
            clam.busy,
            clam.armed,
            clam.armed_by,
            clam.lease.is_some(),
            clam.renewal_error.is_some(),
        )
    };
    if !active
        || busy
        || (!armed && (lease_pending || renewal_failed))
        || s["autoArm"].as_bool() != Some(true)
    {
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
    Power::reap_finished_clamshell_renewal(d);
    let Some(operation) = d.power.operations.begin() else {
        return json!({ "ok": false, "error": "Jarvis завершает работу или power занят" });
    };
    {
        let mut clam = d.power.clam.lock().unwrap();
        if !clam.active {
            return json!({ "ok": false, "error": "плагин выключен" });
        }
        if clam.busy {
            return json!({ "ok": false, "error": "операция уже идёт" });
        }
        if clam.armed {
            return json!({ "ok": true });
        }
        if clam.lease.is_some() {
            return json!({
                "ok": false,
                "error": "предыдущая helper lease ожидает точного release; повторный arm запрещён"
            });
        }
        clam.busy = true;
    }
    let owner_generation = new_owner_generation();
    let profile = power_profile_id();
    let worker_daemon = d.clone();
    let commit_daemon = d.clone();
    let rollback_daemon = d.clone();
    let lease_client = d.power.lease_client.clone();
    let rollback_client = lease_client.clone();
    let acquired = tauri::async_runtime::spawn_blocking(move || {
        let worker_operations = &worker_daemon.power.operations;
        let operation_epoch = operation.epoch();
        if !worker_operations.accepts(operation_epoch) {
            return Ok(AcquireDisposition::RolledBack);
        }
        let acquire_result = lease_client.acquire(&profile, &owner_generation);
        worker_operations.finish_acquire(
            operation,
            acquire_result,
            move |receipt| {
                let mut clam = commit_daemon.power.clam.lock().unwrap();
                if commit_daemon.power.operations.accepts(operation_epoch) {
                    clam.commit_acquired_if_active(receipt, by)
                } else {
                    Err(receipt)
                }
            },
            move |receipt| {
                match ExactReleaseOutcome::from_result(rollback_client.release(&receipt)) {
                    ExactReleaseOutcome::Confirmed | ExactReleaseOutcome::AlreadyAbsent(_) => {
                        Ok(())
                    }
                    ExactReleaseOutcome::Retryable(error) => {
                        let mut clam = rollback_daemon.power.clam.lock().unwrap();
                        // Preserve the exact late receipt so a repeated shutdown
                        // cleanup can retry; never convert it into a new acquire.
                        clam.retain_lease_debt(receipt, error.to_string());
                        Err(error)
                    }
                }
            },
        )
    })
    .await;
    d.power.clam.lock().unwrap().busy = false;
    let result = match acquired {
        Ok(Ok(AcquireDisposition::Committed)) => match Power::start_clamshell_renewal(d) {
            Ok(()) => {
                changed(d);
                json!({ "ok": true })
            }
            Err(error) => {
                let error = Power::release_after_renewal_start_failure(d, &error);
                json!({ "ok": false, "error": error })
            }
        },
        other => {
            let error = match other {
                Ok(Ok(AcquireDisposition::RolledBack)) => {
                    "arm отменён: плагин выключен или Jarvis завершает работу".into()
                }
                Ok(Err(error)) => error.to_string(),
                Err(error) => format!("power worker failed: {error}"),
                Ok(Ok(AcquireDisposition::Committed)) => unreachable!(),
            };
            if d.power.clam.lock().unwrap().lease.is_some() {
                let error = Power::release_after_renewal_start_failure(d, &error);
                json!({ "ok": false, "error": error })
            } else {
                json!({ "ok": false, "error": error })
            }
        }
    };
    result
}

async fn disarm(d: &Arc<Daemon>) -> Value {
    let (receipt, renewal) = {
        let mut clam = d.power.clam.lock().unwrap();
        if clam.busy {
            return json!({ "ok": false, "error": "операция уже идёт" });
        }
        let Some(receipt) = clam.lease.clone() else {
            clam.armed = false;
            clam.armed_by = None;
            return json!({ "ok": true });
        };
        clam.busy = true;
        (receipt, clam.renewal.take())
    };
    if let Some(renewal) = renewal {
        renewal.stop();
    }
    let Some(operation) = d
        .power
        .operations
        .begin_wait(POWER_OPERATION_BARRIER_TIMEOUT)
    else {
        let error = "Jarvis завершает работу или power занят после остановки renewal";
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.busy = false;
            clam.mark_lease_unrenewable(&receipt, error);
        }
        if d.power.operations.accepting() {
            changed(d);
        }
        return json!({ "ok": false, "error": error });
    };
    let lease_client = d.power.lease_client.clone();
    let worker_receipt = receipt.clone();
    let released = tauri::async_runtime::spawn_blocking(move || {
        let result = lease_client.release(&worker_receipt);
        (operation, result)
    })
    .await;
    let (returned_operation, release) = match released {
        Ok(result) => result,
        Err(error) => {
            let error = format!("power worker failed after stopping renewal: {error}");
            {
                let mut clam = d.power.clam.lock().unwrap();
                clam.busy = false;
                clam.mark_lease_unrenewable(&receipt, error.clone());
            }
            if d.power.operations.accepting() {
                changed(d);
            }
            return json!({
                "ok": false,
                "error": error,
            });
        }
    };
    let release = ExactReleaseOutcome::from_result(release);
    let mut restart_renewal = false;
    let result = match release {
        ExactReleaseOutcome::Confirmed | ExactReleaseOutcome::AlreadyAbsent(_) => {
            let mut clam = d.power.clam.lock().unwrap();
            if clam.lease.as_ref() == Some(&receipt) {
                clam.armed = false;
                clam.armed_by = None;
                clam.lease = None;
                clam.renewal_error = None;
            }
            drop(clam);
            changed(d);
            json!({ "ok": true })
        }
        ExactReleaseOutcome::Retryable(error) => {
            let mut clam = d.power.clam.lock().unwrap();
            if clam.lease.as_ref() == Some(&receipt) {
                clam.renewal_error = Some(error.to_string());
                restart_renewal = clam.active && clam.armed && d.power.operations.accepting();
                if !restart_renewal {
                    clam.mark_lease_unrenewable(&receipt, error.to_string());
                }
            }
            json!({ "ok": false, "error": error.to_string() })
        }
    };
    d.power.clam.lock().unwrap().busy = false;
    drop(returned_operation);
    if restart_renewal {
        if let Err(error) = Power::start_clamshell_renewal(d) {
            let error = Power::release_after_renewal_start_failure(d, &error);
            if d.power.operations.accepting() {
                changed(d);
            }
            return json!({ "ok": false, "error": error });
        }
    }
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
    let (receipt, renewal) = {
        let mut clam = d.power.clam.lock().unwrap();
        if clam.busy {
            return;
        }
        let Some(receipt) = clam.lease.clone() else {
            return;
        };
        clam.busy = true;
        (receipt, clam.renewal.take())
    };
    if let Some(renewal) = renewal {
        renewal.stop();
    }
    let Some(operation) = d
        .power
        .operations
        .begin_wait(POWER_OPERATION_BARRIER_TIMEOUT)
    else {
        let error = "battery release admission failed after stopping renewal";
        {
            let mut clam = d.power.clam.lock().unwrap();
            clam.busy = false;
            clam.mark_lease_unrenewable(&receipt, error);
        }
        if d.power.operations.accepting() {
            changed(d);
            d.notify(
                "⌒ Крышка: батарея садится",
                &format!("Осталось {pct}% — {error}; принудительно усыпляю мак"),
                None,
                "done",
            );
            // Non-privileged safety action only; never writes disablesleep.
            clamshell::force_sleep_now().await;
        }
        return;
    };
    println!("[jarvis:clamshell] батарея {pct}% ≤ {floor}% — освобождаю ownership lease");
    let lease_client = d.power.lease_client.clone();
    let worker_receipt = receipt.clone();
    let released = tauri::async_runtime::spawn_blocking(move || {
        let result = lease_client.release(&worker_receipt);
        (operation, result)
    })
    .await;
    let (returned_operation, release) = match released {
        Ok(result) => result,
        Err(error) => {
            let error = format!("battery helper worker failed after stopping renewal: {error}");
            {
                let mut clam = d.power.clam.lock().unwrap();
                clam.busy = false;
                clam.mark_lease_unrenewable(&receipt, error.clone());
            }
            if d.power.operations.accepting() {
                changed(d);
                d.notify(
                    "⌒ Крышка: батарея садится",
                    &format!("Осталось {pct}% — {error}; принудительно усыпляю мак"),
                    None,
                    "done",
                );
                // Non-privileged safety action only; never writes disablesleep.
                clamshell::force_sleep_now().await;
            }
            eprintln!("[jarvis:clamshell] battery helper worker failed: {error}");
            return;
        }
    };
    let release = ExactReleaseOutcome::from_result(release);
    {
        let mut clam = d.power.clam.lock().unwrap();
        match release {
            ExactReleaseOutcome::Confirmed | ExactReleaseOutcome::AlreadyAbsent(_) => {
                if clam.lease.as_ref() == Some(&receipt) {
                    clam.armed = false;
                    clam.armed_by = None;
                    clam.lease = None;
                    clam.renewal_error = None;
                }
            }
            ExactReleaseOutcome::Retryable(error) => {
                // Renewal remains stopped, so helper TTL is the backstop.
                // Keep the receipt for shutdown release retry.
                clam.mark_lease_unrenewable(&receipt, error.to_string());
            }
        }
        clam.busy = false;
    }
    drop(returned_operation);
    let decision = battery_guard_decision(pct, &release);
    if d.power.operations.accepting() {
        changed(d);
        d.notify("⌒ Крышка: батарея садится", &decision.message, None, "done");
        if decision.force_sleep {
            // A Released response is per-receipt, not proof that the global
            // baseline is sleep-enabled. Always use the non-privileged action.
            clamshell::force_sleep_now().await;
        }
    }
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
/// TASK7 RELEASE BLOCKER: this legacy command remains tracked only for the
/// immediate one-way v1 migration task and must not ship in a v2 release.
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

    fn helper_receipt() -> LeaseReceipt {
        LeaseReceipt {
            lease_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            owner_generation: "g".into(),
        }
    }

    fn other_helper_receipt() -> LeaseReceipt {
        LeaseReceipt {
            lease_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            owner_generation: "other".into(),
        }
    }

    #[test]
    fn clamshell_transition_fences_arm_until_the_serialized_toggle_finishes() {
        let receipt = helper_receipt();
        let mut clam = Clam::default();
        let transition = clam.begin_transition();

        assert!(clam.transitioning);
        assert_eq!(
            clam.commit_acquired_if_active(receipt.clone(), "manual"),
            Err(receipt)
        );
        assert!(clam.set_transition_active(transition, true));
        assert!(!clam.finish_transition(transition.wrapping_sub(1)));
        assert!(
            clam.transitioning,
            "stale completion must not open the fence"
        );
        assert!(clam.finish_transition(transition));
        assert!(clam.active);
        assert!(!clam.transitioning);

        let source = include_str!("mod.rs");
        let enable = source
            .split_once("async fn set_clamshell_enabled")
            .expect("serialized clamshell toggle")
            .1
            .split_once("fn deactivate_clamshell")
            .expect("toggle boundary")
            .0;
        assert!(enable.contains("clam_transition.lock().await"));
        assert!(enable.contains("persisted_clamshell_enabled"));
    }

    #[test]
    fn mismatched_receipt_debt_is_a_full_noop() {
        let current = other_helper_receipt();
        let stale = helper_receipt();
        let mut clam = Clam {
            active: true,
            armed: true,
            armed_by: Some("manual"),
            lease: Some(current.clone()),
            renewal_error: Some("current error".into()),
            ..Clam::default()
        };

        assert!(!clam.retain_lease_debt(stale, "stale error"));
        assert!(clam.active);
        assert!(clam.armed);
        assert_eq!(clam.armed_by, Some("manual"));
        assert_eq!(clam.lease.as_ref(), Some(&current));
        assert_eq!(clam.renewal_error.as_deref(), Some("current error"));
    }

    #[test]
    fn retained_debt_and_terminal_loss_remain_battery_guarded() {
        let receipt = helper_receipt();
        let mut clam = Clam {
            lease: Some(receipt.clone()),
            armed: false,
            last_guard_at: 0,
            ..Clam::default()
        };

        assert!(clam.needs_battery_guard(GUARD_EVERY_MS));
        assert!(clam.mark_terminal_lease_loss(&receipt, "lease expired"));
        assert!(clam.lease.is_none());
        assert!(clam.safety_sleep_pending);
        assert!(clam.needs_battery_guard(GUARD_EVERY_MS));
    }

    #[test]
    fn unknown_acquire_retries_the_same_generation_with_backoff_until_ttl() {
        let mut pending = UnknownAcquire::new(
            "profile".into(),
            "generation-a".into(),
            "manual",
            10_000,
            "first response timed out".into(),
        );

        assert!(pending.blocks_blind_retry(10_000));
        assert!(!pending.retry_due(pending.next_retry_at - 1));
        assert!(pending.retry_due(pending.next_retry_at));
        let generation = pending.owner_generation.clone();
        let first_retry_at = pending.next_retry_at;
        assert!(pending.record_retry_failure(first_retry_at, "still unknown".into()));
        assert_eq!(pending.owner_generation, generation);
        assert!(pending.next_retry_at > first_retry_at);
        assert!(!pending.blocks_blind_retry(pending.expires_at));
    }

    #[test]
    fn cleanup_debt_has_truthful_tray_status_and_safe_retry_action() {
        let clam = Clam {
            active: false,
            lease: Some(helper_receipt()),
            renewal_error: Some("helper unavailable".into()),
            ..Clam::default()
        };

        let status = clamshell_tray_status(&clam);
        assert!(status.contains("освобождение"));
        assert!(!status.contains("уснёт"));
        assert!(clam.can_retry_cleanup());

        let source = include_str!("mod.rs");
        assert!(source.contains("\"retry-cleanup\" => retry_clamshell_cleanup"));
        assert!(source.contains("\"cs:retry-cleanup\""));
    }

    #[test]
    fn disabled_clamshell_rejects_late_acquire_and_keeps_cleanup_debt_visible() {
        let receipt = helper_receipt();
        let mut clam = Clam {
            active: false,
            busy: true,
            ..Clam::default()
        };

        assert_eq!(
            clam.commit_acquired_if_active(receipt.clone(), "manual"),
            Err(receipt.clone())
        );
        assert!(!clam.armed);
        assert!(clam.lease.is_none());

        clam.retain_lease_debt(receipt.clone(), "exact release failed");
        assert_eq!(clam.lease.as_ref(), Some(&receipt));
        assert!(!clam.armed);
        assert!(clam.has_visible_status());
    }

    #[test]
    fn stopped_or_dead_renewal_never_leaves_a_truthful_armed_state() {
        let receipt = helper_receipt();
        let mut clam = Clam {
            active: true,
            armed: true,
            armed_by: Some("manual"),
            lease: Some(receipt.clone()),
            ..Clam::default()
        };

        clam.mark_lease_unrenewable(&receipt, "renewal worker died");

        assert!(!clam.armed);
        assert_eq!(clam.armed_by, None);
        assert_eq!(clam.lease.as_ref(), Some(&receipt));
        assert_eq!(clam.renewal_error.as_deref(), Some("renewal worker died"));
        assert!(clam.has_visible_status());
    }

    #[test]
    fn disabled_cleanup_failure_is_propagated_instead_of_hidden() {
        let response = clamshell_disable_response(ClamshellDisposeOutcome::ReleaseFailed(
            "helper unavailable".into(),
        ));

        assert_eq!(response["ok"], false);
        assert_eq!(response["pendingCleanup"], true);
        assert!(response["error"]
            .as_str()
            .is_some_and(|error| error.contains("helper unavailable")));

        let source = include_str!("mod.rs");
        let disable_path = source
            .split_once("(\"clamshell\", false) =>")
            .expect("clamshell disable path")
            .1
            .split_once("_ => unreachable!()")
            .expect("disable path boundary")
            .0;
        assert!(
            disable_path.find("deactivate_clamshell").unwrap()
                < disable_path.find("settings.set_plugin").unwrap(),
            "runtime cleanup must run before persisting disabled"
        );
        assert!(disable_path.contains("clamshell_disable_response(outcome)"));
    }

    #[test]
    fn low_battery_forces_nonprivileged_sleep_for_every_release_outcome() {
        use helper::renewal::{ExactReleaseOutcome, LeaseError};
        use jarvis_power_core::protocol::ErrorCode;

        for outcome in [
            ExactReleaseOutcome::Confirmed,
            ExactReleaseOutcome::AlreadyAbsent(ErrorCode::LeaseExpired),
            ExactReleaseOutcome::Retryable(LeaseError::HelperUnavailable),
        ] {
            let decision = battery_guard_decision(9, &outcome);
            assert!(decision.force_sleep);
            assert!(decision.message.contains("принудительно усыпляю"));
        }
    }

    #[test]
    fn arm_reconciles_any_post_acquire_renewal_start_failure() {
        let source = include_str!("mod.rs");
        let arm = source
            .split_once("async fn arm")
            .expect("arm boundary")
            .1
            .split_once("async fn disarm")
            .expect("disarm boundary")
            .0;

        assert!(arm.contains("commit_acquired_if_active"));
        assert!(arm.contains("release_after_renewal_start_failure"));
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
    fn helper_release_report_requires_exact_success() {
        assert!(!release_was_confirmed(clamshell::ReleaseOutcome::NotOwned));
        assert!(PowerDisposeReport {
            clamshell: ClamshellDisposeOutcome::Released,
        }
        .released());
        assert!(!PowerDisposeReport {
            clamshell: ClamshellDisposeOutcome::ReleaseFailed("unavailable".into()),
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
    fn helper_profile_id_is_stable_bounded_and_protocol_safe() {
        let profile = power_profile_id_for(Path::new("/tmp/jarvis profile/a"));
        assert_eq!(
            profile,
            power_profile_id_for(Path::new("/tmp/jarvis profile/a"))
        );
        assert_ne!(
            profile,
            power_profile_id_for(Path::new("/tmp/jarvis profile/b"))
        );
        assert!(profile.len() <= 128);
        assert!(jarvis_power_core::protocol::Request::AcquireLease {
            profile,
            owner_generation: "g".into(),
            ttl_ms: jarvis_power_core::protocol::DEFAULT_TTL_MS,
        }
        .validate()
        .is_ok());
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
                    Ok::<_, ()>(LeaseReceipt {
                        lease_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                        owner_generation: "g".into(),
                    }),
                    |_| panic!("closed acquire must not commit"),
                    |receipt| {
                        assert_eq!(receipt.lease_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                        assert_eq!(receipt.owner_generation, "g");
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
                    Ok(())
                },
                |_| panic!("accepted acquire must not roll back"),
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
            |_| Err("rollback failed"),
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
