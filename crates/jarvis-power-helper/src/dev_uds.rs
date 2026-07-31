use std::ffi::{CStr, CString, OsStr};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jarvis_power_core::engine::{EngineError, RuntimeGuardFailureOutcome};
use jarvis_power_core::protocol::{
    decode_request, encode_response, ErrorCode, ProtocolError, Request, RequestEnvelope, Response,
    ResponseEnvelope, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use jarvis_power_core::state::{DarwinProcessIdentity, LeaseId, Principal};

use crate::coordinator::{
    CoordinatorError, MonotonicClock, ProcessInspector, RandomSource, SystemRandom,
};
use crate::dev_store::DevStore;
use crate::pmset::{DevSudoPmset, PmsetBackend};
use crate::root_store::{open_development_directory, StateStore, StoreError};
use crate::watchdog::{
    GenericServingRuntime, GenericStartupRuntime, ListenerPermit, SchedulerArmError,
    SystemMonotonicClock, SystemProcessInspector, HELPER_SERVICE_VERSION, MINIMUM_CLIENT_BUILD,
};
use crate::{HelperEvent, HelperEventSink, NoopEventSink};

pub const DEV_SOCKET_FILE: &str = "power-helper-dev.sock";

const RUN_DIRECTORY: &CStr = c"run";
const DEV_SOCKET_COMPONENT: &CStr = c"power-helper-dev.sock";
const DIRECTORY_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;
const IO_TIMEOUT: Duration = Duration::from_millis(250);
const DEV_BUNDLE_ID: &str = "app.jarvis.dev";
const DEV_TEAM_ID: &str = "JARVISDEV1";
const DEV_REQUIREMENT_DIGEST: [u8; 32] = [0x44; 32];
const DEV_SIGNED_BUILD: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    Unsupported,
    InvalidEnvironment,
    PeerRejected,
    UnsafeMetadata,
    Deadline,
    InvalidFrame,
    Io,
    Protocol(ProtocolError),
    Coordinator(CoordinatorError),
    Scheduler(SchedulerArmError),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "development power transport is unsupported",
            Self::InvalidEnvironment => "development power environment is invalid",
            Self::PeerRejected => "development power peer was rejected",
            Self::UnsafeMetadata => "development power socket metadata is unsafe",
            Self::Deadline => "development power transport deadline expired",
            Self::InvalidFrame => "development power frame is invalid",
            Self::Io => "development power transport I/O failed",
            Self::Protocol(_) => "development power protocol failed",
            Self::Coordinator(_) => "development power coordinator failed",
            Self::Scheduler(_) => "development power watchdog failed to start",
        })
    }
}

impl std::error::Error for TransportError {}

impl From<ProtocolError> for TransportError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<CoordinatorError> for TransportError {
    fn from(error: CoordinatorError) -> Self {
        Self::Coordinator(error)
    }
}

impl From<SchedulerArmError> for TransportError {
    fn from(error: SchedulerArmError) -> Self {
        Self::Scheduler(error)
    }
}

impl From<StoreError> for TransportError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::UnsafeMetadata => Self::UnsafeMetadata,
            _ => Self::Coordinator(CoordinatorError::Store(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionEvent {
    FrameRead,
    Decoded,
    Dispatched,
}

pub(crate) trait ConnectionObserver {
    fn record(&self, event: ConnectionEvent);
}

struct NoopConnectionObserver;

impl ConnectionObserver for NoopConnectionObserver {
    fn record(&self, _event: ConnectionEvent) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PeerSnapshot {
    pub(crate) socket_uid: Option<u32>,
    pub(crate) socket_gid: Option<u32>,
    pub(crate) socket_pid: Option<i32>,
    pub(crate) process_uid: Option<u32>,
    pub(crate) process_gid: Option<u32>,
    pub(crate) process_pid: Option<i32>,
    pub(crate) start_seconds: Option<u64>,
    pub(crate) start_microseconds: Option<u32>,
}

pub(crate) trait PeerIdentityProbe {
    fn snapshot(&self, stream: &UnixStream) -> Result<PeerSnapshot, TransportError>;
}

struct SystemPeerIdentityProbe;

#[cfg(target_os = "macos")]
impl PeerIdentityProbe for SystemPeerIdentityProbe {
    fn snapshot(&self, stream: &UnixStream) -> Result<PeerSnapshot, TransportError> {
        let socket = stream.as_raw_fd();
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: uid/gid are valid fixed-size output pointers and socket is
        // an accepted AF_UNIX stream descriptor.
        if unsafe { libc::getpeereid(socket, &mut uid, &mut gid) } != 0 {
            return Err(TransportError::PeerRejected);
        }

        let mut pid: libc::pid_t = 0;
        let mut pid_size = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: pid and pid_size describe one pid_t output buffer and the
        // queried option is the fixed Darwin local-peer PID.
        if unsafe {
            libc::getsockopt(
                socket,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut pid_size,
            )
        } != 0
            || pid_size as usize != std::mem::size_of::<libc::pid_t>()
            || pid <= 0
        {
            return Err(TransportError::PeerRejected);
        }

        // SAFETY: proc_bsdinfo is plain data and proc_pidinfo receives the
        // exact buffer size for the fixed peer PID.
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let expected = std::mem::size_of::<libc::proc_bsdinfo>();
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                i32::try_from(expected).map_err(|_| TransportError::PeerRejected)?,
            )
        };
        if received < 0 || usize::try_from(received).ok() != Some(expected) {
            return Err(TransportError::PeerRejected);
        }
        const PROCESS_STATUS_ZOMBIE: u32 = 5;
        if info.pbi_status == PROCESS_STATUS_ZOMBIE {
            return Err(TransportError::PeerRejected);
        }
        let process_pid = i32::try_from(info.pbi_pid).map_err(|_| TransportError::PeerRejected)?;
        let start_microseconds =
            u32::try_from(info.pbi_start_tvusec).map_err(|_| TransportError::PeerRejected)?;
        Ok(PeerSnapshot {
            socket_uid: Some(uid),
            socket_gid: Some(gid),
            socket_pid: Some(pid),
            process_uid: Some(info.pbi_uid),
            process_gid: Some(info.pbi_gid),
            process_pid: Some(process_pid),
            start_seconds: Some(info.pbi_start_tvsec),
            start_microseconds: Some(start_microseconds),
        })
    }
}

#[cfg(not(target_os = "macos"))]
impl PeerIdentityProbe for SystemPeerIdentityProbe {
    fn snapshot(&self, _stream: &UnixStream) -> Result<PeerSnapshot, TransportError> {
        Err(TransportError::Unsupported)
    }
}

pub(crate) trait RequestDispatcher {
    fn dispatch(&self, principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope;
}

pub(crate) struct RuntimeDispatcher<'a, B, C, P, R, S> {
    runtime: &'a GenericServingRuntime<B, C, P, R, S>,
}

impl<'a, B, C, P, R, S> RuntimeDispatcher<'a, B, C, P, R, S> {
    pub(crate) fn new(runtime: &'a GenericServingRuntime<B, C, P, R, S>) -> Self {
        Self { runtime }
    }
}

impl<B, C, P, R, S> RequestDispatcher for RuntimeDispatcher<'_, B, C, P, R, S>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
    S: StateStore,
{
    fn dispatch(&self, principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope {
        let RequestEnvelope {
            request_id,
            request,
            ..
        } = request;
        let response = match request {
            Request::AcquireLease {
                profile,
                owner_generation,
                ttl_ms,
            } => self
                .runtime
                .acquire(principal, &profile, &owner_generation, ttl_ms)
                .map(|grant| Response::Acquired {
                    lease_id: grant.lease_id.as_str().to_owned(),
                    granted_ttl_ms: grant.granted_ttl_ms,
                }),
            Request::RenewLease {
                lease_id,
                owner_generation,
                ttl_ms,
            } => LeaseId::parse(lease_id)
                .map_err(|_| CoordinatorError::Internal)
                .and_then(|lease_id| {
                    self.runtime
                        .renew(principal, &lease_id, &owner_generation, ttl_ms)
                })
                .map(|grant| Response::Renewed {
                    lease_id: grant.lease_id.as_str().to_owned(),
                    granted_ttl_ms: grant.granted_ttl_ms,
                }),
            Request::ReleaseLease {
                lease_id,
                owner_generation,
            } => LeaseId::parse(lease_id)
                .map_err(|_| CoordinatorError::Internal)
                .and_then(|lease_id| {
                    self.runtime
                        .release(principal, &lease_id, &owner_generation)
                        .map(|()| lease_id)
                })
                .map(|lease_id| Response::Released {
                    lease_id: lease_id.as_str().to_owned(),
                }),
            Request::Status => self.runtime.status().map(|status| Response::Status {
                active_leases: status.active_leases,
                mutation_active: status.mutation_active,
                recovery_required: status.recovery_required,
            }),
        }
        .unwrap_or_else(|error| Response::Error {
            code: coordinator_error_code(error),
        });
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response,
        }
    }
}

pub(crate) struct DevListener {
    listener: UnixListener,
    run_directory: OwnedFd,
    #[cfg(test)]
    path: PathBuf,
    identity: SocketIdentity,
    uid: u32,
    gid: u32,
}

impl DevListener {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DevListener {
    fn drop(&mut self) {
        let Ok(Some(metadata)) = socket_metadata(self.run_directory.as_raw_fd()) else {
            return;
        };
        if validate_socket_metadata(&metadata, self.uid, self.gid).is_ok()
            && SocketIdentity::from(metadata) == self.identity
        {
            // SAFETY: the exact fixed socket entry was revalidated against
            // the identity created by this listener. No other entry is
            // removed if validation or identity comparison fails.
            let _ = unsafe {
                libc::unlinkat(
                    self.run_directory.as_raw_fd(),
                    DEV_SOCKET_COMPONENT.as_ptr(),
                    0,
                )
            };
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SocketIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

impl From<libc::stat> for SocketIdentity {
    fn from(metadata: libc::stat) -> Self {
        Self {
            device: metadata.st_dev,
            inode: metadata.st_ino,
        }
    }
}

pub(crate) fn bind_listener(
    _permit: &ListenerPermit<'_>,
    jarvis_directory: &Path,
    expected_uid: u32,
    events: Arc<dyn HelperEventSink>,
) -> Result<DevListener, TransportError> {
    // SAFETY: these calls only read the current process identity.
    let actual_uid = unsafe { libc::geteuid() };
    // SAFETY: see the identity-only note above.
    let gid = unsafe { libc::getegid() };
    if expected_uid == 0 || expected_uid != actual_uid {
        return Err(TransportError::PeerRejected);
    }
    let run_directory = open_development_directory(jarvis_directory, RUN_DIRECTORY)?;
    let run_metadata = stat_fd(run_directory.as_raw_fd())?;
    if permission_bits(&run_metadata) != DIRECTORY_MODE
        || run_metadata.st_uid != expected_uid
        || run_metadata.st_gid != gid
    {
        return Err(TransportError::UnsafeMetadata);
    }

    if let Some(first) = socket_metadata(run_directory.as_raw_fd())? {
        validate_socket_metadata(&first, expected_uid, gid)?;
        let second =
            socket_metadata(run_directory.as_raw_fd())?.ok_or(TransportError::UnsafeMetadata)?;
        validate_socket_metadata(&second, expected_uid, gid)?;
        if SocketIdentity::from(first) != SocketIdentity::from(second) {
            return Err(TransportError::UnsafeMetadata);
        }
        // SAFETY: both no-follow metadata samples proved the fixed entry is
        // the same private socket. The parent is a held 0700 directory fd.
        if unsafe { libc::unlinkat(run_directory.as_raw_fd(), DEV_SOCKET_COMPONENT.as_ptr(), 0) }
            != 0
        {
            return Err(TransportError::Io);
        }
    }

    let path = jarvis_directory.join("run").join(DEV_SOCKET_FILE);
    let listener = UnixListener::bind(&path).map_err(|_| TransportError::Io)?;
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| TransportError::InvalidEnvironment)?;
    // SAFETY: path_c names the just-created fixed socket under the held
    // private run directory. chmod has no caller-controlled mode.
    if unsafe { libc::chmod(path_c.as_ptr(), SOCKET_MODE as libc::mode_t) } != 0 {
        return Err(TransportError::Io);
    }
    let metadata =
        socket_metadata(run_directory.as_raw_fd())?.ok_or(TransportError::UnsafeMetadata)?;
    validate_socket_metadata(&metadata, expected_uid, gid)?;
    events.record(HelperEvent::DevListenerPublished);
    Ok(DevListener {
        listener,
        run_directory,
        #[cfg(test)]
        path,
        identity: SocketIdentity::from(metadata),
        uid: expected_uid,
        gid,
    })
}

#[cfg(test)]
pub(crate) fn handle_connection_for_testing<P, D, O>(
    stream: UnixStream,
    expected_uid: u32,
    peer: &P,
    dispatcher: &D,
    observer: &O,
) -> Result<(), TransportError>
where
    P: PeerIdentityProbe,
    D: RequestDispatcher,
    O: ConnectionObserver,
{
    handle_connection(stream, expected_uid, peer, dispatcher, observer)
}

#[cfg(test)]
pub(crate) fn read_frame_for_testing(stream: &mut UnixStream) -> Result<Vec<u8>, TransportError> {
    read_frame(stream)
}

fn handle_connection<P, D, O>(
    mut stream: UnixStream,
    expected_uid: u32,
    peer: &P,
    dispatcher: &D,
    observer: &O,
) -> Result<(), TransportError>
where
    P: PeerIdentityProbe,
    D: RequestDispatcher,
    O: ConnectionObserver,
{
    configure_deadlines(&stream)?;
    let principal = authenticate_peer(&stream, expected_uid, peer)?;
    let frame = read_frame(&mut stream)?;
    observer.record(ConnectionEvent::FrameRead);
    let request = decode_request(frame)?;
    observer.record(ConnectionEvent::Decoded);
    let response = dispatcher.dispatch(&principal, request);
    observer.record(ConnectionEvent::Dispatched);
    let response = encode_response(&response)?;
    write_frame(&mut stream, &response)?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

fn authenticate_peer<P>(
    stream: &UnixStream,
    expected_uid: u32,
    peer: &P,
) -> Result<Principal, TransportError>
where
    P: PeerIdentityProbe,
{
    if expected_uid == 0 {
        return Err(TransportError::PeerRejected);
    }
    let first = peer.snapshot(stream)?;
    let second = peer.snapshot(stream)?;
    if first != second {
        return Err(TransportError::PeerRejected);
    }
    let socket_uid = first.socket_uid.ok_or(TransportError::PeerRejected)?;
    let socket_gid = first.socket_gid.ok_or(TransportError::PeerRejected)?;
    let socket_pid = first.socket_pid.ok_or(TransportError::PeerRejected)?;
    let process_uid = first.process_uid.ok_or(TransportError::PeerRejected)?;
    let process_gid = first.process_gid.ok_or(TransportError::PeerRejected)?;
    let process_pid = first.process_pid.ok_or(TransportError::PeerRejected)?;
    let start_seconds = first.start_seconds.ok_or(TransportError::PeerRejected)?;
    let start_microseconds = first
        .start_microseconds
        .ok_or(TransportError::PeerRejected)?;
    if socket_uid != expected_uid
        || socket_gid == 0
        || socket_pid <= 0
        || socket_uid != process_uid
        || socket_gid != process_gid
        || socket_pid != process_pid
        || start_seconds == 0
        || start_microseconds >= 1_000_000
    {
        return Err(TransportError::PeerRejected);
    }
    let identity = DarwinProcessIdentity::new(1, start_seconds, start_microseconds)
        .map_err(|_| TransportError::PeerRejected)?;
    Principal::from_helper_attestation(
        socket_uid,
        socket_pid,
        identity,
        DEV_BUNDLE_ID,
        DEV_TEAM_ID,
        DEV_REQUIREMENT_DIGEST,
        DEV_SIGNED_BUILD,
    )
    .map_err(|_| TransportError::PeerRejected)
}

fn configure_deadlines(stream: &UnixStream) -> Result<(), TransportError> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|_| TransportError::Io)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|_| TransportError::Io)
}

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, TransportError> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(map_transport_io_error)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(TransportError::InvalidFrame);
    }
    let mut body = vec![0_u8; length];
    stream
        .read_exact(&mut body)
        .map_err(map_transport_io_error)?;
    let mut trailing = [0_u8; 1];
    match stream.read(&mut trailing) {
        Ok(0) => Ok(body),
        Ok(_) => Err(TransportError::InvalidFrame),
        Err(error) => Err(map_transport_io_error(error)),
    }
}

fn write_frame(stream: &mut UnixStream, body: &[u8]) -> Result<(), TransportError> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(TransportError::InvalidFrame);
    }
    let length = u32::try_from(body.len()).map_err(|_| TransportError::InvalidFrame)?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(map_transport_io_error)?;
    stream.write_all(body).map_err(map_transport_io_error)
}

fn map_transport_io_error(error: io::Error) -> TransportError {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => TransportError::Deadline,
        io::ErrorKind::UnexpectedEof => TransportError::InvalidFrame,
        _ => TransportError::Io,
    }
}

fn coordinator_error_code(error: CoordinatorError) -> ErrorCode {
    match error {
        CoordinatorError::Store(_) => ErrorCode::StateUnavailable,
        CoordinatorError::Pmset(_)
        | CoordinatorError::Random(_)
        | CoordinatorError::ClockUnavailable => ErrorCode::HelperUnavailable,
        CoordinatorError::Process(_)
        | CoordinatorError::VerificationFailed { .. }
        | CoordinatorError::RecoveryRequired => ErrorCode::RecoveryRequired,
        CoordinatorError::RuntimeGuard(outcome) => match outcome {
            RuntimeGuardFailureOutcome::Recovered(_) => ErrorCode::LeaseExpired,
            RuntimeGuardFailureOutcome::RecoveryRequired(_) => ErrorCode::RecoveryRequired,
        },
        CoordinatorError::Engine(engine) => match engine {
            EngineError::LeaseNotFound => ErrorCode::LeaseNotFound,
            EngineError::Expired | EngineError::LeaseNearExpiry => ErrorCode::LeaseExpired,
            EngineError::PrincipalMismatch => ErrorCode::Unauthorized,
            EngineError::OwnerGenerationMismatch => ErrorCode::OwnerMismatch,
            EngineError::DuplicateLease
            | EngineError::LeaseLimitReached
            | EngineError::NotExtended => ErrorCode::Conflict,
            EngineError::ClientBuildTooOld => ErrorCode::IncompatibleVersion,
            EngineError::PolicyMismatch
            | EngineError::MutationGenerationMismatch
            | EngineError::BootMismatch
            | EngineError::ProcessUnverifiable
            | EngineError::RecoveryRequired
            | EngineError::ObservedStateMismatch => ErrorCode::RecoveryRequired,
            EngineError::CorruptState(_) => ErrorCode::StateUnavailable,
            EngineError::InvalidIdentifier
            | EngineError::InvalidTtl
            | EngineError::DeadlineOverflow
            | EngineError::InvalidMutationGeneration => ErrorCode::InvalidRequest,
        },
        CoordinatorError::Internal => ErrorCode::Internal,
    }
}

fn socket_metadata(directory: RawFd) -> Result<Option<libc::stat>, TransportError> {
    // SAFETY: metadata is a valid output buffer and the fixed socket component
    // is resolved without following links beneath the held run directory fd.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            directory,
            DEV_SOCKET_COMPONENT.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(Some(metadata))
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(TransportError::Io)
        }
    }
}

fn validate_socket_metadata(
    metadata: &libc::stat,
    uid: u32,
    gid: u32,
) -> Result<(), TransportError> {
    if file_kind(metadata) != libc::S_IFSOCK
        || metadata.st_uid != uid
        || metadata.st_gid != gid
        || permission_bits(metadata) != SOCKET_MODE
        || metadata.st_nlink != 1
    {
        Err(TransportError::UnsafeMetadata)
    } else {
        Ok(())
    }
}

fn stat_fd(file: RawFd) -> Result<libc::stat, TransportError> {
    // SAFETY: metadata is a valid output pointer and file is an owned fd held
    // by the caller.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(file, &mut metadata) } == 0 {
        Ok(metadata)
    } else {
        Err(TransportError::Io)
    }
}

fn file_kind(metadata: &libc::stat) -> libc::mode_t {
    metadata.st_mode & libc::S_IFMT
}

fn permission_bits(metadata: &libc::stat) -> u32 {
    u32::from(metadata.st_mode) & 0o777
}

#[cfg(target_os = "macos")]
pub fn run_from_environment() -> Result<(), TransportError> {
    if !development_runtime_enabled(std::env::var_os("JARVIS_DEV").as_deref()) {
        return Err(TransportError::InvalidEnvironment);
    }
    let jarvis_directory =
        PathBuf::from(std::env::var_os("JARVIS_DIR").ok_or(TransportError::InvalidEnvironment)?);
    if !jarvis_directory.is_absolute() {
        return Err(TransportError::InvalidEnvironment);
    }
    // SAFETY: reads the current helper identity without mutating it.
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return Err(TransportError::PeerRejected);
    }
    let events: Arc<dyn HelperEventSink> = Arc::new(NoopEventSink);
    let store = DevStore::open(&jarvis_directory, events.clone())?;
    let ready = GenericStartupRuntime::from_parts(
        store,
        DevSudoPmset,
        SystemMonotonicClock,
        SystemProcessInspector,
        SystemRandom,
        HELPER_SERVICE_VERSION,
        MINIMUM_CLIENT_BUILD,
    )
    .reconcile_before_listener()?;
    let runtime = ready.arm_system()?;
    let listener = bind_listener(&runtime.listener_permit(), &jarvis_directory, uid, events)?;
    loop {
        let (stream, _) = listener.listener.accept().map_err(|_| TransportError::Io)?;
        let dispatcher = RuntimeDispatcher::new(&runtime);
        let _ = handle_connection(
            stream,
            uid,
            &SystemPeerIdentityProbe,
            &dispatcher,
            &NoopConnectionObserver,
        );
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn development_runtime_enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

#[cfg(not(target_os = "macos"))]
pub fn run_from_environment() -> Result<(), TransportError> {
    Err(TransportError::Unsupported)
}
