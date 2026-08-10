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

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::process::{Command, Output, Stdio};
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

/// Process liveness is useful only when it also proves the exact process
/// incarnation. A PID by itself can be reused after a crash.
pub trait ProcessInspector: Send + Sync {
    fn start_identity(&self, pid: u32) -> Result<Option<String>, PowerError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcesses;

const PROCESS_IDENTITY_PREFIX: &str = "darwin-v1:uid=";
const PROCESS_STATUS_ZOMBIE: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessSnapshot {
    status: u32,
    uid: u32,
    start_sec: u64,
    start_usec: u64,
}

fn process_identity_from_snapshot(snapshot: ProcessSnapshot) -> Option<String> {
    (snapshot.status != PROCESS_STATUS_ZOMBIE).then(|| {
        format!(
            "{PROCESS_IDENTITY_PREFIX}{}:start={}.{}",
            snapshot.uid, snapshot.start_sec, snapshot.start_usec
        )
    })
}

pub(super) fn valid_process_identity(identity: &str) -> bool {
    let Some(rest) = identity.strip_prefix(PROCESS_IDENTITY_PREFIX) else {
        return false;
    };
    let Some((uid, start)) = rest.split_once(":start=") else {
        return false;
    };
    let Some((sec, usec)) = start.split_once('.') else {
        return false;
    };
    let (Ok(uid), Ok(sec), Ok(usec)) =
        (uid.parse::<u32>(), sec.parse::<u64>(), usec.parse::<u64>())
    else {
        return false;
    };
    sec > 0
        && usec < 1_000_000
        && identity == format!("{PROCESS_IDENTITY_PREFIX}{uid}:start={sec}.{usec}")
}

#[cfg(target_os = "macos")]
impl ProcessInspector for SystemProcesses {
    fn start_identity(&self, pid: u32) -> Result<Option<String>, PowerError> {
        let pid = i32::try_from(pid)
            .map_err(|_| PowerError::InvalidState("process PID exceeds Darwin pid_t".into()))?;
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let expected = std::mem::size_of::<libc::proc_bsdinfo>();
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                i32::try_from(expected).expect("proc_bsdinfo fits c_int"),
            )
        };
        if received == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ESRCH) | Some(libc::ENOENT) => Ok(None),
                _ => Err(PowerError::Io(error)),
            };
        }
        if received < 0 || usize::try_from(received).ok() != Some(expected) {
            return Err(PowerError::InvalidState(format!(
                "proc_pidinfo returned partial BSD info for PID {pid}: {received}/{expected}"
            )));
        }
        let info = unsafe { info.assume_init() };
        if info.pbi_pid != u32::try_from(pid).unwrap_or_default() {
            return Err(PowerError::InvalidState(format!(
                "proc_pidinfo PID mismatch: requested {pid}, got {}",
                info.pbi_pid
            )));
        }
        let snapshot = ProcessSnapshot {
            status: info.pbi_status,
            uid: info.pbi_uid,
            start_sec: info.pbi_start_tvsec,
            start_usec: info.pbi_start_tvusec,
        };
        if snapshot.status == PROCESS_STATUS_ZOMBIE {
            return Ok(None);
        }
        if snapshot.start_sec == 0 || snapshot.start_usec >= 1_000_000 {
            return Err(PowerError::InvalidState(format!(
                "proc_pidinfo returned invalid start identity for PID {pid}"
            )));
        }
        Ok(process_identity_from_snapshot(snapshot))
    }
}

#[cfg(not(target_os = "macos"))]
impl ProcessInspector for SystemProcesses {
    fn start_identity(&self, _pid: u32) -> Result<Option<String>, PowerError> {
        Err(PowerError::InvalidState(
            "Darwin process identity is unavailable on this platform".into(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    NoRegistry,
    KeptForLiveLease,
    Restored(bool),
    BaselineUnchanged(bool),
    BlockedExpiredLiveLease,
    BlockedLegacyRepair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    Mutated,
    Joined,
    BaselineAlreadyOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireObligation {
    None,
    LeaseMayExist,
    MutationMayRemain,
}

#[derive(Debug)]
pub struct AcquireFailure {
    pub error: PowerError,
    pub obligation: AcquireObligation,
}

impl AcquireFailure {
    fn none(error: impl Into<PowerError>) -> Self {
        Self {
            error: error.into(),
            obligation: AcquireObligation::None,
        }
    }

    fn lease_may_exist(error: impl Into<PowerError>) -> Self {
        Self {
            error: error.into(),
            obligation: AcquireObligation::LeaseMayExist,
        }
    }

    fn mutation_may_remain(error: impl Into<PowerError>) -> Self {
        Self {
            error: error.into(),
            obligation: AcquireObligation::MutationMayRemain,
        }
    }
}

impl fmt::Display for AcquireFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} (cleanup obligation: {:?})",
            self.error, self.obligation
        )
    }
}

impl std::error::Error for AcquireFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<PowerError> for AcquireFailure {
    fn from(error: PowerError) -> Self {
        Self::none(error)
    }
}

impl From<StoreError> for AcquireFailure {
    fn from(error: StoreError) -> Self {
        Self::none(error)
    }
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

#[derive(Debug, PartialEq)]
pub enum LegacyMarkerState {
    Missing,
    Present(serde_json::Value),
    Corrupt,
}

#[derive(Debug, Default, Clone, Copy)]
/// Legacy v1 recovery backend.
///
/// Runtime arm/disarm/renew/release is owned exclusively by the attested
/// power-helper. The app may use this backend only while reconciling the v1
/// ownership registry during startup (and in unit fixtures).
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
) -> Result<AcquireOutcome, AcquireFailure> {
    let guard = store.lock()?;
    if let Some(mut state) = guard.read()? {
        validate_ownership_state(&state)?;
        if state.leases.is_empty() {
            return Err(PowerError::InvalidState(
                "a pending restore must complete before another acquire".into(),
            )
            .into());
        }
        let current_boot = backend.boot_id()?;
        if state.boot_id != current_boot {
            return Err(PowerError::InvalidState(format!(
                "registry boot {} does not match current boot {}",
                state.boot_id, current_boot
            ))
            .into());
        }
        let current = backend.read_disabled()?;
        if current != state.applied {
            return Err(PowerError::InvalidState(format!(
                "registry says applied={}, system reports {}",
                state.applied, current
            ))
            .into());
        }
        if state.did_mutate && !backend.can_restore_noninteractive() {
            return Err(PowerError::RollbackUnavailable.into());
        }
        state.acquire(lease);
        guard
            .write(&state)
            .map_err(AcquireFailure::lease_may_exist)?;
        return Ok(AcquireOutcome::Joined);
    }

    let baseline = backend.read_disabled()?;
    if !baseline && !backend.can_restore_noninteractive() {
        return Err(PowerError::RollbackUnavailable.into());
    }
    let mut state = OwnershipState::new(
        baseline,
        backend.boot_id()?,
        u64::try_from(now_ms()).unwrap_or_default(),
    );
    state.acquire(lease);
    guard
        .write(&state)
        .map_err(AcquireFailure::lease_may_exist)?;

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

/// Recover machine-global ownership before any profile-specific startup path.
///
/// The registry lock spans classification, tombstone persistence, restore and
/// read-back. Same-boot leases survive only when the exact versioned
/// PID/start/UID identity still matches. Cross-boot leases are always stale.
pub fn recover_with<B: PmsetBackend, P: ProcessInspector>(
    backend: &B,
    store: &OwnershipStore,
    processes: &P,
    current_time_ms: i64,
) -> Result<RecoveryOutcome, PowerError> {
    let guard = store.lock()?;
    let Some(mut state) = guard.read()? else {
        return Ok(RecoveryOutcome::NoRegistry);
    };
    validate_ownership_state(&state)?;

    let current_boot = backend.boot_id()?;
    if state.boot_id == current_boot {
        let mut retained = Vec::with_capacity(state.leases.len());
        let mut expired_live = false;
        for lease in &state.leases {
            if !valid_process_identity(&lease.process_identity) {
                return Err(PowerError::InvalidState(format!(
                    "unsupported process identity for profile {}",
                    lease.profile
                )));
            }
            match processes.start_identity(lease.pid)? {
                Some(identity) if identity == lease.process_identity => {
                    if current_time_ms >= lease.expires_at_ms {
                        expired_live = true;
                    }
                    retained.push(lease.clone());
                }
                Some(_) | None => {}
            }
        }
        if expired_live {
            return Ok(RecoveryOutcome::BlockedExpiredLiveLease);
        }
        if !retained.is_empty() {
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
            if retained.len() != state.leases.len() {
                state.leases = retained;
                guard.write(&state)?;
            }
            return Ok(RecoveryOutcome::KeptForLiveLease);
        }
    }

    // Persist the restore obligation before any pmset read or mutation.
    if !state.leases.is_empty() {
        state.leases.clear();
        guard.write(&state)?;
    }
    if !state.did_mutate {
        guard.clear()?;
        return Ok(RecoveryOutcome::BaselineUnchanged(state.baseline));
    }

    match restore_and_clear(backend, &guard, state.baseline)? {
        ReleaseOutcome::Restored(value) => Ok(RecoveryOutcome::Restored(value)),
        ReleaseOutcome::BaselineUnchanged(value) => Ok(RecoveryOutcome::BaselineUnchanged(value)),
        ReleaseOutcome::NotOwned | ReleaseOutcome::KeptApplied => Err(PowerError::InvalidState(
            "startup recovery returned an impossible release outcome".into(),
        )),
    }
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
) -> AcquireFailure {
    let rollback = backend
        .set_disabled(baseline)
        .and_then(|()| verify_state(backend, baseline));
    match rollback {
        Ok(()) => match guard.clear() {
            Ok(()) => AcquireFailure::none(primary),
            Err(error) => AcquireFailure::lease_may_exist(PowerError::RollbackFailed(format!(
                "baseline restored but ownership record could not be cleared: {error}"
            ))),
        },
        Err(error) => AcquireFailure::mutation_may_remain(PowerError::RollbackFailed(format!(
            "{primary}; rollback error: {error}"
        ))),
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
    legacy_marker_state_at(&marker_file())
}

fn legacy_marker_state_at(path: &std::path::Path) -> LegacyMarkerState {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return LegacyMarkerState::Missing,
        Err(_) => return LegacyMarkerState::Corrupt,
    };
    parse_legacy_marker(&bytes)
}

/// Legacy markers never recorded a baseline and therefore cannot authorize
/// either a pmset mutation or automatic deletion. The backend is deliberately
/// injectable and deliberately unused so tests can prove observation-only
/// behavior even when every power operation would fail.
pub fn recover_legacy_with<B: PmsetBackend>(
    _backend: &B,
    marker: &std::path::Path,
) -> Result<RecoveryOutcome, PowerError> {
    Ok(match legacy_marker_state_at(marker) {
        LegacyMarkerState::Missing => RecoveryOutcome::NoRegistry,
        LegacyMarkerState::Present(_) | LegacyMarkerState::Corrupt => {
            RecoveryOutcome::BlockedLegacyRepair
        }
    })
}

fn parse_legacy_marker(bytes: &[u8]) -> LegacyMarkerState {
    match serde_json::from_slice(&bytes) {
        Ok(value) => LegacyMarkerState::Present(value),
        Err(_) => LegacyMarkerState::Corrupt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::ownership::Lease;
    use crate::power::ownership_store::OwnershipStore;
    use std::collections::HashMap;
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

    fn process_identity(uid: u32, sec: u64, usec: u64) -> String {
        format!("darwin-v1:uid={uid}:start={sec}.{usec}")
    }

    fn recovery_lease(
        profile: &str,
        pid: u32,
        identity: impl Into<String>,
        expires_at_ms: i64,
    ) -> Lease {
        Lease {
            profile: profile.into(),
            pid,
            process_identity: identity.into(),
            owner_generation: format!("generation-{profile}"),
            acquired_at_ms: 10,
            expires_at_ms,
        }
    }

    fn recovery_state(
        baseline: bool,
        boot_id: &str,
        leases: impl IntoIterator<Item = Lease>,
    ) -> OwnershipState {
        let mut state = OwnershipState::new(baseline, boot_id, 1);
        for lease in leases {
            state.acquire(lease);
        }
        state
    }

    enum ProcessAnswer {
        Identity(String),
        Missing,
        Ambiguous,
    }

    #[derive(Default)]
    struct FakeProcesses {
        answers: HashMap<u32, ProcessAnswer>,
    }

    impl FakeProcesses {
        fn exact(pid: u32, identity: impl Into<String>) -> Self {
            Self {
                answers: HashMap::from([(pid, ProcessAnswer::Identity(identity.into()))]),
            }
        }

        fn missing(pid: u32) -> Self {
            Self {
                answers: HashMap::from([(pid, ProcessAnswer::Missing)]),
            }
        }

        fn ambiguous(pid: u32) -> Self {
            Self {
                answers: HashMap::from([(pid, ProcessAnswer::Ambiguous)]),
            }
        }

        fn with(mut self, pid: u32, answer: ProcessAnswer) -> Self {
            self.answers.insert(pid, answer);
            self
        }
    }

    impl ProcessInspector for FakeProcesses {
        fn start_identity(&self, pid: u32) -> Result<Option<String>, PowerError> {
            match self.answers.get(&pid) {
                Some(ProcessAnswer::Identity(identity)) => Ok(Some(identity.clone())),
                Some(ProcessAnswer::Missing) | None => Ok(None),
                Some(ProcessAnswer::Ambiguous) => Err(PowerError::InvalidState(
                    "injected process inspection ambiguity".into(),
                )),
            }
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

    struct ReadbackFailurePmset {
        reads: Mutex<usize>,
        fail_with_error: bool,
    }

    impl ReadbackFailurePmset {
        fn mismatch() -> Self {
            Self {
                reads: Mutex::new(0),
                fail_with_error: false,
            }
        }

        fn error() -> Self {
            Self {
                reads: Mutex::new(0),
                fail_with_error: true,
            }
        }
    }

    impl PmsetBackend for ReadbackFailurePmset {
        fn read_disabled(&self) -> Result<bool, PowerError> {
            let mut reads = self.reads.lock().unwrap();
            *reads += 1;
            if *reads == 1 {
                Ok(true)
            } else if self.fail_with_error {
                Err(PowerError::Command("injected read-back failure".into()))
            } else {
                Ok(true)
            }
        }

        fn can_restore_noninteractive(&self) -> bool {
            true
        }

        fn set_disabled(&self, _value: bool) -> Result<(), PowerError> {
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
            Err(AcquireFailure {
                error: PowerError::RollbackUnavailable,
                obligation: AcquireObligation::None,
            })
        ));
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn post_rename_write_error_reports_visible_lease_obligation() {
        let (dir, store) = test_store("post-rename-write-error");
        let backend = FakePmset::new(false);
        store.fail_next_parent_sync_after_rename();

        let failure = acquire_with(&backend, &store, test_lease("prod", "generation")).unwrap_err();

        assert_eq!(failure.obligation, AcquireObligation::LeaseMayExist);
        assert!(matches!(failure.error, PowerError::Store(_)));
        let visible = store.read().unwrap().unwrap();
        assert!(visible
            .leases
            .iter()
            .any(|lease| lease.profile == "prod" && lease.owner_generation == "generation"));
        assert!(!backend.current());
        assert!(!backend
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|step| step.starts_with("set:")));
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
            Err(AcquireFailure {
                error: PowerError::Command(_),
                obligation: AcquireObligation::None,
            })
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
    fn startup_keeps_another_profile_only_for_exact_live_process_identity() {
        let (dir, store) = test_store("recover-live");
        let backend = FakePmset::new(true);
        let identity = process_identity(501, 100, 7);
        store
            .write(&recovery_state(
                false,
                "boot-test",
                [recovery_lease("dev", 42, &identity, 1_000)],
            ))
            .unwrap();

        assert_eq!(
            recover_with(&backend, &store, &FakeProcesses::exact(42, identity), 100).unwrap(),
            RecoveryOutcome::KeptForLiveLease
        );
        assert_eq!(store.read().unwrap().unwrap().leases.len(), 1);
        assert!(!backend
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|step| step.starts_with("set:")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_removes_stale_sibling_while_exact_live_lease_stays() {
        let (dir, store) = test_store("recover-mixed");
        let backend = FakePmset::new(true);
        let live_identity = process_identity(501, 100, 7);
        store
            .write(&recovery_state(
                false,
                "boot-test",
                [
                    recovery_lease("prod", 41, process_identity(501, 90, 1), 1_000),
                    recovery_lease("dev", 42, &live_identity, 1_000),
                ],
            ))
            .unwrap();
        let processes = FakeProcesses::missing(41).with(42, ProcessAnswer::Identity(live_identity));

        assert_eq!(
            recover_with(&backend, &store, &processes, 100).unwrap(),
            RecoveryOutcome::KeptForLiveLease
        );
        let state = store.read().unwrap().unwrap();
        assert_eq!(state.leases.len(), 1);
        assert_eq!(state.leases[0].profile, "dev");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_restores_last_stale_mutating_lease_from_tombstone() {
        let (dir, store) = test_store("recover-last-stale");
        let backend = FakePmset::new(true);
        store
            .write(&recovery_state(
                false,
                "boot-test",
                [recovery_lease(
                    "prod",
                    42,
                    process_identity(501, 100, 7),
                    1_000,
                )],
            ))
            .unwrap();

        assert_eq!(
            recover_with(&backend, &store, &FakeProcesses::missing(42), 100).unwrap(),
            RecoveryOutcome::Restored(false)
        );
        assert!(!backend.current());
        assert!(store.read().unwrap().is_none());
        assert!(backend
            .trace
            .lock()
            .unwrap()
            .windows(3)
            .any(|steps| steps == ["preflight:1", "set:0", "read:0"]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reused_pid_with_different_start_identity_is_stale() {
        let (dir, store) = test_store("recover-pid-reuse");
        let backend = FakePmset::new(true);
        store
            .write(&recovery_state(
                false,
                "boot-test",
                [recovery_lease(
                    "prod",
                    42,
                    process_identity(501, 100, 7),
                    1_000,
                )],
            ))
            .unwrap();

        assert_eq!(
            recover_with(
                &backend,
                &store,
                &FakeProcesses::exact(42, process_identity(501, 101, 7)),
                100,
            )
            .unwrap(),
            RecoveryOutcome::Restored(false)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zombie_snapshot_is_proven_stale() {
        assert_eq!(
            process_identity_from_snapshot(ProcessSnapshot {
                status: PROCESS_STATUS_ZOMBIE,
                uid: 501,
                start_sec: 100,
                start_usec: 7,
            }),
            None
        );
    }

    #[test]
    fn cross_boot_registry_cannot_retain_even_matching_process() {
        let (dir, store) = test_store("recover-cross-boot");
        let backend = FakePmset::new(true);
        let identity = process_identity(501, 100, 7);
        store
            .write(&recovery_state(
                false,
                "old-boot",
                [recovery_lease("prod", 42, &identity, 1_000)],
            ))
            .unwrap();

        assert_eq!(
            recover_with(&backend, &store, &FakeProcesses::exact(42, identity), 100).unwrap(),
            RecoveryOutcome::Restored(false)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expired_dead_or_reused_pid_is_stale_but_expired_exact_live_blocks() {
        for (label, processes) in [
            ("dead", FakeProcesses::missing(42)),
            (
                "reused",
                FakeProcesses::exact(42, process_identity(501, 101, 7)),
            ),
        ] {
            let (dir, store) = test_store(label);
            let backend = FakePmset::new(true);
            store
                .write(&recovery_state(
                    false,
                    "boot-test",
                    [recovery_lease(
                        "prod",
                        42,
                        process_identity(501, 100, 7),
                        99,
                    )],
                ))
                .unwrap();
            assert_eq!(
                recover_with(&backend, &store, &processes, 100).unwrap(),
                RecoveryOutcome::Restored(false)
            );
            std::fs::remove_dir_all(dir).unwrap();
        }

        let (dir, store) = test_store("expired-live");
        let backend = FakePmset::new(true);
        let identity = process_identity(501, 100, 7);
        let state = recovery_state(
            false,
            "boot-test",
            [recovery_lease("prod", 42, &identity, 99)],
        );
        store.write(&state).unwrap();
        let before = std::fs::read(dir.join("ownership.json")).unwrap();

        assert_eq!(
            recover_with(&backend, &store, &FakeProcesses::exact(42, identity), 100).unwrap(),
            RecoveryOutcome::BlockedExpiredLiveLease
        );
        assert_eq!(std::fs::read(dir.join("ownership.json")).unwrap(), before);
        assert!(backend.trace.lock().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_provisional_identity_and_inspector_ambiguity_block_unchanged() {
        for (label, identity, processes) in [
            (
                "unknown-identity",
                "42:100".to_string(),
                FakeProcesses::exact(42, "42:100"),
            ),
            (
                "ambiguous-inspector",
                process_identity(501, 100, 7),
                FakeProcesses::ambiguous(42),
            ),
        ] {
            let (dir, store) = test_store(label);
            let backend = FakePmset::new(true);
            store
                .write(&recovery_state(
                    false,
                    "boot-test",
                    [recovery_lease("prod", 42, identity, 1_000)],
                ))
                .unwrap();
            let before = std::fs::read(dir.join("ownership.json")).unwrap();

            assert!(matches!(
                recover_with(&backend, &store, &processes, 100),
                Err(PowerError::InvalidState(_))
            ));
            assert_eq!(std::fs::read(dir.join("ownership.json")).unwrap(), before);
            assert!(backend.trace.lock().unwrap().is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn corrupt_schema_and_semantic_registry_remain_byte_for_byte() {
        let cases = [
            ("json", b"{".as_slice()),
            (
                "schema",
                br#"{"schemaVersion":99,"bootId":"boot-test","baseline":false,"applied":true,"didMutate":true,"generation":1,"leases":[]}"#
                    .as_slice(),
            ),
            (
                "semantic",
                br#"{"schemaVersion":1,"bootId":"","baseline":false,"applied":true,"didMutate":true,"generation":1,"leases":[]}"#
                    .as_slice(),
            ),
        ];
        for (label, bytes) in cases {
            let (dir, store) = test_store(label);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("ownership.json"), bytes).unwrap();
            let backend = FakePmset::new(false);

            assert!(recover_with(&backend, &store, &FakeProcesses::default(), 100).is_err());
            assert_eq!(std::fs::read(dir.join("ownership.json")).unwrap(), bytes);
            assert!(backend.trace.lock().unwrap().is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn recovery_failure_leaves_zero_lease_tombstone_for_retry() {
        for (label, backend) in [
            ("preflight", FakePmset::without_rollback(true)),
            ("set", FakePmset::failing_first_set(true)),
        ] {
            let (dir, store) = test_store(label);
            store
                .write(&recovery_state(
                    false,
                    "boot-test",
                    [recovery_lease(
                        "prod",
                        42,
                        process_identity(501, 100, 7),
                        1_000,
                    )],
                ))
                .unwrap();

            assert!(recover_with(&backend, &store, &FakeProcesses::missing(42), 100).is_err());
            let pending = store.read().unwrap().unwrap();
            assert!(pending.leases.is_empty());
            assert!(pending.did_mutate);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn recovery_readback_error_or_mismatch_keeps_tombstone_evidence() {
        for (label, backend) in [
            ("readback-error", ReadbackFailurePmset::error()),
            ("readback-mismatch", ReadbackFailurePmset::mismatch()),
        ] {
            let (dir, store) = test_store(label);
            store
                .write(&recovery_state(
                    false,
                    "boot-test",
                    [recovery_lease(
                        "prod",
                        42,
                        process_identity(501, 100, 7),
                        1_000,
                    )],
                ))
                .unwrap();

            assert!(recover_with(&backend, &store, &FakeProcesses::missing(42), 100).is_err());
            let pending = store.read().unwrap().unwrap();
            assert!(pending.leases.is_empty());
            assert!(pending.did_mutate);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn recovery_of_baseline_true_never_writes_disablesleep_zero() {
        let (dir, store) = test_store("recover-baseline-on");
        let backend = FakePmset::new(true);
        store
            .write(&recovery_state(
                true,
                "boot-test",
                [recovery_lease(
                    "prod",
                    42,
                    process_identity(501, 100, 7),
                    1_000,
                )],
            ))
            .unwrap();

        assert_eq!(
            recover_with(&backend, &store, &FakeProcesses::missing(42), 100).unwrap(),
            RecoveryOutcome::BaselineUnchanged(true)
        );
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
    fn legacy_marker_is_observation_only_even_when_backend_would_error() {
        for (label, bytes) in [
            ("present", br#"{"armed":true}"#.as_slice()),
            ("corrupt", b"{".as_slice()),
        ] {
            let (dir, _) = test_store(label);
            std::fs::create_dir_all(&dir).unwrap();
            let marker = dir.join("clamshell.json");
            std::fs::write(&marker, bytes).unwrap();
            let backend = FakePmset::failing_first_set(false);

            assert_eq!(
                recover_legacy_with(&backend, &marker).unwrap(),
                RecoveryOutcome::BlockedLegacyRepair
            );
            assert_eq!(std::fs::read(&marker).unwrap(), bytes);
            assert!(backend.trace.lock().unwrap().is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn corrupt_registry_blocks_mutation() {
        let (dir, store) = test_store("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ownership.json"), b"{").unwrap();
        let backend = FakePmset::new(false);

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("prod", "generation")),
            Err(AcquireFailure {
                error: PowerError::Store(_),
                obligation: AcquireObligation::None,
            })
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
        let mut invalid = crate::power::ownership::OwnershipState::new(false, "boot-test", 1);
        invalid.did_mutate = false;
        invalid.acquire(test_lease("prod", "generation"));
        store.write(&invalid).unwrap();

        assert!(matches!(
            acquire_with(&backend, &store, test_lease("dev", "other")),
            Err(AcquireFailure {
                error: PowerError::InvalidState(_),
                obligation: AcquireObligation::None,
            })
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
