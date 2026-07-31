use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jarvis_power_core::engine::{ProcessState, RuntimeGuardError, RuntimeGuardFailureOutcome};
use jarvis_power_core::protocol::{DEFAULT_TTL_MS, MIN_TTL_MS};
use jarvis_power_core::state::{
    DarwinProcessIdentity, LeaseId, MonotonicTime, MutationPhase, Principal,
};
use jarvis_power_helper::coordinator::{
    CoordinatorError, MonotonicClock, ProcessInspectionError, ProcessInspector, RandomError,
    RandomSource,
};
use jarvis_power_helper::pmset::{PmsetBackend, PmsetError, SystemPmset};
use jarvis_power_helper::root_store::{RootStore, StoreError};
use jarvis_power_helper::watchdog::{StartupRuntime, Watchdog};
use jarvis_power_helper::{HelperEvent, HelperEventSink};
use tempfile::TempDir;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<HelperEvent>>,
}

impl RecordingSink {
    fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    fn events(&self) -> Vec<HelperEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl HelperEventSink for RecordingSink {
    fn record(&self, event: HelperEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Clone)]
struct FakeBackend {
    state: Arc<Mutex<BackendState>>,
}

struct BackendState {
    disabled: bool,
    boot_id: String,
    fail_set: Option<bool>,
    install_symlink_on_read: Option<(PathBuf, PathBuf)>,
}

impl PmsetBackend for FakeBackend {
    fn read_disabled(&mut self) -> Result<bool, PmsetError> {
        let mut state = self.state.lock().unwrap();
        if let Some((state_path, outside)) = state.install_symlink_on_read.take() {
            symlink(outside, state_path).map_err(|_| PmsetError::Io)?;
        }
        Ok(state.disabled)
    }

    fn set_disabled(&mut self, value: bool) -> Result<(), PmsetError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_set == Some(value) {
            return Err(PmsetError::CommandFailed);
        }
        state.disabled = value;
        Ok(())
    }

    fn boot_id(&mut self) -> Result<String, PmsetError> {
        Ok(self.state.lock().unwrap().boot_id.clone())
    }
}

#[derive(Clone)]
struct FakeClock {
    samples: Arc<Mutex<VecDeque<u64>>>,
    fallback: Arc<Mutex<u64>>,
}

impl FakeClock {
    fn fixed(value: u64) -> Self {
        Self {
            samples: Arc::new(Mutex::new(VecDeque::new())),
            fallback: Arc::new(Mutex::new(value)),
        }
    }

    fn set(&self, value: u64) {
        *self.fallback.lock().unwrap() = value;
        self.samples.lock().unwrap().clear();
    }

    fn script(&self, values: impl IntoIterator<Item = u64>) {
        *self.samples.lock().unwrap() = values.into_iter().collect();
    }
}

impl MonotonicClock for FakeClock {
    fn now(&mut self) -> Result<MonotonicTime, CoordinatorError> {
        let next = self.samples.lock().unwrap().pop_front();
        Ok(MonotonicTime::from_millis(
            next.unwrap_or_else(|| *self.fallback.lock().unwrap()),
        ))
    }
}

#[derive(Clone)]
struct FakeProcesses {
    state: Arc<Mutex<ProcessState>>,
}

impl ProcessInspector for FakeProcesses {
    fn inspect(&mut self, _principal: &Principal) -> Result<ProcessState, ProcessInspectionError> {
        Ok(*self.state.lock().unwrap())
    }
}

struct FixedRandom {
    next_lease: u128,
    next_generation: u64,
}

impl RandomSource for FixedRandom {
    fn next_lease_id(&mut self) -> Result<LeaseId, RandomError> {
        let text = format!("{:032x}", self.next_lease);
        self.next_lease += 1;
        LeaseId::parse(text).map_err(|_| RandomError::Unavailable)
    }

    fn next_mutation_generation(&mut self) -> Result<u64, RandomError> {
        let generation = self.next_generation;
        self.next_generation += 1;
        Ok(generation)
    }
}

type Ready = jarvis_power_helper::watchdog::ReadyRuntime<
    FakeBackend,
    FakeClock,
    FakeProcesses,
    FixedRandom,
>;

struct Harness {
    _temp: TempDir,
    state_path: PathBuf,
    outside: PathBuf,
    store: RootStore,
    sink: Arc<RecordingSink>,
    backend_state: Arc<Mutex<BackendState>>,
    clock: FakeClock,
    process_state: Arc<Mutex<ProcessState>>,
    ready: Ready,
}

impl Harness {
    fn baseline(disabled: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("v2");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let state_path = directory.join("state.json");
        let outside = temp.path().join("outside");
        fs::write(&outside, b"sentinel").unwrap();
        let sink = Arc::new(RecordingSink::default());
        // SAFETY: querying the current test process does not mutate external state.
        let uid = unsafe { libc::geteuid() };
        // SAFETY: querying the current test process does not mutate external state.
        let gid = unsafe { libc::getegid() };
        let store =
            RootStore::open_for_testing_with_sink(&directory, uid, gid, uid, gid, sink.clone())
                .unwrap();
        let backend_state = Arc::new(Mutex::new(BackendState {
            disabled,
            boot_id: "boot-a".to_owned(),
            fail_set: None,
            install_symlink_on_read: None,
        }));
        let backend = FakeBackend {
            state: backend_state.clone(),
        };
        let clock = FakeClock::fixed(1_000);
        let process_state = Arc::new(Mutex::new(ProcessState::AliveExact));
        let processes = FakeProcesses {
            state: process_state.clone(),
        };
        let random = FixedRandom {
            next_lease: 1,
            next_generation: 7,
        };
        let startup = StartupRuntime::new(
            store.clone(),
            backend,
            clock.clone(),
            processes,
            random,
            1,
            1,
        );
        let ready = startup.reconcile_before_listener().unwrap();
        sink.clear();
        Self {
            _temp: temp,
            state_path,
            outside,
            store,
            sink,
            backend_state,
            clock,
            process_state,
            ready,
        }
    }

    fn acquire(&mut self, ttl_ms: u64) -> Result<(), CoordinatorError> {
        self.ready
            .acquire(&owner(), "prod", "generation-a", ttl_ms)
            .map(|_| ())
    }

    fn disabled(&self) -> bool {
        self.backend_state.lock().unwrap().disabled
    }

    fn fail_restore(&self) {
        self.backend_state.lock().unwrap().fail_set = Some(false);
    }

    fn set_process_state(&self, state: ProcessState) {
        *self.process_state.lock().unwrap() = state;
    }
}

fn owner() -> Principal {
    Principal::from_helper_attestation(
        503,
        42,
        DarwinProcessIdentity::new(1, 100, 20).unwrap(),
        "app.jarvis.monitor",
        "ABCDE12345",
        [9; 32],
        1,
    )
    .unwrap()
}

#[test]
fn acquire_holds_one_lock_through_durable_write_mutation_readback_and_reply() {
    let mut harness = Harness::baseline(false);
    harness.acquire(DEFAULT_TTL_MS).unwrap();

    assert_eq!(
        harness.sink.events(),
        vec![
            HelperEvent::LockAcquired,
            HelperEvent::PowerRead(false),
            HelperEvent::StateWriteStarted(MutationPhase::Prepared),
            HelperEvent::TempFileSynced,
            HelperEvent::StateRenamed,
            HelperEvent::ParentDirectorySynced,
            HelperEvent::PowerWrite(true),
            HelperEvent::PowerRead(true),
            HelperEvent::StateWriteStarted(MutationPhase::Applied),
            HelperEvent::TempFileSynced,
            HelperEvent::StateRenamed,
            HelperEvent::ParentDirectorySynced,
            HelperEvent::ReplyReady,
            HelperEvent::LockReleased,
        ]
    );
}

#[test]
fn dead_or_mismatched_process_and_deadline_expiry_restore_autonomously() {
    for process_state in [ProcessState::Dead, ProcessState::Mismatch] {
        let mut harness = Harness::baseline(false);
        harness.acquire(DEFAULT_TTL_MS).unwrap();
        harness.set_process_state(process_state);
        harness.ready.watchdog().tick().unwrap();
        assert!(!harness.disabled());
        assert_eq!(harness.store.load().unwrap(), None);
    }

    let mut expired = Harness::baseline(false);
    expired.acquire(MIN_TTL_MS).unwrap();
    expired.clock.set(6_000);
    expired.ready.watchdog().tick().unwrap();
    assert!(!expired.disabled());
    assert_eq!(expired.store.load().unwrap(), None);
}

#[test]
fn failed_restore_retains_a_restore_pending_tombstone() {
    let mut harness = Harness::baseline(false);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.fail_restore();

    assert!(harness.ready.watchdog().tick().is_err());
    let state = harness.store.load().unwrap().unwrap();
    assert_eq!(state.phase, MutationPhase::RestorePending);
    assert!(state.leases.is_empty());
    assert!(harness.disabled());
}

#[test]
fn startup_reconciles_before_a_listener_permit_can_exist() {
    let mut harness = Harness::baseline(false);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.sink.clear();

    let backend = FakeBackend {
        state: harness.backend_state.clone(),
    };
    let processes = FakeProcesses {
        state: harness.process_state.clone(),
    };
    let startup = StartupRuntime::new(
        harness.store.clone(),
        backend,
        harness.clock.clone(),
        processes,
        FixedRandom {
            next_lease: 50,
            next_generation: 50,
        },
        1,
        1,
    );
    let ready = startup.reconcile_before_listener().unwrap();
    let _permit = ready.listener_permit();

    let events = harness.sink.events();
    assert_eq!(events.first(), Some(&HelperEvent::StartupRecovery));
    assert_eq!(events.last(), Some(&HelperEvent::StartupReady));
    assert_eq!(harness.store.load().unwrap(), None);
    assert!(!harness.disabled());
}

#[test]
fn watchdog_is_the_only_post_startup_recovery_trigger_and_ticks_each_second() {
    assert_eq!(Watchdog::<FakeBackend, FakeClock, FakeProcesses, FixedRandom>::INTERVAL, Duration::from_secs(1));
    let source = include_str!("../src/coordinator.rs");
    assert!(!source.contains("pub fn recover"));
    assert!(!source.contains("pub fn reconcile"));
}

#[test]
fn baseline_true_is_cleared_without_ever_writing_false() {
    let mut harness = Harness::baseline(true);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.sink.clear();

    harness.ready.watchdog().tick().unwrap();

    assert!(harness.disabled());
    assert!(!harness
        .sink
        .events()
        .iter()
        .any(|event| *event == HelperEvent::PowerWrite(false)));
    assert_eq!(harness.store.load().unwrap(), None);
}

#[test]
fn runtime_guard_failure_reconciles_under_the_same_lock_and_never_replies_success() {
    let mut harness = Harness::baseline(false);
    harness.clock.script([1_000, 6_000, 6_000, 6_000]);

    let error = harness.acquire(MIN_TTL_MS).unwrap_err();

    assert_eq!(
        error,
        CoordinatorError::RuntimeGuard(RuntimeGuardFailureOutcome::Recovered(
            RuntimeGuardError::DeadlineExpired
        ))
    );
    let events = harness.sink.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == HelperEvent::LockAcquired)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == HelperEvent::LockReleased)
            .count(),
        1
    );
    assert!(!events.contains(&HelperEvent::ReplyReady));
    assert_eq!(harness.store.load().unwrap(), None);
}

#[test]
fn unverifiable_process_blocks_recovery_and_keeps_applied_evidence() {
    let mut harness = Harness::baseline(false);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Unverifiable);

    assert_eq!(
        harness.ready.watchdog().tick(),
        Err(CoordinatorError::Process(
            ProcessInspectionError::Unverifiable
        ))
    );
    assert_eq!(
        harness.store.load().unwrap().unwrap().phase,
        MutationPhase::Applied
    );
    assert!(harness.disabled());
}

#[test]
fn symlink_inserted_between_read_and_persist_is_rejected_not_overwritten() {
    let mut harness = Harness::baseline(false);
    harness
        .backend_state
        .lock()
        .unwrap()
        .install_symlink_on_read = Some((harness.state_path.clone(), harness.outside.clone()));

    assert_eq!(
        harness.acquire(DEFAULT_TTL_MS),
        Err(CoordinatorError::Store(StoreError::UnsafeMetadata))
    );
    assert_eq!(fs::read(&harness.outside).unwrap(), b"sentinel");
    assert!(fs::symlink_metadata(&harness.state_path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn system_pmset_policy_is_fixed_and_has_no_caller_arguments() {
    let policy = SystemPmset::policy();
    assert_eq!(policy.program(), "/usr/bin/pmset");
    assert_eq!(policy.timeout(), Duration::from_secs(8));
    assert_eq!(policy.read_args(), ["-g"]);
    assert_eq!(policy.write_args(false), ["-a", "disablesleep", "0"]);
    assert_eq!(policy.write_args(true), ["-a", "disablesleep", "1"]);
    assert!(policy.stdin_is_null());
    assert!(policy.environment_is_cleared());
    assert!(policy.output_is_bounded());
}
