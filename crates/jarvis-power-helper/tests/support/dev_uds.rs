use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jarvis_power_core::engine::ProcessState;
use jarvis_power_core::protocol::{
    decode_response, encode_request, Request, RequestEnvelope, RequestId, Response,
    ResponseEnvelope, DEFAULT_TTL_MS, PROTOCOL_VERSION,
};
use jarvis_power_core::state::{DarwinProcessIdentity, LeaseId, MonotonicTime, Principal};
use jarvis_power_helper::coordinator::{
    CoordinatorError, MonotonicClock, ProcessInspectionError, ProcessInspector, RandomError,
    RandomSource,
};
use jarvis_power_helper::dev_store::{DevStore, DEV_LOCK_FILE, DEV_STATE_FILE};
use jarvis_power_helper::dev_uds::{
    bind_listener, handle_connection_for_testing, read_frame_for_testing, ConnectionEvent,
    ConnectionObserver, PeerIdentityProbe, PeerSnapshot, RequestDispatcher, RuntimeDispatcher,
    TransportError, DEV_SOCKET_FILE,
};
use jarvis_power_helper::pmset::{PmsetBackend, PmsetError};
use jarvis_power_helper::watchdog::{
    SchedulerArmError, SchedulerFactory, ServingRuntime, StartupRuntime, WatchdogGuard,
    WatchdogTask, WatchdogTermination,
};
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
    disabled: Arc<Mutex<bool>>,
}

impl PmsetBackend for FakeBackend {
    fn read_disabled(&mut self) -> Result<bool, PmsetError> {
        Ok(*self.disabled.lock().unwrap())
    }

    fn set_disabled(&mut self, value: bool) -> Result<(), PmsetError> {
        *self.disabled.lock().unwrap() = value;
        Ok(())
    }

    fn boot_id(&mut self) -> Result<String, PmsetError> {
        Ok("boot-dev-contract".into())
    }
}

#[derive(Clone)]
struct FakeClock {
    now: Arc<Mutex<u64>>,
}

impl MonotonicClock for FakeClock {
    fn now(&mut self) -> Result<MonotonicTime, CoordinatorError> {
        Ok(MonotonicTime::from_millis(*self.now.lock().unwrap()))
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
    lease: u128,
    generation: u64,
}

impl RandomSource for FixedRandom {
    fn next_lease_id(&mut self) -> Result<LeaseId, RandomError> {
        let lease =
            LeaseId::parse(format!("{:032x}", self.lease)).map_err(|_| RandomError::Unavailable)?;
        self.lease += 1;
        Ok(lease)
    }

    fn next_mutation_generation(&mut self) -> Result<u64, RandomError> {
        let generation = self.generation;
        self.generation += 1;
        Ok(generation)
    }
}

#[derive(Clone)]
struct ReadyScheduler {
    task: Arc<Mutex<Option<WatchdogTask>>>,
    sink: Arc<RecordingSink>,
}

impl ReadyScheduler {
    fn trigger(&self) {
        let mut task = self.task.lock().unwrap().take().unwrap();
        task();
        *self.task.lock().unwrap() = Some(task);
    }
}

struct ReadyGuard {
    task: Arc<Mutex<Option<WatchdogTask>>>,
}

impl WatchdogGuard for ReadyGuard {}

impl Drop for ReadyGuard {
    fn drop(&mut self) {
        self.task.lock().unwrap().take();
    }
}

impl SchedulerFactory for ReadyScheduler {
    fn start(
        self,
        _interval: Duration,
        _ready_timeout: Duration,
        task: WatchdogTask,
        _termination: WatchdogTermination,
    ) -> Result<Box<dyn WatchdogGuard>, SchedulerArmError> {
        *self.task.lock().unwrap() = Some(task);
        self.sink.record(HelperEvent::WatchdogSchedulerReady);
        Ok(Box::new(ReadyGuard {
            task: self.task.clone(),
        }))
    }
}

type Runtime = ServingRuntime<FakeBackend, FakeClock, FakeProcesses, FixedRandom, DevStore>;

struct DevHarness {
    _temp: TempDir,
    jarvis_dir: PathBuf,
    store: DevStore,
    sink: Arc<RecordingSink>,
    disabled: Arc<Mutex<bool>>,
    process: Arc<Mutex<ProcessState>>,
    scheduler: ReadyScheduler,
    runtime: Runtime,
}

impl DevHarness {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let jarvis_dir = temp.path().join("jarvis");
        fs::create_dir(&jarvis_dir).unwrap();
        fs::set_permissions(&jarvis_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let sink = Arc::new(RecordingSink::default());
        let store = DevStore::open_for_testing(&jarvis_dir, sink.clone()).unwrap();
        let disabled = Arc::new(Mutex::new(false));
        let process = Arc::new(Mutex::new(ProcessState::AliveExact));
        let clock = FakeClock {
            now: Arc::new(Mutex::new(1_000)),
        };
        let scheduler = ReadyScheduler {
            task: Arc::new(Mutex::new(None)),
            sink: sink.clone(),
        };
        let ready = StartupRuntime::new(
            store.clone(),
            FakeBackend {
                disabled: disabled.clone(),
            },
            clock,
            FakeProcesses {
                state: process.clone(),
            },
            FixedRandom {
                lease: 1,
                generation: 10,
            },
            1,
            1,
        )
        .reconcile_before_listener()
        .unwrap();
        let runtime = ready.arm_with_scheduler(scheduler.clone()).unwrap();
        Self {
            _temp: temp,
            jarvis_dir,
            store,
            sink,
            disabled,
            process,
            scheduler,
            runtime,
        }
    }
}

fn owner() -> Principal {
    Principal::from_helper_attestation(
        current_uid(),
        42,
        DarwinProcessIdentity::new(1, 1_700_000_000, 7).unwrap(),
        "app.jarvis.dev",
        "JARVISDEV1",
        [0x44; 32],
        1,
    )
    .unwrap()
}

fn current_uid() -> u32 {
    // SAFETY: reads the current test process identity without mutating it.
    unsafe { libc::geteuid() }
}

fn current_gid() -> u32 {
    // SAFETY: reads the current test process identity without mutating it.
    unsafe { libc::getegid() }
}

fn valid_peer() -> PeerSnapshot {
    PeerSnapshot {
        socket_uid: Some(current_uid()),
        socket_gid: Some(current_gid()),
        socket_pid: Some(42),
        process_uid: Some(current_uid()),
        process_gid: Some(current_gid()),
        process_pid: Some(42),
        start_seconds: Some(1_700_000_000),
        start_microseconds: Some(7),
    }
}

struct ScriptedPeer {
    snapshots: Mutex<VecDeque<PeerSnapshot>>,
}

impl ScriptedPeer {
    fn stable(snapshot: PeerSnapshot) -> Self {
        Self {
            snapshots: Mutex::new(VecDeque::from([snapshot, snapshot])),
        }
    }
}

impl PeerIdentityProbe for ScriptedPeer {
    fn snapshot(&self, _stream: &UnixStream) -> Result<PeerSnapshot, TransportError> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(TransportError::PeerRejected)
    }
}

#[derive(Default)]
struct Observer {
    events: Mutex<Vec<ConnectionEvent>>,
}

impl Observer {
    fn count(&self, event: ConnectionEvent) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|candidate| **candidate == event)
            .count()
    }
}

impl ConnectionObserver for Observer {
    fn record(&self, event: ConnectionEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Default)]
struct FakeDispatcher {
    calls: Mutex<usize>,
}

impl RequestDispatcher for FakeDispatcher {
    fn dispatch(&self, _principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope {
        *self.calls.lock().unwrap() += 1;
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            response: Response::Status {
                active_leases: 0,
                mutation_active: false,
                recovery_required: false,
            },
        }
    }
}

fn request(id: &str, request: Request) -> RequestEnvelope {
    RequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse(id).unwrap(),
        request,
    }
}

fn framed_request(request: &RequestEnvelope) -> Vec<u8> {
    let body = encode_request(request).unwrap();
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn run_raw(
    bytes: &[u8],
    keep_write_open: bool,
    peer: &ScriptedPeer,
    dispatcher: &impl RequestDispatcher,
    observer: &impl ConnectionObserver,
) -> Result<(), TransportError> {
    let (mut client, server) = UnixStream::pair().unwrap();
    client.write_all(bytes).unwrap();
    if !keep_write_open {
        client.shutdown(std::net::Shutdown::Write).unwrap();
    }
    let result = handle_connection_for_testing(server, current_uid(), peer, dispatcher, observer);
    drop(client);
    result
}

#[test]
fn dev_store_uses_the_same_locked_decide_persist_mutate_readback_transaction() {
    let harness = DevHarness::new();
    harness.sink.clear();

    harness
        .runtime
        .acquire(&owner(), "prod", "generation-a", DEFAULT_TTL_MS)
        .unwrap();

    assert_eq!(
        harness.sink.events(),
        vec![
            HelperEvent::LockAcquired,
            HelperEvent::PowerRead(false),
            HelperEvent::StateWriteStarted(jarvis_power_core::state::MutationPhase::Prepared),
            HelperEvent::TempFileSynced,
            HelperEvent::StateRenamed,
            HelperEvent::ParentDirectorySynced,
            HelperEvent::PowerWrite(true),
            HelperEvent::PowerRead(true),
            HelperEvent::StateWriteStarted(jarvis_power_core::state::MutationPhase::Applied),
            HelperEvent::TempFileSynced,
            HelperEvent::StateRenamed,
            HelperEvent::ParentDirectorySynced,
            HelperEvent::ReplyReady,
            HelperEvent::LockReleased,
        ]
    );
}

#[test]
fn wrong_or_inconsistent_peer_is_rejected_before_frame_read_and_decode() {
    let mut cases = Vec::new();
    let mut wrong_uid = valid_peer();
    wrong_uid.socket_uid = Some(current_uid().saturating_add(1));
    cases.push([wrong_uid, wrong_uid]);
    let mut wrong_gid = valid_peer();
    wrong_gid.process_gid = Some(current_gid().saturating_add(1));
    cases.push([wrong_gid, wrong_gid]);
    let mut missing_pid = valid_peer();
    missing_pid.socket_pid = None;
    cases.push([missing_pid, missing_pid]);
    let mut mismatched_pid = valid_peer();
    mismatched_pid.process_pid = Some(43);
    cases.push([mismatched_pid, mismatched_pid]);
    let mut missing_start = valid_peer();
    missing_start.start_seconds = None;
    cases.push([missing_start, missing_start]);
    let mut changed_start = valid_peer();
    changed_start.start_microseconds = Some(8);
    cases.push([valid_peer(), changed_start]);

    for snapshots in cases {
        let peer = ScriptedPeer {
            snapshots: Mutex::new(VecDeque::from(snapshots)),
        };
        let dispatcher = FakeDispatcher::default();
        let observer = Observer::default();
        assert_eq!(
            run_raw(&[0, 0, 0, 0], false, &peer, &dispatcher, &observer),
            Err(TransportError::PeerRejected)
        );
        assert_eq!(observer.count(ConnectionEvent::FrameRead), 0);
        assert_eq!(observer.count(ConnectionEvent::Decoded), 0);
        assert_eq!(observer.count(ConnectionEvent::Dispatched), 0);
        assert_eq!(*dispatcher.calls.lock().unwrap(), 0);
    }
}

#[test]
fn malformed_or_ambiguous_frames_never_dispatch() {
    let valid = framed_request(&request(
        "018f0000-0000-7000-8000-000000000001",
        Request::Status,
    ));
    let mut oversized =
        Vec::from(((jarvis_power_core::protocol::MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
    oversized.extend_from_slice(b"x");
    let mut truncated_body = Vec::from(8_u32.to_be_bytes());
    truncated_body.extend_from_slice(b"tiny");
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut concatenated = valid.clone();
    concatenated.extend_from_slice(&valid);
    let cases = vec![
        (Vec::from(0_u32.to_be_bytes()), false),
        (oversized, false),
        (vec![0, 0], false),
        (truncated_body, false),
        (trailing, false),
        (concatenated, false),
        (valid, true),
    ];

    for (bytes, keep_write_open) in cases {
        let dispatcher = FakeDispatcher::default();
        let observer = Observer::default();
        assert!(run_raw(
            &bytes,
            keep_write_open,
            &ScriptedPeer::stable(valid_peer()),
            &dispatcher,
            &observer,
        )
        .is_err());
        assert_eq!(observer.count(ConnectionEvent::Decoded), 0);
        assert_eq!(observer.count(ConnectionEvent::Dispatched), 0);
        assert_eq!(*dispatcher.calls.lock().unwrap(), 0);
    }
}

#[test]
fn socket_and_dev_state_are_private_without_following_or_overwriting() {
    let harness = DevHarness::new();
    harness
        .runtime
        .acquire(&owner(), "prod", "generation-a", DEFAULT_TTL_MS)
        .unwrap();
    let listener = bind_listener(
        &harness.runtime.listener_permit(),
        &harness.jarvis_dir,
        current_uid(),
        harness.sink.clone(),
    )
    .unwrap();

    let run = harness.jarvis_dir.join("run");
    let power = harness.jarvis_dir.join("power");
    assert_eq!(fs::metadata(&run).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::symlink_metadata(listener.path()).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&power).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        [
            fs::metadata(power.join(DEV_STATE_FILE)).unwrap().mode() & 0o777,
            fs::metadata(power.join(DEV_LOCK_FILE)).unwrap().mode() & 0o777,
        ],
        [0o600, 0o600]
    );
    drop(listener);

    let sentinel = harness._temp.path().join("sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    symlink(&sentinel, run.join(DEV_SOCKET_FILE)).unwrap();
    assert!(bind_listener(
        &harness.runtime.listener_permit(),
        &harness.jarvis_dir,
        current_uid(),
        harness.sink.clone(),
    )
    .is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
}

fn round_trip(
    runtime: &Runtime,
    envelope: &RequestEnvelope,
) -> Result<ResponseEnvelope, TransportError> {
    let (mut client, server) = UnixStream::pair().unwrap();
    client.write_all(&framed_request(envelope)).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    handle_connection_for_testing(
        server,
        current_uid(),
        &ScriptedPeer::stable(valid_peer()),
        &RuntimeDispatcher::new(runtime),
        &Observer::default(),
    )?;
    let bytes = read_frame_for_testing(&mut client)?;
    decode_response(bytes).map_err(TransportError::Protocol)
}

#[test]
fn armed_runtime_round_trips_and_watchdog_recovers_without_a_uds_request() {
    let harness = DevHarness::new();
    let acquired = round_trip(
        &harness.runtime,
        &request(
            "018f0000-0000-7000-8000-000000000001",
            Request::AcquireLease {
                profile: "prod".into(),
                owner_generation: "generation-a".into(),
                ttl_ms: DEFAULT_TTL_MS,
            },
        ),
    )
    .unwrap();
    let lease_id = match acquired.response {
        Response::Acquired { lease_id, .. } => lease_id,
        response => panic!("unexpected response: {response:?}"),
    };
    let idempotent = round_trip(
        &harness.runtime,
        &request(
            "018f0000-0000-7000-8000-000000000002",
            Request::AcquireLease {
                profile: "prod".into(),
                owner_generation: "generation-a".into(),
                ttl_ms: DEFAULT_TTL_MS,
            },
        ),
    )
    .unwrap();
    assert!(matches!(
        idempotent.response,
        Response::Acquired {
            lease_id: ref existing,
            ..
        } if existing == &lease_id
    ));
    let status = round_trip(
        &harness.runtime,
        &request("018f0000-0000-7000-8000-000000000003", Request::Status),
    )
    .unwrap();
    assert!(matches!(
        status.response,
        Response::Status {
            active_leases: 1,
            mutation_active: true,
            recovery_required: false,
        }
    ));

    *harness.process.lock().unwrap() = ProcessState::Dead;
    harness.scheduler.trigger();

    assert!(!*harness.disabled.lock().unwrap());
    assert!(harness.store.load().unwrap().is_none());
}

#[test]
fn listener_publication_follows_recovery_and_scheduler_acknowledgement() {
    let harness = DevHarness::new();
    let listener = bind_listener(
        &harness.runtime.listener_permit(),
        &harness.jarvis_dir,
        current_uid(),
        harness.sink.clone(),
    )
    .unwrap();
    let events = harness.sink.events();
    let startup = events
        .iter()
        .position(|event| *event == HelperEvent::StartupReady)
        .unwrap();
    let armed = events
        .iter()
        .position(|event| *event == HelperEvent::WatchdogSchedulerReady)
        .unwrap();
    let published = events
        .iter()
        .position(|event| *event == HelperEvent::DevListenerPublished)
        .unwrap();
    assert!(startup < armed);
    assert!(armed < published);
    drop(listener);
}
