//! «Крышка»: closed-display mode — мак работает с закрытой крышкой.
//!
//! Механика: `pmset -a disablesleep 1` — запрет на уровне IOPMrootDomain,
//! выше категорий idle/forced. Это root-уровень и термо-риски, поэтому
//! политика плагина: включать persistent override только после доказанного
//! non-interactive rollback. Тихое переключение доступно после явного опт-ина:
//! установки /etc/sudoers.d/jarvis-pmset (ровно две команды pmset).
//!
//! Fail-safe (урок Amphetamine Enhancer — «мак не должен зажариться в рюкзаке»):
//!   1) machine-global registry пишет baseline до mutation и fsync-ит его;
//!   2) каждый профиль держит отдельную lease, last lease восстанавливает baseline;
//!   3) dispose/квит освобождает только доказанную Jarvis-owned mutation;
//!   4) батарейный сторож: armed + батарея ≤ floor → тихий сброс, нельзя
//!      тихо → pmset sleepnow (форс-сон без root: лучше уснуть, чем зажариться;
//!      admin-диалог под закрытой крышкой никто не увидит — его не зовём).
//! Старый ~/.jarvis/clamshell.json не содержал baseline, поэтому он только
//! сигнализирует blocked repair и никогда не разрешает автоматический pmset 0.

use std::fmt;
use std::io;
use std::process::{Command, Output, Stdio};
use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use crate::power::ownership::{Lease, OwnershipState, ReleaseDecision};
use crate::power::ownership_store::{OwnershipStore, StoreError};
use crate::util::{jarvis_dir, now_ms};

pub const SUDOERS: &str = "/etc/sudoers.d/jarvis-pmset";

/* ================= чистое ядро: парсеры и решения ================= */

#[derive(Debug, PartialEq)]
pub struct LidState {
    pub present: bool,
    pub closed: Option<bool>,
    pub causes_sleep: Option<bool>,
}

/// ioreg -r -k AppleClamshellState -d 4 → состояние крышки.
/// causesSleep учитывает и родной clamshell-режим, и disablesleep —
/// macOS сама говорит, уснёт ли мак от закрытия крышки прямо сейчас.
pub fn parse_clamshell_state(out: &str) -> LidState {
    let grab = |key: &str| -> Option<bool> {
        let re = regex::RegexBuilder::new(&format!(r#""{key}"\s*=\s*(Yes|No)"#))
            .case_insensitive(true)
            .build()
            .unwrap();
        re.captures(out).map(|c| c[1].eq_ignore_ascii_case("yes"))
    };
    let closed = grab("AppleClamshellState");
    let causes_sleep = grab("AppleClamshellCausesSleep");
    LidState {
        present: closed.is_some() || causes_sleep.is_some(),
        closed,
        causes_sleep,
    }
}

/// pmset -g → стоит ли сейчас флаг disablesleep (строка SleepDisabled).
pub fn parse_sleep_disabled(out: &str) -> Option<bool> {
    regex::Regex::new(r"SleepDisabled\s+(\d)")
        .unwrap()
        .captures(out)
        .map(|c| &c[1] == "1")
}

#[derive(Debug, PartialEq)]
pub struct Battery {
    pub pct: Option<u32>,
    pub on_battery: Option<bool>,
    pub charging: Option<bool>,
}

/// pmset -g batt → процент и источник питания (десктоп без батареи → None).
pub fn parse_battery(out: &str) -> Battery {
    let pct = regex::Regex::new(r"(\d{1,3})%")
        .unwrap()
        .captures(out)
        .and_then(|c| c[1].parse::<u32>().ok())
        .map(|p| p.min(100));
    let on_battery = regex::Regex::new(r"Now drawing from '([^']+)'")
        .unwrap()
        .captures(out)
        .map(|c| c[1].to_lowercase().contains("battery"));
    let charging = if regex::RegexBuilder::new(r";\s*charging")
        .case_insensitive(true)
        .build()
        .unwrap()
        .is_match(out)
    {
        Some(true)
    } else if out.to_lowercase().contains("discharging") {
        Some(false)
    } else {
        None
    };
    Battery {
        pct,
        on_battery,
        charging,
    }
}

#[derive(Debug, PartialEq)]
pub enum Suggest {
    No,
    /// Предложить disablesleep.
    Arm,
    /// Есть внешний дисплей — рассказать про родной clamshell-режим (root не нужен).
    Native,
}

/// Проснулись после сна: предлагать ли closed-display?
pub fn decide_suggest(
    working_at_sleep: usize,
    armed: bool,
    external_display: bool,
    last_suggest_at: i64,
    now: i64,
    min_gap_ms: i64,
) -> Suggest {
    if working_at_sleep == 0 || armed {
        return Suggest::No;
    }
    if now - last_suggest_at < min_gap_ms {
        return Suggest::No;
    }
    if external_display {
        Suggest::Native
    } else {
        Suggest::Arm
    }
}

/// /etc/sudoers.d/jarvis-pmset: тихий доступ ровно к двум командам.
/// Имя юзера валидируем жёстко — содержимое уходит в sudoers.
pub fn sudoers_content(user: &str) -> Result<String, String> {
    let valid = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*$").unwrap();
    if !valid.is_match(user) {
        return Err(format!(
            "недопустимое имя пользователя для sudoers: {user:?}"
        ));
    }
    Ok([
        "# Jarvis: тихое переключение closed-display mode (плагин clamshell).",
        "# Разрешает БЕЗ пароля ровно две команды — включить/выключить disablesleep.",
        &format!("{user} ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0"),
        "",
    ]
    .join("\n"))
}

/* ================= durable ownership transaction ================= */

#[derive(Debug)]
pub enum PowerError {
    Store(StoreError),
    Io(io::Error),
    Command(String),
    Timeout(String),
    InvalidState(String),
    RollbackUnavailable,
    VerificationFailed { expected: bool, actual: bool },
    RollbackFailed(String),
}

impl fmt::Display for PowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "power command I/O failed: {error}"),
            Self::Command(error) => write!(formatter, "{error}"),
            Self::Timeout(command) => write!(formatter, "power command timed out: {command}"),
            Self::InvalidState(error) => write!(formatter, "power ownership is ambiguous: {error}"),
            Self::RollbackUnavailable => {
                write!(formatter, "non-interactive sleep rollback is unavailable")
            }
            Self::VerificationFailed { expected, actual } => write!(
                formatter,
                "power state verification failed: expected {}, got {}",
                u8::from(*expected),
                u8::from(*actual)
            ),
            Self::RollbackFailed(error) => {
                write!(formatter, "power rollback could not be confirmed: {error}")
            }
        }
    }
}

impl std::error::Error for PowerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreError> for PowerError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<io::Error> for PowerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub trait PmsetBackend: Send + Sync {
    fn read_disabled(&self) -> Result<bool, PowerError>;
    fn can_restore_noninteractive(&self) -> bool;
    fn set_disabled(&self, value: bool) -> Result<(), PowerError>;
    fn boot_id(&self) -> Result<String, PowerError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    Mutated,
    Joined,
    BaselineAlreadyOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    NotOwned,
    KeptApplied,
    BaselineUnchanged(bool),
    Restored(bool),
}

impl ReleaseOutcome {
    pub fn sleep_disabled(self) -> Option<bool> {
        match self {
            Self::NotOwned => None,
            Self::KeptApplied => Some(true),
            Self::BaselineUnchanged(value) | Self::Restored(value) => Some(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMarkerDecision {
    ClearMarker,
    BlockedRepair,
}

#[derive(Debug, PartialEq)]
pub enum LegacyMarkerState {
    Missing,
    Present(serde_json::Value),
    Corrupt,
}

pub fn decide_legacy_marker(current: Option<bool>) -> LegacyMarkerDecision {
    if current == Some(false) {
        LegacyMarkerDecision::ClearMarker
    } else {
        LegacyMarkerDecision::BlockedRepair
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPmset;

impl PmsetBackend for SystemPmset {
    fn read_disabled(&self) -> Result<bool, PowerError> {
        let output = bounded_output("/usr/bin/pmset", &["-g"], Duration::from_secs(4))?;
        if !output.status.success() {
            return Err(command_error("pmset -g", &output));
        }
        parse_sleep_disabled(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| PowerError::InvalidState("pmset -g did not report SleepDisabled".into()))
    }

    fn can_restore_noninteractive(&self) -> bool {
        if !sudoers_installed() {
            return false;
        }
        bounded_output(
            "/usr/bin/sudo",
            &["-n", "-l", "/usr/bin/pmset", "-a", "disablesleep", "0"],
            Duration::from_secs(4),
        )
        .is_ok_and(|output| output.status.success())
    }

    fn set_disabled(&self, value: bool) -> Result<(), PowerError> {
        let output = bounded_output(
            "/usr/bin/sudo",
            &[
                "-n",
                "/usr/bin/pmset",
                "-a",
                "disablesleep",
                if value { "1" } else { "0" },
            ],
            Duration::from_secs(8),
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("sudo -n pmset", &output))
        }
    }

    fn boot_id(&self) -> Result<String, PowerError> {
        let output = bounded_output(
            "/usr/sbin/sysctl",
            &["-n", "kern.boottime"],
            Duration::from_secs(3),
        )?;
        if !output.status.success() {
            return Err(command_error("sysctl kern.boottime", &output));
        }
        let boot_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if boot_id.is_empty() {
            Err(PowerError::InvalidState(
                "sysctl returned an empty boot identity".into(),
            ))
        } else {
            Ok(boot_id)
        }
    }
}

fn bounded_output(program: &str, args: &[&str], timeout: Duration) -> Result<Output, PowerError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(PowerError::Io);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PowerError::Timeout(format!("{program} {}", args.join(" "))));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn command_error(command: &str, output: &Output) -> PowerError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    PowerError::Command(if detail.is_empty() {
        format!("{command} exited with {}", output.status)
    } else {
        format!("{command} failed: {detail}")
    })
}

/// Acquire one logical profile lease while holding the global registry lock
/// across baseline read, durable write-ahead, mutation, and read-back.
pub fn acquire_with<B: PmsetBackend>(
    backend: &B,
    store: &OwnershipStore,
    lease: Lease,
) -> Result<AcquireOutcome, PowerError> {
    let guard = store.lock()?;
    if let Some(mut state) = guard.read()? {
        validate_ownership_state(&state)?;
        if state.leases.is_empty() {
            return Err(PowerError::InvalidState(
                "a pending restore must complete before another acquire".into(),
            ));
        }
        let current_boot = backend.boot_id()?;
        if state.boot_id != current_boot {
            return Err(PowerError::InvalidState(format!(
                "registry boot {} does not match current boot {}",
                state.boot_id, current_boot
            )));
        }
        let current = backend.read_disabled()?;
        if current != state.applied {
            return Err(PowerError::InvalidState(format!(
                "registry says applied={}, system reports {}",
                state.applied, current
            )));
        }
        if state.did_mutate && !backend.can_restore_noninteractive() {
            return Err(PowerError::RollbackUnavailable);
        }
        state.acquire(lease);
        guard.write(&state)?;
        return Ok(AcquireOutcome::Joined);
    }

    let baseline = backend.read_disabled()?;
    if !baseline && !backend.can_restore_noninteractive() {
        return Err(PowerError::RollbackUnavailable);
    }
    let mut state = OwnershipState::new(
        baseline,
        backend.boot_id()?,
        u64::try_from(now_ms()).unwrap_or_default(),
    );
    state.acquire(lease);
    guard.write(&state)?;

    if baseline {
        return Ok(AcquireOutcome::BaselineAlreadyOn);
    }

    let mutation_result = backend
        .set_disabled(true)
        .and_then(|()| verify_state(backend, true));
    match mutation_result {
        Ok(()) => Ok(AcquireOutcome::Mutated),
        Err(primary) => Err(rollback_acquire(backend, &guard, baseline, primary)),
    }
}

/// Release only the exact profile/generation lease. The last Jarvis-owned
/// mutating lease restores the recorded baseline and clears the registry only
/// after read-back confirms that restore.
pub fn release_with<B: PmsetBackend>(
    backend: &B,
    store: &OwnershipStore,
    profile: &str,
    owner_generation: &str,
) -> Result<ReleaseOutcome, PowerError> {
    let guard = store.lock()?;
    let Some(mut state) = guard.read()? else {
        return Ok(ReleaseOutcome::NotOwned);
    };
    validate_ownership_state(&state)?;
    if state.boot_id != backend.boot_id()? {
        return Err(PowerError::InvalidState(
            "refusing to release a lease from another boot".into(),
        ));
    }

    // A previous last-owner release may have durably removed the lease and
    // then failed during read/preflight/set/read-back. That zero-lease record
    // is a restore obligation, not "not owned"; every retry must resume it.
    if state.leases.is_empty() {
        return if state.did_mutate {
            restore_and_clear(backend, &guard, state.baseline)
        } else {
            guard.clear()?;
            Ok(ReleaseOutcome::BaselineUnchanged(state.baseline))
        };
    }

    if !state
        .leases
        .iter()
        .any(|lease| lease.profile == profile && lease.owner_generation == owner_generation)
    {
        return Ok(ReleaseOutcome::NotOwned);
    }

    let outcome = match state.release(profile, owner_generation) {
        ReleaseDecision::KeepApplied => {
            guard.write(&state)?;
            ReleaseOutcome::KeptApplied
        }
        ReleaseDecision::ClearWithoutMutation => {
            guard.clear()?;
            ReleaseOutcome::BaselineUnchanged(state.baseline)
        }
        ReleaseDecision::Restore(baseline) => {
            // Persist the zero-lease restore obligation before touching pmset.
            guard.write(&state)?;
            return restore_and_clear(backend, &guard, baseline);
        }
    };
    Ok(outcome)
}

fn restore_and_clear<B: PmsetBackend>(
    backend: &B,
    guard: &crate::power::ownership_store::OwnershipStoreGuard<'_>,
    baseline: bool,
) -> Result<ReleaseOutcome, PowerError> {
    let current = backend.read_disabled()?;
    if current != baseline {
        if !backend.can_restore_noninteractive() {
            return Err(PowerError::RollbackUnavailable);
        }
        backend.set_disabled(baseline)?;
        verify_state(backend, baseline)?;
    }
    guard.clear()?;
    Ok(ReleaseOutcome::Restored(baseline))
}

fn verify_state<B: PmsetBackend>(backend: &B, expected: bool) -> Result<(), PowerError> {
    let actual = backend.read_disabled()?;
    if actual == expected {
        Ok(())
    } else {
        Err(PowerError::VerificationFailed { expected, actual })
    }
}

fn validate_ownership_state(state: &OwnershipState) -> Result<(), PowerError> {
    if state.boot_id.trim().is_empty() {
        return Err(PowerError::InvalidState(
            "registry boot identity is empty".into(),
        ));
    }
    if !state.applied {
        return Err(PowerError::InvalidState(
            "registry does not describe an applied state".into(),
        ));
    }
    if state.did_mutate != !state.baseline {
        return Err(PowerError::InvalidState(format!(
            "baseline={} conflicts with didMutate={}",
            state.baseline, state.did_mutate
        )));
    }
    let mut owners = HashSet::new();
    for lease in &state.leases {
        if lease.profile.trim().is_empty()
            || lease.process_identity.trim().is_empty()
            || lease.owner_generation.trim().is_empty()
            || lease.pid == 0
            || lease.expires_at_ms <= lease.acquired_at_ms
        {
            return Err(PowerError::InvalidState(
                "registry contains a malformed lease".into(),
            ));
        }
        if !owners.insert((&lease.profile, &lease.owner_generation)) {
            return Err(PowerError::InvalidState(
                "registry contains duplicate lease owners".into(),
            ));
        }
    }
    Ok(())
}

fn rollback_acquire<B: PmsetBackend>(
    backend: &B,
    guard: &crate::power::ownership_store::OwnershipStoreGuard<'_>,
    baseline: bool,
    primary: PowerError,
) -> PowerError {
    let rollback = backend
        .set_disabled(baseline)
        .and_then(|()| verify_state(backend, baseline));
    match rollback {
        Ok(()) => match guard.clear() {
            Ok(()) => primary,
            Err(error) => PowerError::RollbackFailed(format!(
                "baseline restored but ownership record could not be cleared: {error}"
            )),
        },
        Err(error) => PowerError::RollbackFailed(format!("{primary}; rollback error: {error}")),
    }
}

/* ================= системные обвязки ================= */

async fn run(cmd: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let mut c = tokio::process::Command::new(cmd);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(timeout, c.output()).await.ok()?.ok()
}

pub fn sudoers_installed() -> bool {
    std::path::Path::new(SUDOERS).exists()
}

pub async fn read_sleep_disabled() -> Option<bool> {
    let out = run("pmset", &["-g"], Duration::from_secs(4)).await?;
    parse_sleep_disabled(&String::from_utf8_lossy(&out.stdout))
}

pub async fn read_battery() -> Battery {
    match run("pmset", &["-g", "batt"], Duration::from_secs(4)).await {
        Some(out) => parse_battery(&String::from_utf8_lossy(&out.stdout)),
        None => Battery {
            pct: None,
            on_battery: None,
            charging: None,
        },
    }
}

pub async fn read_lid() -> LidState {
    match run(
        "ioreg",
        &["-r", "-k", "AppleClamshellState", "-d", "4"],
        Duration::from_secs(4),
    )
    .await
    {
        Some(out) => parse_clamshell_state(&String::from_utf8_lossy(&out.stdout)),
        None => LidState {
            present: false,
            closed: None,
            causes_sleep: None,
        },
    }
}

pub async fn force_sleep_now() {
    run("pmset", &["sleepnow"], Duration::from_secs(4)).await;
}

/// MacBook Air без вентилятора — под крышкой троттлит, предупреждаем.
pub async fn detect_is_air() -> bool {
    run("sysctl", &["-n", "hw.model"], Duration::from_secs(3))
        .await
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("macbookair")
        })
}

/// Есть ли внешний дисплей (для подсказки про родной clamshell-режим).
pub fn external_display_present() -> bool {
    core_graphics::display::CGDisplay::active_displays()
        .map(|ids| {
            ids.iter()
                .any(|&id| !core_graphics::display::CGDisplay::new(id).is_builtin())
        })
        .unwrap_or(false)
}

/* ---- маркер fail-safe ---- */

fn marker_file() -> std::path::PathBuf {
    jarvis_dir().join("clamshell.json")
}

pub fn legacy_marker_state() -> LegacyMarkerState {
    let bytes = match std::fs::read(marker_file()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return LegacyMarkerState::Missing
        }
        Err(_) => return LegacyMarkerState::Corrupt,
    };
    parse_legacy_marker(&bytes)
}

fn parse_legacy_marker(bytes: &[u8]) -> LegacyMarkerState {
    match serde_json::from_slice(&bytes) {
        Ok(value) => LegacyMarkerState::Present(value),
        Err(_) => LegacyMarkerState::Corrupt,
    }
}

pub fn clear_marker() {
    let _ = std::fs::remove_file(marker_file());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::ownership::Lease;
    use crate::power::ownership_store::OwnershipStore;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_store(label: &str) -> (PathBuf, OwnershipStore) {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-clamshell-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let store = OwnershipStore::at(dir.join("ownership.json"));
        (dir, store)
    }

    fn test_lease(profile: &str, generation: &str) -> Lease {
        Lease {
            profile: profile.into(),
            pid: 42,
            process_identity: "test-process".into(),
            owner_generation: generation.into(),
            acquired_at_ms: 10,
            expires_at_ms: 20,
        }
    }

    struct FakePmset {
        current: Mutex<bool>,
        rollback_available: bool,
        trace: Arc<Mutex<Vec<String>>>,
        registry_path: Option<PathBuf>,
        fail_first_set: Mutex<bool>,
    }

    impl FakePmset {
        fn new(current: bool) -> Self {
            Self {
                current: Mutex::new(current),
                rollback_available: true,
                trace: Arc::new(Mutex::new(Vec::new())),
                registry_path: None,
                fail_first_set: Mutex::new(false),
            }
        }

        fn without_rollback(current: bool) -> Self {
            Self {
                rollback_available: false,
                ..Self::new(current)
            }
        }

        fn checking_write_ahead(current: bool, registry_path: PathBuf) -> Self {
            Self {
                registry_path: Some(registry_path),
                ..Self::new(current)
            }
        }

        fn failing_first_set(current: bool) -> Self {
            Self {
                fail_first_set: Mutex::new(true),
                ..Self::new(current)
            }
        }

        fn current(&self) -> bool {
            *self.current.lock().unwrap()
        }

        fn fail_next_set(&self) {
            *self.fail_first_set.lock().unwrap() = true;
        }
    }

    impl PmsetBackend for FakePmset {
        fn read_disabled(&self) -> Result<bool, PowerError> {
            let current = self.current();
            self.trace
                .lock()
                .unwrap()
                .push(format!("read:{}", u8::from(current)));
            Ok(current)
        }

        fn can_restore_noninteractive(&self) -> bool {
            let current = self.current();
            self.trace
                .lock()
                .unwrap()
                .push(format!("preflight:{}", u8::from(current)));
            self.rollback_available
        }

        fn set_disabled(&self, value: bool) -> Result<(), PowerError> {
            if value {
                if let Some(path) = &self.registry_path {
                    let bytes = std::fs::read(path)
                        .expect("write-ahead ownership state must exist before pmset mutation");
                    let state: crate::power::ownership::OwnershipState =
                        serde_json::from_slice(&bytes).unwrap();
                    assert!(!state.leases.is_empty());
                    self.trace.lock().unwrap().push("store:write".into());
                }
            }
            self.trace
                .lock()
                .unwrap()
                .push(format!("set:{}", u8::from(value)));
            let mut fail = self.fail_first_set.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(PowerError::Command("injected set failure".into()));
            }
            *self.current.lock().unwrap() = value;
            Ok(())
        }

        fn boot_id(&self) -> Result<String, PowerError> {
            Ok("boot-test".into())
        }
    }

    #[test]
    fn acquire_writes_registry_before_mutation() {
        let (dir, store) = test_store("write-ahead");
        let backend = FakePmset::checking_write_ahead(false, dir.join("ownership.json"));
        let trace = backend.trace.clone();

        let outcome = acquire_with(&backend, &store, test_lease("prod", "generation")).unwrap();

        assert_eq!(outcome, AcquireOutcome::Mutated);
        assert_eq!(
            trace.lock().unwrap().as_slice(),
            ["read:0", "preflight:0", "store:write", "set:1", "read:1"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquire_refuses_when_noninteractive_restore_is_unavailable() {
        let (dir, store) = test_store("no-rollback");
        let backend = FakePmset::without_rollback(false);

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("prod", "generation")),
            Err(PowerError::RollbackUnavailable)
        ));
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn baseline_on_does_not_write_zero_on_release() {
        let (dir, store) = test_store("baseline-on");
        let backend = FakePmset::new(true);

        assert_eq!(
            acquire_with(&backend, &store, test_lease("prod", "generation")).unwrap(),
            AcquireOutcome::BaselineAlreadyOn
        );
        assert_eq!(
            release_with(&backend, &store, "prod", "generation").unwrap(),
            ReleaseOutcome::BaselineUnchanged(true)
        );

        assert!(backend.current());
        assert!(!backend
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|step| step == "set:0"));
        assert!(store.read().unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn last_lease_alone_restores_mutated_baseline() {
        let (dir, store) = test_store("multi-lease");
        let backend = FakePmset::new(false);
        acquire_with(&backend, &store, test_lease("prod", "one")).unwrap();
        acquire_with(&backend, &store, test_lease("dev", "two")).unwrap();

        assert_eq!(
            release_with(&backend, &store, "prod", "one").unwrap(),
            ReleaseOutcome::KeptApplied
        );
        assert!(backend.current());
        assert!(store.read().unwrap().is_some());

        assert_eq!(
            release_with(&backend, &store, "dev", "two").unwrap(),
            ReleaseOutcome::Restored(false)
        );
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_mutation_restores_and_clears_only_after_readback() {
        let (dir, store) = test_store("rollback");
        let backend = FakePmset::failing_first_set(false);

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("prod", "generation")),
            Err(PowerError::Command(_))
        ));
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        assert!(backend
            .trace
            .lock()
            .unwrap()
            .windows(3)
            .any(|steps| steps == ["set:1", "set:0", "read:0"]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_last_release_is_resumed_from_zero_lease_tombstone() {
        let (dir, store) = test_store("release-retry");
        let backend = FakePmset::new(false);
        acquire_with(&backend, &store, test_lease("prod", "generation")).unwrap();
        backend.fail_next_set();

        assert!(matches!(
            release_with(&backend, &store, "prod", "generation"),
            Err(PowerError::Command(_))
        ));
        let pending = store.read().unwrap().unwrap();
        assert!(pending.leases.is_empty());
        assert!(pending.did_mutate);
        assert!(backend.current());

        assert_eq!(
            release_with(&backend, &store, "prod", "generation").unwrap(),
            ReleaseOutcome::Restored(false)
        );
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_registry_blocks_mutation() {
        let (dir, store) = test_store("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ownership.json"), b"{").unwrap();
        let backend = FakePmset::new(false);

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("prod", "generation")),
            Err(PowerError::Store(_))
        ));
        assert!(!backend.current());
        assert!(std::fs::read(dir.join("ownership.json"))
            .unwrap()
            .starts_with(b"{"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn semantically_invalid_registry_blocks_join_and_release() {
        let (dir, store) = test_store("invalid-semantics");
        let backend = FakePmset::new(true);
        let mut invalid =
            crate::power::ownership::OwnershipState::new(false, "boot-test", 1);
        invalid.did_mutate = false;
        invalid.acquire(test_lease("prod", "generation"));
        store.write(&invalid).unwrap();

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("dev", "other")),
            Err(PowerError::InvalidState(_))
        ));
        assert!(matches!(
            release_with(&backend, &store, "prod", "generation"),
            Err(PowerError::InvalidState(_))
        ));
        assert!(!backend
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|step| step.starts_with("set:")));
        assert!(store.read().unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_legacy_marker_is_not_the_same_as_missing() {
        let (dir, _) = test_store("legacy-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clamshell.json");
        std::fs::write(&path, b"{").unwrap();

        assert_eq!(
            parse_legacy_marker(&std::fs::read(&path).unwrap()),
            LegacyMarkerState::Corrupt
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ambiguous_legacy_marker_never_authorizes_disabling_sleep() {
        assert_eq!(
            decide_legacy_marker(Some(true)),
            LegacyMarkerDecision::BlockedRepair
        );
        assert_eq!(
            decide_legacy_marker(None),
            LegacyMarkerDecision::BlockedRepair
        );
        assert_eq!(
            decide_legacy_marker(Some(false)),
            LegacyMarkerDecision::ClearMarker
        );
    }

    #[test]
    fn bounded_command_collects_output_without_touching_power_state() {
        let output = bounded_output("/bin/echo", &["bounded"], Duration::from_secs(1)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "bounded");
    }

    #[test]
    fn ioreg_lid_states() {
        let open = "+-o IOPMrootDomain  <class IOPMrootDomain>\n  |   \"AppleClamshellCausesSleep\" = Yes\n  |   \"AppleClamshellState\" = No\n";
        let lid = parse_clamshell_state(open);
        assert_eq!(
            lid,
            LidState {
                present: true,
                closed: Some(false),
                causes_sleep: Some(true)
            }
        );

        let closed =
            "  |   \"AppleClamshellCausesSleep\" = No\n  |   \"AppleClamshellState\" = Yes";
        let lid = parse_clamshell_state(closed);
        assert_eq!(
            lid,
            LidState {
                present: true,
                closed: Some(true),
                causes_sleep: Some(false)
            }
        );

        assert!(
            !parse_clamshell_state("что-то без ключей").present,
            "нет ключей — крышки нет (десктоп)"
        );
    }

    #[test]
    fn pmset_sleep_disabled() {
        assert_eq!(
            parse_sleep_disabled(" SleepDisabled\t\t1\n standby 1"),
            Some(true)
        );
        assert_eq!(parse_sleep_disabled(" SleepDisabled\t\t0"), Some(false));
        assert_eq!(parse_sleep_disabled("мусор"), None);
    }

    #[test]
    fn battery_parsing() {
        let batt = parse_battery("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=23396451)\t37%; discharging; 4:27 remaining present: true");
        assert_eq!(batt.pct, Some(37));
        assert_eq!(batt.on_battery, Some(true));
        let ac = parse_battery("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t95%; charging; 0:40 remaining present: true");
        assert_eq!(ac.pct, Some(95));
        assert_eq!(ac.on_battery, Some(false));
        assert_eq!(parse_battery("garbage").pct, None, "десктоп без батареи");
    }

    #[test]
    fn suggest_decision_matrix() {
        let now = 10 * 3600 * 1000;
        let gap = 3600 * 1000;
        assert_eq!(decide_suggest(2, false, false, 0, now, gap), Suggest::Arm);
        assert_eq!(decide_suggest(2, false, true, 0, now, gap), Suggest::Native);
        assert_eq!(
            decide_suggest(0, false, false, 0, now, gap),
            Suggest::No,
            "работы не было — молчим"
        );
        assert_eq!(
            decide_suggest(2, true, false, 0, now, gap),
            Suggest::No,
            "уже armed — молчим"
        );
        assert_eq!(
            decide_suggest(2, false, false, now - 1000, now, gap),
            Suggest::No,
            "недавно подсказывали"
        );
    }

    #[test]
    fn sudoers_is_strict() {
        let content = sudoers_content("se.chernyshev").unwrap();
        assert!(content.contains("se.chernyshev ALL=(root) NOPASSWD:"));
        assert!(content.contains("/usr/bin/pmset -a disablesleep 1"));
        assert!(content.contains("/usr/bin/pmset -a disablesleep 0"));
        assert!(
            sudoers_content("user name; ALL").is_err(),
            "кривое имя не пролазит"
        );
    }
}
