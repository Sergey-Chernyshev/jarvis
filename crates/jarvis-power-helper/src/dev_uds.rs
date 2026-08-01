use std::ffi::{CStr, CString, OsStr};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::root_store::{DevRoot, StateStore, StoreError};
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
const QUARANTINE_PREFIX: &str = ".power-helper-dev.cleanup-";
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
    _cleanup: SocketCleanup,
    #[cfg(test)]
    path: PathBuf,
}

impl DevListener {
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

struct SocketCleanup {
    run_directory: OwnedFd,
    identity: Option<SocketIdentity>,
    uid: u32,
    gid: u32,
    quarantine: CString,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        quarantine_owned_socket(
            self.run_directory.as_raw_fd(),
            self.identity,
            self.uid,
            self.gid,
            &self.quarantine,
        );
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
    root: &DevRoot,
    expected_uid: u32,
    events: Arc<dyn HelperEventSink>,
) -> Result<DevListener, TransportError> {
    bind_listener_inner(root, expected_uid, events, |_, _| {})
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindStage {
    AfterSocketPreparedBeforeProof,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum BindStage {
    AfterSocketPreparedBeforeProof,
}

#[cfg(test)]
pub(crate) fn bind_listener_with_hook_for_testing<F>(
    _permit: &ListenerPermit<'_>,
    root: &DevRoot,
    expected_uid: u32,
    events: Arc<dyn HelperEventSink>,
    hook: F,
) -> Result<DevListener, TransportError>
where
    F: FnOnce(BindStage, &Path),
{
    bind_listener_inner(root, expected_uid, events, hook)
}

fn bind_listener_inner<F>(
    root: &DevRoot,
    expected_uid: u32,
    events: Arc<dyn HelperEventSink>,
    hook: F,
) -> Result<DevListener, TransportError>
where
    F: FnOnce(BindStage, &Path),
{
    // SAFETY: these calls only read the current process identity.
    let actual_uid = unsafe { libc::geteuid() };
    // SAFETY: see the identity-only note above.
    let gid = unsafe { libc::getegid() };
    if expected_uid == 0 || expected_uid != actual_uid {
        return Err(TransportError::PeerRejected);
    }
    let run_directory = root.open_private_child(RUN_DIRECTORY)?;
    let run_metadata = stat_fd(run_directory.as_raw_fd())?;
    if permission_bits(&run_metadata) != DIRECTORY_MODE
        || run_metadata.st_uid != expected_uid
        || run_metadata.st_gid != gid
    {
        return Err(TransportError::UnsafeMetadata);
    }

    if socket_metadata(run_directory.as_raw_fd())?.is_some() {
        return Err(TransportError::UnsafeMetadata);
    }

    root.revalidate_child_path(RUN_DIRECTORY, run_directory.as_raw_fd())?;
    let path = root.path().join("run").join(DEV_SOCKET_FILE);
    let quarantine = new_quarantine_name()?;
    let listener = UnixListener::bind(&path).map_err(|_| TransportError::Io)?;
    let mut cleanup = SocketCleanup {
        run_directory,
        identity: None,
        uid: expected_uid,
        gid,
        quarantine,
    };
    let initial = socket_metadata(cleanup.run_directory.as_raw_fd())?
        .ok_or(TransportError::UnsafeMetadata)?;
    validate_socket_owner_kind(&initial, expected_uid, gid)?;
    let identity = SocketIdentity::from(initial);
    cleanup.identity = Some(identity);

    // SAFETY: the fixed single component is resolved relative to the held,
    // validated run directory and symlinks are never followed.
    if unsafe {
        libc::fchmodat(
            cleanup.run_directory.as_raw_fd(),
            DEV_SOCKET_COMPONENT.as_ptr(),
            SOCKET_MODE as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(TransportError::Io);
    }
    let metadata = socket_metadata(cleanup.run_directory.as_raw_fd())?
        .ok_or(TransportError::UnsafeMetadata)?;
    validate_socket_metadata(&metadata, expected_uid, gid)?;
    if SocketIdentity::from(metadata) != identity {
        return Err(TransportError::UnsafeMetadata);
    }

    hook(BindStage::AfterSocketPreparedBeforeProof, &path);
    root.revalidate_child_path(RUN_DIRECTORY, cleanup.run_directory.as_raw_fd())?;
    validate_owned_socket_entry(
        cleanup.run_directory.as_raw_fd(),
        identity,
        expected_uid,
        gid,
    )?;
    prove_listener_path(&listener, &path)?;
    root.revalidate_child_path(RUN_DIRECTORY, cleanup.run_directory.as_raw_fd())?;
    validate_owned_socket_entry(
        cleanup.run_directory.as_raw_fd(),
        identity,
        expected_uid,
        gid,
    )?;
    events.record(HelperEvent::DevListenerPublished);
    Ok(DevListener {
        listener,
        _cleanup: cleanup,
        #[cfg(test)]
        path,
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
    read_frame_with_timeout_for_testing(stream, IO_TIMEOUT)
}

#[cfg(test)]
pub(crate) fn read_frame_with_timeout_for_testing(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<Vec<u8>, TransportError> {
    set_nonblocking(stream.as_raw_fd())?;
    read_frame_until(stream, Deadline::after(timeout))
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
    let deadline = Deadline::after(IO_TIMEOUT);
    set_nonblocking(stream.as_raw_fd())?;
    let principal = authenticate_peer(&stream, expected_uid, peer)?;
    let frame = read_frame_until(&mut stream, deadline)?;
    observer.record(ConnectionEvent::FrameRead);
    let request = decode_request(frame)?;
    observer.record(ConnectionEvent::Decoded);
    let response = dispatcher.dispatch(&principal, request);
    observer.record(ConnectionEvent::Dispatched);
    let response = encode_response(&response)?;
    write_frame_until(&mut stream, &response, deadline)?;
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

fn read_frame_until(
    stream: &mut UnixStream,
    deadline: Deadline,
) -> Result<Vec<u8>, TransportError> {
    let mut prefix = [0_u8; 4];
    read_exact_until(stream, &mut prefix, deadline)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(TransportError::InvalidFrame);
    }
    let mut body = vec![0_u8; length];
    read_exact_until(stream, &mut body, deadline)?;
    let mut trailing = [0_u8; 1];
    loop {
        match stream.read(&mut trailing) {
            Ok(0) => return Ok(body),
            Ok(_) => return Err(TransportError::InvalidFrame),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(_) => return Err(TransportError::Io),
        }
    }
}

fn write_frame_until(
    stream: &mut UnixStream,
    body: &[u8],
    deadline: Deadline,
) -> Result<(), TransportError> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(TransportError::InvalidFrame);
    }
    let length = u32::try_from(body.len()).map_err(|_| TransportError::InvalidFrame)?;
    write_all_until(stream, &length.to_be_bytes(), deadline)?;
    write_all_until(stream, body, deadline)
}

#[derive(Clone, Copy)]
struct Deadline {
    expires_at: Instant,
}

impl Deadline {
    fn after(timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            expires_at: now.checked_add(timeout).unwrap_or(now),
        }
    }

    fn poll_timeout_millis(self) -> Result<i32, TransportError> {
        let remaining = self
            .expires_at
            .checked_duration_since(Instant::now())
            .ok_or(TransportError::Deadline)?;
        if remaining.is_zero() {
            return Err(TransportError::Deadline);
        }
        let rounded_up = remaining.as_millis().saturating_add(1);
        Ok(i32::try_from(rounded_up.min(i32::MAX as u128)).unwrap_or(i32::MAX))
    }
}

fn set_nonblocking(file: RawFd) -> Result<(), TransportError> {
    // SAFETY: F_GETFL/F_SETFL only inspect and update flags for the live fd.
    let flags = unsafe { libc::fcntl(file, libc::F_GETFL) };
    if flags < 0 {
        return Err(TransportError::Io);
    }
    if flags & libc::O_NONBLOCK != 0 {
        return Ok(());
    }
    // SAFETY: the original descriptor flags are preserved and O_NONBLOCK is
    // the only bit added.
    if unsafe { libc::fcntl(file, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(TransportError::Io)
    } else {
        Ok(())
    }
}

fn set_close_on_exec(file: RawFd) -> Result<(), TransportError> {
    // SAFETY: F_GETFD/F_SETFD only inspect and update descriptor-local flags.
    let flags = unsafe { libc::fcntl(file, libc::F_GETFD) };
    if flags < 0 {
        return Err(TransportError::Io);
    }
    // SAFETY: the original descriptor flags are preserved and FD_CLOEXEC is
    // the only bit added.
    if unsafe { libc::fcntl(file, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        Err(TransportError::Io)
    } else {
        Ok(())
    }
}

fn wait_fd(file: RawFd, events: i16, deadline: Deadline) -> Result<(), TransportError> {
    loop {
        let mut descriptor = libc::pollfd {
            fd: file,
            events,
            revents: 0,
        };
        let timeout = deadline.poll_timeout_millis()?;
        // SAFETY: descriptor points to one initialized pollfd for the duration
        // of the call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(TransportError::Io);
            }
            if descriptor.revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            continue;
        }
        if result == 0 {
            return Err(TransportError::Deadline);
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(TransportError::Io);
        }
    }
}

fn read_exact_until(
    stream: &mut UnixStream,
    mut destination: &mut [u8],
    deadline: Deadline,
) -> Result<(), TransportError> {
    while !destination.is_empty() {
        match stream.read(destination) {
            Ok(0) => return Err(TransportError::InvalidFrame),
            Ok(read) => {
                let (_, remaining) = destination.split_at_mut(read);
                destination = remaining;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(_) => return Err(TransportError::Io),
        }
    }
    Ok(())
}

fn write_all_until(
    stream: &mut UnixStream,
    mut source: &[u8],
    deadline: Deadline,
) -> Result<(), TransportError> {
    while !source.is_empty() {
        match stream.write(source) {
            Ok(0) => return Err(TransportError::Io),
            Ok(written) => source = &source[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLOUT, deadline)?;
            }
            Err(_) => return Err(TransportError::Io),
        }
    }
    Ok(())
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
    socket_metadata_named(directory, DEV_SOCKET_COMPONENT)
}

fn socket_metadata_named(
    directory: RawFd,
    component: &CStr,
) -> Result<Option<libc::stat>, TransportError> {
    // SAFETY: metadata is a valid output buffer and the fixed socket component
    // is resolved without following links beneath the held run directory fd.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            directory,
            component.as_ptr(),
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

fn validate_socket_owner_kind(
    metadata: &libc::stat,
    uid: u32,
    gid: u32,
) -> Result<(), TransportError> {
    if file_kind(metadata) != libc::S_IFSOCK
        || metadata.st_uid != uid
        || metadata.st_gid != gid
        || metadata.st_nlink != 1
    {
        Err(TransportError::UnsafeMetadata)
    } else {
        Ok(())
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

fn validate_owned_socket_entry(
    directory: RawFd,
    identity: SocketIdentity,
    uid: u32,
    gid: u32,
) -> Result<(), TransportError> {
    let metadata = socket_metadata(directory)?.ok_or(TransportError::UnsafeMetadata)?;
    validate_socket_metadata(&metadata, uid, gid)?;
    if SocketIdentity::from(metadata) == identity {
        Ok(())
    } else {
        Err(TransportError::UnsafeMetadata)
    }
}

fn new_quarantine_name() -> Result<CString, TransportError> {
    let mut randomness = [0_u8; 16];
    getrandom::getrandom(&mut randomness).map_err(|_| TransportError::Io)?;
    let mut name = String::with_capacity(QUARANTINE_PREFIX.len() + randomness.len() * 2);
    name.push_str(QUARANTINE_PREFIX);
    for byte in randomness {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").map_err(|_| TransportError::Io)?;
    }
    CString::new(name).map_err(|_| TransportError::Io)
}

#[cfg(target_os = "macos")]
fn quarantine_owned_socket(
    directory: RawFd,
    identity: Option<SocketIdentity>,
    uid: u32,
    gid: u32,
    quarantine: &CStr,
) {
    // SAFETY: both names are single NUL-terminated components beneath the
    // same held directory. RENAME_EXCL prevents overwriting any quarantine
    // collision.
    if unsafe {
        libc::renameatx_np(
            directory,
            DEV_SOCKET_COMPONENT.as_ptr(),
            directory,
            quarantine.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        return;
    }

    let retained_owned_socket = socket_metadata_named(directory, quarantine)
        .ok()
        .flatten()
        .filter(|metadata| validate_socket_metadata(metadata, uid, gid).is_ok())
        .is_some_and(|metadata| Some(SocketIdentity::from(metadata)) == identity);
    if identity.is_some() && !retained_owned_socket {
        // A raced sentinel is never deleted. Best effort restores its original
        // public name, but RENAME_EXCL also refuses to overwrite a new entry.
        // SAFETY: same held directory and fixed single components as above.
        let _ = unsafe {
            libc::renameatx_np(
                directory,
                quarantine.as_ptr(),
                directory,
                DEV_SOCKET_COMPONENT.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
    }
    // There is no Darwin primitive for "unlink this name only if it still has
    // the inode just validated". Retaining our random quarantine entry is the
    // fail-safe alternative to a stat/unlink swap race. With no validated
    // identity, retain the isolated entry as evidence instead of guessing.
}

#[cfg(not(target_os = "macos"))]
fn quarantine_owned_socket(
    _directory: RawFd,
    _identity: Option<SocketIdentity>,
    _uid: u32,
    _gid: u32,
    _quarantine: &CStr,
) {
}

fn prove_listener_path(listener: &UnixListener, path: &Path) -> Result<(), TransportError> {
    let deadline = Deadline::after(IO_TIMEOUT);
    listener
        .set_nonblocking(true)
        .map_err(|_| TransportError::Io)?;
    let mut client = connect_nonblocking(path, deadline)?;
    let (mut accepted, _) = accept_until(listener, deadline)?;
    set_nonblocking(accepted.as_raw_fd())?;

    let mut challenge = [0_u8; 32];
    getrandom::getrandom(&mut challenge).map_err(|_| TransportError::Io)?;
    write_all_until(&mut client, &challenge, deadline)?;
    let mut received = [0_u8; 32];
    read_exact_until(&mut accepted, &mut received, deadline)?;
    if received != challenge {
        return Err(TransportError::PeerRejected);
    }
    write_all_until(&mut accepted, &received, deadline)?;
    let mut echoed = [0_u8; 32];
    read_exact_until(&mut client, &mut echoed, deadline)?;
    if echoed != challenge {
        return Err(TransportError::PeerRejected);
    }
    listener
        .set_nonblocking(false)
        .map_err(|_| TransportError::Io)
}

fn accept_until(
    listener: &UnixListener,
    deadline: Deadline,
) -> Result<(UnixStream, std::os::unix::net::SocketAddr), TransportError> {
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(listener.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(_) => return Err(TransportError::Io),
        }
    }
}

fn connect_nonblocking(path: &Path, deadline: Deadline) -> Result<UnixStream, TransportError> {
    let path = path.as_os_str().as_bytes();
    if path.is_empty() || path.contains(&0) {
        return Err(TransportError::InvalidEnvironment);
    }
    // SAFETY: socket has no pointer arguments and returns a fresh descriptor.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(TransportError::Io);
    }
    // SAFETY: raw was just returned as an owned descriptor.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    set_close_on_exec(socket.as_raw_fd())?;
    set_nonblocking(socket.as_raw_fd())?;
    // SAFETY: sockaddr_un is plain data and zero is a valid initialization.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if path.len() >= address.sun_path.len() {
        return Err(TransportError::InvalidEnvironment);
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(path.iter().copied()) {
        *target = source as libc::c_char;
    }
    let address_base = (&address as *const libc::sockaddr_un).cast::<u8>() as usize;
    let path_base = address.sun_path.as_ptr().cast::<u8>() as usize;
    let address_length = path_base
        .checked_sub(address_base)
        .and_then(|offset| offset.checked_add(path.len() + 1))
        .ok_or(TransportError::InvalidEnvironment)?;
    let address_length = libc::socklen_t::try_from(address_length)
        .map_err(|_| TransportError::InvalidEnvironment)?;
    #[cfg(target_os = "macos")]
    {
        address.sun_len =
            u8::try_from(address_length).map_err(|_| TransportError::InvalidEnvironment)?;
    }

    // SAFETY: address and its calculated byte length remain valid for the
    // duration of the nonblocking connect call.
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address_length,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS)
            | Some(libc::EALREADY)
            | Some(libc::EWOULDBLOCK)
            | Some(libc::EINTR) => {
                wait_fd(socket.as_raw_fd(), libc::POLLOUT, deadline)?;
                let mut pending_error: libc::c_int = 0;
                let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                // SAFETY: pending_error and size describe one integer output
                // buffer for SO_ERROR on the connected socket.
                if unsafe {
                    libc::getsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&mut pending_error as *mut libc::c_int).cast(),
                        &mut size,
                    )
                } != 0
                    || size as usize != std::mem::size_of::<libc::c_int>()
                    || pending_error != 0
                {
                    return Err(TransportError::Io);
                }
            }
            Some(libc::EISCONN) => {}
            _ => return Err(TransportError::Io),
        }
    }
    // SAFETY: ownership moves from OwnedFd into UnixStream exactly once.
    Ok(unsafe { UnixStream::from_raw_fd(socket.into_raw_fd()) })
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
    u32::from(metadata.st_mode) & 0o7777
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
    let root = DevRoot::open(&jarvis_directory)?;
    let events: Arc<dyn HelperEventSink> = Arc::new(NoopEventSink);
    let store = DevStore::open(&root, events.clone())?;
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
    let listener = bind_listener(&runtime.listener_permit(), &root, uid, events)?;
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
