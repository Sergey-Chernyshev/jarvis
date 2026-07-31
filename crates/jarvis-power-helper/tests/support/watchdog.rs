use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use jarvis_power_core::engine::{ProcessState, RuntimeGuardError, RuntimeGuardFailureOutcome};
use jarvis_power_core::protocol::{DEFAULT_TTL_MS, MIN_TTL_MS};
use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    STATE_SCHEMA_VERSION,
};
use jarvis_power_helper::coordinator::{
    CoordinatorError, MonotonicClock, ProcessInspectionError, ProcessInspector, RandomError,
    RandomSource,
};
use jarvis_power_helper::pmset::{PmsetBackend, PmsetError, SystemPmset};
use jarvis_power_helper::root_store::{RootStore, StoreError, StoreFault};
use jarvis_power_helper::watchdog::{
    ReadyRuntime, RuntimeHealth, SchedulerArmError, SchedulerFactory, ServingRuntime,
    StartupRuntime, SystemSchedulerTestMode, WatchdogGuard, WatchdogTask, WatchdogTermination,
    WATCHDOG_INTERVAL, WATCHDOG_READY_TIMEOUT,
};
use jarvis_power_helper::{HelperEvent, HelperEventSink};
use tempfile::TempDir;

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<HelperEvent>>,
    changed: Condvar,
}

impl RecordingSink {
    fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    fn events(&self) -> Vec<HelperEvent> {
        self.events.lock().unwrap().clone()
    }

    fn wait_for(&self, expected: HelperEvent, timeout: Duration) -> bool {
        let events = self.events.lock().unwrap();
        if events.contains(&expected) {
            return true;
        }
        self.changed
            .wait_timeout_while(events, timeout, |events| !events.contains(&expected))
            .map(|(events, _)| events.contains(&expected))
            .unwrap_or(false)
    }
}

impl HelperEventSink for RecordingSink {
    fn record(&self, event: HelperEvent) {
        self.events.lock().unwrap().push(event);
        self.changed.notify_all();
    }
}

#[derive(Clone)]
struct FakeBackend {
    state: Arc<Mutex<BackendState>>,
    concurrency: Option<BackendConcurrencyProbe>,
}

struct BackendState {
    disabled: bool,
    boot_id: String,
    fail_set: Option<bool>,
    fail_after_set: Option<bool>,
    arm_fault_after_set: Option<(RootStore, StoreFault)>,
    install_symlink_on_read: Option<(PathBuf, PathBuf)>,
}

#[derive(Clone, Default)]
struct BackendConcurrencyProbe {
    state: Arc<(Mutex<BackendConcurrencyState>, Condvar)>,
}

#[derive(Default)]
struct BackendConcurrencyState {
    active: usize,
    maximum_active: usize,
    calls: usize,
    block_next: bool,
    released: bool,
}

impl BackendConcurrencyProbe {
    fn block_next_call(&self) {
        let mut state = self.state.0.lock().unwrap();
        state.block_next = true;
        state.released = false;
    }

    fn enter(&self) -> BackendCallGuard {
        let mut state = self.state.0.lock().unwrap();
        state.active += 1;
        state.calls += 1;
        state.maximum_active = state.maximum_active.max(state.active);
        self.state.1.notify_all();
        if state.block_next {
            state.block_next = false;
            state = self
                .state
                .1
                .wait_while(state, |state| !state.released)
                .unwrap();
        }
        drop(state);
        BackendCallGuard {
            probe: self.clone(),
        }
    }

    fn wait_for_calls(&self, expected: usize, timeout: Duration) -> bool {
        let state = self.state.0.lock().unwrap();
        if state.calls >= expected {
            return true;
        }
        self.state
            .1
            .wait_timeout_while(state, timeout, |state| state.calls < expected)
            .map(|(state, _)| state.calls >= expected)
            .unwrap_or(false)
    }

    fn release(&self) {
        let mut state = self.state.0.lock().unwrap();
        state.released = true;
        self.state.1.notify_all();
    }

    fn maximum_active(&self) -> usize {
        self.state.0.lock().unwrap().maximum_active
    }

    fn calls(&self) -> usize {
        self.state.0.lock().unwrap().calls
    }
}

struct BackendCallGuard {
    probe: BackendConcurrencyProbe,
}

impl Drop for BackendCallGuard {
    fn drop(&mut self) {
        let mut state = self.probe.state.0.lock().unwrap();
        state.active -= 1;
        self.probe.state.1.notify_all();
    }
}

impl PmsetBackend for FakeBackend {
    fn read_disabled(&mut self) -> Result<bool, PmsetError> {
        let _call = self
            .concurrency
            .as_ref()
            .map(BackendConcurrencyProbe::enter);
        let mut state = self.state.lock().unwrap();
        if let Some((state_path, outside)) = state.install_symlink_on_read.take() {
            symlink(outside, state_path).map_err(|_| PmsetError::Io)?;
        }
        Ok(state.disabled)
    }

    fn set_disabled(&mut self, value: bool) -> Result<(), PmsetError> {
        let _call = self
            .concurrency
            .as_ref()
            .map(BackendConcurrencyProbe::enter);
        let mut state = self.state.lock().unwrap();
        if state.fail_set == Some(value) {
            return Err(PmsetError::CommandFailed);
        }
        state.disabled = value;
        if state.fail_after_set.take() == Some(value) {
            return Err(PmsetError::CommandFailed);
        }
        if let Some((store, fault)) = state.arm_fault_after_set.take() {
            store.arm_fault(fault);
        }
        Ok(())
    }

    fn boot_id(&mut self) -> Result<String, PmsetError> {
        let _call = self
            .concurrency
            .as_ref()
            .map(BackendConcurrencyProbe::enter);
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

#[derive(Clone)]
struct FakeScheduler {
    state: Arc<Mutex<FakeSchedulerState>>,
}

struct FakeSchedulerState {
    failure: Option<SchedulerArmError>,
    task: Option<WatchdogTask>,
    termination: Option<WatchdogTermination>,
    interval: Option<Duration>,
    ready_timeout: Option<Duration>,
    events: Vec<&'static str>,
    stopped: bool,
    joined: bool,
}

impl FakeScheduler {
    fn ready() -> Self {
        Self::with_failure(None)
    }

    fn failing(error: SchedulerArmError) -> Self {
        Self::with_failure(Some(error))
    }

    fn with_failure(failure: Option<SchedulerArmError>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeSchedulerState {
                failure,
                task: None,
                termination: None,
                interval: None,
                ready_timeout: None,
                events: Vec::new(),
                stopped: false,
                joined: false,
            })),
        }
    }

    fn trigger_interval(&self) {
        let mut task = self
            .state
            .lock()
            .unwrap()
            .task
            .take()
            .expect("scheduler is armed");
        task();
        self.state.lock().unwrap().task = Some(task);
    }

    fn record_bind(&self) {
        self.state.lock().unwrap().events.push("bind");
    }

    fn events(&self) -> Vec<&'static str> {
        self.state.lock().unwrap().events.clone()
    }

    fn interval(&self) -> Option<Duration> {
        self.state.lock().unwrap().interval
    }

    fn ready_timeout(&self) -> Option<Duration> {
        self.state.lock().unwrap().ready_timeout
    }

    fn joined(&self) -> bool {
        self.state.lock().unwrap().joined
    }
}

struct FakeWatchdogGuard {
    state: Arc<Mutex<FakeSchedulerState>>,
}

impl WatchdogGuard for FakeWatchdogGuard {}

impl Drop for FakeWatchdogGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.events.push("stop");
        state.stopped = true;
        state.task = None;
        state.termination = None;
        state.events.push("join");
        state.joined = true;
    }
}

impl SchedulerFactory for FakeScheduler {
    fn start(
        self,
        interval: Duration,
        ready_timeout: Duration,
        task: WatchdogTask,
        termination: WatchdogTermination,
    ) -> Result<Box<dyn WatchdogGuard>, SchedulerArmError> {
        let mut state = self.state.lock().unwrap();
        state.events.push("start");
        state.interval = Some(interval);
        state.ready_timeout = Some(ready_timeout);
        if let Some(error) = state.failure {
            if error == SchedulerArmError::ReadyTimeout {
                state.events.extend(["stop", "join"]);
                state.stopped = true;
                state.joined = true;
            }
            return Err(error);
        }
        state.task = Some(task);
        state.termination = Some(termination);
        state.events.push("ready");
        drop(state);
        Ok(Box::new(FakeWatchdogGuard {
            state: self.state.clone(),
        }))
    }
}

type Serving = ServingRuntime<FakeBackend, FakeClock, FakeProcesses, FixedRandom>;
type Ready = ReadyRuntime<FakeBackend, FakeClock, FakeProcesses, FixedRandom>;

struct Harness {
    _temp: TempDir,
    state_path: PathBuf,
    outside: PathBuf,
    store: RootStore,
    sink: Arc<RecordingSink>,
    backend_state: Arc<Mutex<BackendState>>,
    clock: FakeClock,
    process_state: Arc<Mutex<ProcessState>>,
    scheduler: FakeScheduler,
    runtime: Serving,
}

impl Harness {
    fn baseline(disabled: bool) -> Self {
        Self::with_scheduler(disabled, FakeScheduler::ready()).unwrap()
    }

    fn with_scheduler(disabled: bool, scheduler: FakeScheduler) -> Result<Self, SchedulerArmError> {
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
            fail_after_set: None,
            arm_fault_after_set: None,
            install_symlink_on_read: None,
        }));
        let backend = FakeBackend {
            state: backend_state.clone(),
            concurrency: None,
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
        let runtime = ready.arm_with_scheduler(scheduler.clone())?;
        sink.clear();
        Ok(Self {
            _temp: temp,
            state_path,
            outside,
            store,
            sink,
            backend_state,
            clock,
            process_state,
            scheduler,
            runtime,
        })
    }

    fn acquire(&self, ttl_ms: u64) -> Result<(), CoordinatorError> {
        self.runtime
            .acquire(&owner(), "prod", "generation-a", ttl_ms)
            .map(|_| ())
    }

    fn disabled(&self) -> bool {
        self.backend_state.lock().unwrap().disabled
    }

    fn fail_restore(&self) {
        self.backend_state.lock().unwrap().fail_set = Some(false);
    }

    fn allow_restore(&self) {
        self.backend_state.lock().unwrap().fail_set = None;
    }

    fn set_process_state(&self, state: ProcessState) {
        *self.process_state.lock().unwrap() = state;
    }
}

struct SeededReady {
    _temp: TempDir,
    store: RootStore,
    sink: Arc<RecordingSink>,
    backend_state: Arc<Mutex<BackendState>>,
    process_state: Arc<Mutex<ProcessState>>,
    ready: Option<Ready>,
}

impl SeededReady {
    fn new(concurrency: Option<BackendConcurrencyProbe>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("v2");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let sink = Arc::new(RecordingSink::default());
        // SAFETY: querying the current test process does not mutate external state.
        let uid = unsafe { libc::geteuid() };
        // SAFETY: querying the current test process does not mutate external state.
        let gid = unsafe { libc::getegid() };
        let store =
            RootStore::open_for_testing_with_sink(&directory, uid, gid, uid, gid, sink.clone())
                .unwrap();
        let state = HelperState {
            schema_version: STATE_SCHEMA_VERSION,
            service_version: 1,
            minimum_client_build: 1,
            boot_id: "boot-a".to_owned(),
            baseline: false,
            applied: true,
            did_mutate: true,
            mutation_generation: 7,
            phase: MutationPhase::Applied,
            leases: vec![Lease {
                lease_id: LeaseId::parse("00000000000000000000000000000001").unwrap(),
                profile: "prod".to_owned(),
                owner_generation: "generation-a".to_owned(),
                principal: owner(),
                deadline: MonotonicTime::from_millis(20_000),
            }],
        };
        store.lock().unwrap().persist(&state).unwrap();

        let backend_state = Arc::new(Mutex::new(BackendState {
            disabled: true,
            boot_id: "boot-a".to_owned(),
            fail_set: None,
            fail_after_set: None,
            arm_fault_after_set: None,
            install_symlink_on_read: None,
        }));
        let backend = FakeBackend {
            state: backend_state.clone(),
            concurrency,
        };
        let clock = FakeClock::fixed(1_000);
        let process_state = Arc::new(Mutex::new(ProcessState::AliveExact));
        let processes = FakeProcesses {
            state: process_state.clone(),
        };
        let startup = StartupRuntime::new(
            store.clone(),
            backend,
            clock,
            processes,
            FixedRandom {
                next_lease: 100,
                next_generation: 100,
            },
            1,
            1,
        );
        let ready = startup.reconcile_before_listener().unwrap();
        sink.clear();
        Self {
            _temp: temp,
            store,
            sink,
            backend_state,
            process_state,
            ready: Some(ready),
        }
    }

    fn arm(&mut self, mode: SystemSchedulerTestMode) -> Serving {
        self.ready
            .take()
            .unwrap()
            .arm_system_for_testing(Duration::from_millis(5), Duration::from_millis(250), mode)
            .unwrap()
    }

    fn set_process_state(&self, state: ProcessState) {
        *self.process_state.lock().unwrap() = state;
    }

    fn disabled(&self) -> bool {
        self.backend_state.lock().unwrap().disabled
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
fn idempotent_acquire_renew_and_exact_release_complete_under_the_coordinator() {
    let mut harness = Harness::baseline(false);
    let first = harness
        .runtime
        .acquire(&owner(), "prod", "generation-a", DEFAULT_TTL_MS)
        .unwrap();
    let retry = harness
        .runtime
        .acquire(&owner(), "prod", "generation-a", DEFAULT_TTL_MS)
        .unwrap();
    assert_eq!(retry.lease_id, first.lease_id);
    assert_eq!(retry.granted_ttl_ms, first.granted_ttl_ms);

    harness.clock.set(20_000);
    let renewed = harness
        .runtime
        .renew(&owner(), &first.lease_id, "generation-a", DEFAULT_TTL_MS)
        .unwrap();
    assert_eq!(renewed.lease_id, first.lease_id);
    assert_eq!(renewed.granted_ttl_ms, DEFAULT_TTL_MS);

    harness
        .runtime
        .release(&owner(), &first.lease_id, "generation-a")
        .unwrap();
    assert!(!harness.disabled());
    assert_eq!(harness.store.load().unwrap(), None);
}

#[test]
fn dead_or_mismatched_process_and_deadline_expiry_restore_autonomously() {
    for process_state in [ProcessState::Dead, ProcessState::Mismatch] {
        let mut harness = Harness::baseline(false);
        harness.acquire(DEFAULT_TTL_MS).unwrap();
        harness.set_process_state(process_state);
        harness.scheduler.trigger_interval();
        assert!(!harness.disabled());
        assert_eq!(harness.store.load().unwrap(), None);
    }

    let mut expired = Harness::baseline(false);
    expired.acquire(MIN_TTL_MS).unwrap();
    expired.clock.set(6_000);
    expired.scheduler.trigger_interval();
    assert!(!expired.disabled());
    assert_eq!(expired.store.load().unwrap(), None);
}

#[test]
fn failed_restore_retains_a_restore_pending_tombstone() {
    let mut harness = Harness::baseline(false);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.fail_restore();

    harness.scheduler.trigger_interval();
    assert!(matches!(
        harness.runtime.health(),
        RuntimeHealth::Unhealthy { .. }
    ));
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
        concurrency: None,
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
    let events = harness.sink.events();
    assert_eq!(events.first(), Some(&HelperEvent::StartupRecovery));
    assert_eq!(events.last(), Some(&HelperEvent::StartupReady));
    assert_eq!(harness.store.load().unwrap(), None);
    assert!(!harness.disabled());

    let scheduler = FakeScheduler::ready();
    let runtime = ready.arm_with_scheduler(scheduler.clone()).unwrap();
    let _permit = runtime.listener_permit();
    scheduler.record_bind();
    assert_eq!(scheduler.events(), ["start", "ready", "bind"]);
}

#[test]
fn failed_scheduler_arm_never_yields_serving_or_a_listener_permit() {
    let spawn_failed = FakeScheduler::failing(SchedulerArmError::SpawnFailed);
    assert!(matches!(
        Harness::with_scheduler(false, spawn_failed.clone()),
        Err(SchedulerArmError::SpawnFailed)
    ));
    assert_eq!(spawn_failed.events(), ["start"]);
    assert!(!spawn_failed.events().contains(&"bind"));
    assert!(!spawn_failed.joined());

    let ready_timeout = FakeScheduler::failing(SchedulerArmError::ReadyTimeout);
    assert!(matches!(
        Harness::with_scheduler(false, ready_timeout.clone()),
        Err(SchedulerArmError::ReadyTimeout)
    ));
    assert_eq!(ready_timeout.events(), ["start", "stop", "join"]);
    assert!(!ready_timeout.events().contains(&"bind"));
    assert!(ready_timeout.joined());
}

#[test]
fn dropping_serving_runtime_stops_and_joins_the_scheduler_guard() {
    let scheduler = FakeScheduler::ready();
    {
        let harness = Harness::with_scheduler(false, scheduler.clone()).unwrap();
        assert!(!scheduler.joined());
        assert_eq!(scheduler.events(), ["start", "ready"]);
        drop(harness);
    }
    assert!(scheduler.joined());
    assert_eq!(scheduler.events(), ["start", "ready", "stop", "join"]);
}

#[test]
fn real_scheduler_acks_then_recovers_autonomously_and_drop_stops_and_joins() {
    let mut fixture = SeededReady::new(None);
    fixture.set_process_state(ProcessState::Dead);

    let runtime = fixture.arm(SystemSchedulerTestMode::Normal);
    assert!(fixture
        .sink
        .events()
        .contains(&HelperEvent::WatchdogSchedulerReady));
    let _permit = runtime.listener_permit();
    assert!(fixture
        .sink
        .wait_for(HelperEvent::StateCleared, Duration::from_millis(500)));
    assert_eq!(fixture.store.load().unwrap(), None);
    assert!(!fixture.disabled());

    drop(runtime);
    assert!(fixture
        .sink
        .events()
        .contains(&HelperEvent::WatchdogSchedulerStopped));
    assert!(fixture
        .sink
        .events()
        .contains(&HelperEvent::WatchdogSchedulerJoined));
    let events = fixture.sink.events();
    let ready = events
        .iter()
        .position(|event| *event == HelperEvent::WatchdogSchedulerReady)
        .unwrap();
    let stopped = events
        .iter()
        .position(|event| *event == HelperEvent::WatchdogSchedulerStopped)
        .unwrap();
    let joined = events
        .iter()
        .position(|event| *event == HelperEvent::WatchdogSchedulerJoined)
        .unwrap();
    assert!(ready < stopped);
    assert!(stopped < joined);
}

#[test]
fn scheduler_and_requests_share_one_serialized_runtime_core() {
    let probe = BackendConcurrencyProbe::default();
    let mut fixture = SeededReady::new(Some(probe.clone()));
    let next_call = probe.calls() + 1;
    probe.block_next_call();
    let runtime = Arc::new(fixture.arm(SystemSchedulerTestMode::Normal));
    assert!(probe.wait_for_calls(next_call, Duration::from_millis(500)));

    let request_runtime = runtime.clone();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let request = thread::spawn(move || {
        let result = request_runtime.acquire(&owner(), "secondary", "generation-b", DEFAULT_TTL_MS);
        result_tx.send(result).unwrap();
    });
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(probe.maximum_active(), 1);

    probe.release();
    assert!(result_rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap()
        .is_ok());
    request.join().unwrap();
    assert_eq!(probe.maximum_active(), 1);
    drop(runtime);
}

#[test]
fn unexpected_scheduler_exit_or_panic_is_terminal_unhealthy_and_blocks_requests() {
    for mode in [
        SystemSchedulerTestMode::ExitAfterReady,
        SystemSchedulerTestMode::PanicAfterReady,
    ] {
        let mut fixture = SeededReady::new(None);
        let runtime = fixture.arm(mode);
        assert!(fixture.sink.wait_for(
            HelperEvent::WatchdogSchedulerTerminated,
            Duration::from_millis(500)
        ));
        assert_eq!(runtime.health(), RuntimeHealth::SchedulerTerminated);
        assert_eq!(
            runtime.acquire(&owner(), "secondary", "generation-b", DEFAULT_TTL_MS),
            Err(CoordinatorError::RecoveryRequired)
        );
        drop(runtime);
        assert!(fixture
            .sink
            .events()
            .contains(&HelperEvent::WatchdogSchedulerJoined));
    }
}

#[test]
fn scheduler_failure_blocks_mutations_retains_tombstone_and_retries_to_health() {
    let harness = Harness::baseline(false);
    let lease = harness
        .runtime
        .acquire(&owner(), "prod", "generation-a", DEFAULT_TTL_MS)
        .unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.fail_restore();

    harness.scheduler.trigger_interval();
    assert!(matches!(
        harness.runtime.health(),
        RuntimeHealth::Unhealthy {
            last_error: CoordinatorError::Pmset(PmsetError::CommandFailed),
            consecutive_failures: 1,
        }
    ));
    let tombstone = harness.store.load().unwrap().unwrap();
    assert_eq!(tombstone.phase, MutationPhase::RestorePending);
    assert!(tombstone.leases.is_empty());
    assert!(harness.disabled());

    harness.sink.clear();
    assert_eq!(
        harness
            .runtime
            .acquire(&owner(), "prod", "generation-b", DEFAULT_TTL_MS),
        Err(CoordinatorError::RecoveryRequired)
    );
    assert_eq!(
        harness
            .runtime
            .renew(&owner(), &lease.lease_id, "generation-a", DEFAULT_TTL_MS,),
        Err(CoordinatorError::RecoveryRequired)
    );
    assert_eq!(
        harness
            .runtime
            .release(&owner(), &lease.lease_id, "generation-a"),
        Err(CoordinatorError::RecoveryRequired)
    );
    assert_eq!(harness.store.load().unwrap(), Some(tombstone.clone()));
    assert!(!harness
        .sink
        .events()
        .contains(&HelperEvent::PowerWrite(false)));

    harness.scheduler.trigger_interval();
    assert!(matches!(
        harness.runtime.health(),
        RuntimeHealth::Unhealthy {
            consecutive_failures: 2,
            ..
        }
    ));
    harness.allow_restore();
    harness.scheduler.trigger_interval();
    assert_eq!(harness.runtime.health(), RuntimeHealth::Healthy);
    assert_eq!(harness.store.load().unwrap(), None);
    assert!(!harness.disabled());
    assert!(harness
        .runtime
        .acquire(&owner(), "prod", "generation-b", DEFAULT_TTL_MS)
        .is_ok());
}

#[test]
fn scheduler_is_the_only_post_startup_recovery_trigger_and_ticks_each_second() {
    let harness = Harness::baseline(false);
    assert_eq!(WATCHDOG_INTERVAL, Duration::from_secs(1));
    assert_eq!(harness.scheduler.interval(), Some(WATCHDOG_INTERVAL));
    assert_eq!(
        harness.scheduler.ready_timeout(),
        Some(WATCHDOG_READY_TIMEOUT)
    );
    assert!(WATCHDOG_READY_TIMEOUT > Duration::ZERO);
    assert!(WATCHDOG_READY_TIMEOUT <= Duration::from_secs(5));
    let source = include_str!("../../src/coordinator.rs");
    assert!(!source.contains("pub fn recover"));
    assert!(!source.contains("pub fn reconcile"));
}

#[test]
fn baseline_true_is_cleared_without_ever_writing_false() {
    let mut harness = Harness::baseline(true);
    harness.acquire(DEFAULT_TTL_MS).unwrap();
    harness.set_process_state(ProcessState::Dead);
    harness.sink.clear();

    harness.scheduler.trigger_interval();

    assert!(harness.disabled());
    assert!(!harness
        .sink
        .events()
        .contains(&HelperEvent::PowerWrite(false)));
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

    harness.scheduler.trigger_interval();
    assert!(matches!(
        harness.runtime.health(),
        RuntimeHealth::Unhealthy {
            last_error: CoordinatorError::Process(ProcessInspectionError::Unverifiable),
            ..
        }
    ));
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
fn failed_prepared_persist_never_reaches_the_power_mutation() {
    let mut harness = Harness::baseline(false);
    harness.store.arm_fault(StoreFault::TempFsync);

    assert_eq!(
        harness.acquire(DEFAULT_TTL_MS),
        Err(CoordinatorError::Store(StoreError::Unavailable))
    );
    assert!(!harness.disabled());
    assert!(!harness
        .sink
        .events()
        .iter()
        .any(|event| matches!(event, HelperEvent::PowerWrite(_))));
    assert_eq!(harness.store.load().unwrap(), None);
}

#[test]
fn ambiguous_pmset_failure_and_applied_persist_failure_reconcile_immediately() {
    let mut ambiguous = Harness::baseline(false);
    ambiguous.backend_state.lock().unwrap().fail_after_set = Some(true);
    assert_eq!(
        ambiguous.acquire(DEFAULT_TTL_MS),
        Err(CoordinatorError::Pmset(PmsetError::CommandFailed))
    );
    assert!(!ambiguous.disabled());
    assert_eq!(ambiguous.store.load().unwrap(), None);

    let mut applied_persist = Harness::baseline(false);
    applied_persist
        .backend_state
        .lock()
        .unwrap()
        .arm_fault_after_set = Some((applied_persist.store.clone(), StoreFault::TempFsync));
    assert_eq!(
        applied_persist.acquire(DEFAULT_TTL_MS),
        Err(CoordinatorError::Store(StoreError::Unavailable))
    );
    assert!(!applied_persist.disabled());
    assert_eq!(applied_persist.store.load().unwrap(), None);
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
