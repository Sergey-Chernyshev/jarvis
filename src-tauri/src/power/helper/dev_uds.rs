use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_power_core::protocol::{
    decode_response, encode_request, Request, RequestEnvelope, RequestId, MAX_FRAME_BYTES,
    PROTOCOL_VERSION,
};

use super::client::{map_io_error, HelperClient, HelperClientError, HelperReply, HelperTrust};

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const SOCKET_NAME: &str = "power-helper-dev.sock";
const RUN_COMPONENT: &CStr = c"run";
const SOCKET_COMPONENT: &CStr = c"power-helper-dev.sock";
const DIRECTORY_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;

#[derive(Clone, Debug)]
pub(crate) struct DevUdsClient {
    jarvis_directory: PathBuf,
}

impl DevUdsClient {
    pub(crate) fn new(jarvis_directory: impl AsRef<Path>) -> Self {
        Self {
            jarvis_directory: jarvis_directory.as_ref().to_path_buf(),
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.jarvis_directory.join("run").join(SOCKET_NAME)
    }

    fn connect_until<P, F>(
        &self,
        deadline: Deadline,
        peer: &P,
        hook: F,
    ) -> Result<UnixStream, HelperClientError>
    where
        P: ServerPeerIdentityProbe,
        F: FnOnce(ClientConnectStage),
    {
        let endpoint = HeldDevEndpoint::open(&self.jarvis_directory)?;
        deadline.ensure_remaining()?;
        let stream = connect_path_until(&self.socket_path(), deadline)?;
        let first = peer.snapshot(&stream)?;
        hook(ClientConnectStage::AfterConnectBeforeRevalidation);
        endpoint.revalidate()?;
        deadline.ensure_remaining()?;
        let second = peer.snapshot(&stream)?;
        validate_server_peer(first, second, endpoint.uid, endpoint.gid)?;
        deadline.ensure_remaining()?;
        Ok(stream)
    }

    #[cfg(test)]
    fn connect_with_peer_for_testing<P>(&self, peer: &P) -> Result<UnixStream, HelperClientError>
    where
        P: ServerPeerIdentityProbe,
    {
        self.connect_until(Deadline::after(IO_TIMEOUT), peer, |_| {})
    }

    #[cfg(test)]
    fn connect_with_hook_for_testing<P, F>(
        &self,
        peer: &P,
        hook: F,
    ) -> Result<UnixStream, HelperClientError>
    where
        P: ServerPeerIdentityProbe,
        F: FnOnce(ClientConnectStage),
    {
        self.connect_until(Deadline::after(IO_TIMEOUT), peer, hook)
    }
}

impl HelperClient for DevUdsClient {
    fn send(&self, request: Request) -> Result<HelperReply, HelperClientError> {
        let deadline = Deadline::after(IO_TIMEOUT);
        let request_id = next_request_id()?;
        let envelope = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            request,
        };
        let body = encode_request(&envelope).map_err(HelperClientError::Protocol)?;
        let mut stream = self.connect_until(deadline, &SystemServerPeerIdentityProbe, |_| {})?;
        write_frame_until(&mut stream, &body, deadline)?;
        deadline.ensure_remaining()?;
        stream.shutdown(Shutdown::Write).map_err(map_io_error)?;
        let body = read_frame_until(&mut stream, deadline)?;
        let response = decode_response(body).map_err(HelperClientError::Protocol)?;
        if response.request_id != request_id {
            return Err(HelperClientError::ResponseRequestIdMismatch);
        }
        Ok(HelperReply {
            response,
            trust: self.trust(),
        })
    }

    fn trust(&self) -> HelperTrust {
        HelperTrust::DevelopmentOnly
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientConnectStage {
    AfterConnectBeforeRevalidation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ServerPeerSnapshot {
    pub(crate) socket_uid: Option<u32>,
    pub(crate) socket_gid: Option<u32>,
    pub(crate) socket_pid: Option<i32>,
    pub(crate) process_uid: Option<u32>,
    pub(crate) process_gid: Option<u32>,
    pub(crate) process_pid: Option<i32>,
    pub(crate) start_seconds: Option<u64>,
    pub(crate) start_microseconds: Option<u32>,
}

pub(crate) trait ServerPeerIdentityProbe {
    fn snapshot(&self, stream: &UnixStream) -> Result<ServerPeerSnapshot, HelperClientError>;
}

struct SystemServerPeerIdentityProbe;

#[cfg(target_os = "macos")]
impl ServerPeerIdentityProbe for SystemServerPeerIdentityProbe {
    fn snapshot(&self, stream: &UnixStream) -> Result<ServerPeerSnapshot, HelperClientError> {
        let socket = stream.as_raw_fd();
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: uid/gid are fixed-size outputs and socket is a connected
        // AF_UNIX stream descriptor.
        if unsafe { libc::getpeereid(socket, &mut uid, &mut gid) } != 0 {
            return Err(HelperClientError::PeerRejected);
        }

        let mut pid: libc::pid_t = 0;
        let mut pid_size = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: pid and pid_size describe one pid_t output for LOCAL_PEERPID.
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
            return Err(HelperClientError::PeerRejected);
        }

        // SAFETY: proc_bsdinfo is plain data and proc_pidinfo receives its
        // exact buffer size for the fixed peer pid.
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let expected = std::mem::size_of::<libc::proc_bsdinfo>();
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                i32::try_from(expected).map_err(|_| HelperClientError::PeerRejected)?,
            )
        };
        if usize::try_from(received).ok() != Some(expected) {
            return Err(HelperClientError::PeerRejected);
        }
        const PROCESS_STATUS_ZOMBIE: u32 = 5;
        if info.pbi_status == PROCESS_STATUS_ZOMBIE {
            return Err(HelperClientError::PeerRejected);
        }
        Ok(ServerPeerSnapshot {
            socket_uid: Some(uid),
            socket_gid: Some(gid),
            socket_pid: Some(pid),
            process_uid: Some(info.pbi_uid),
            process_gid: Some(info.pbi_gid),
            process_pid: i32::try_from(info.pbi_pid).ok(),
            start_seconds: Some(info.pbi_start_tvsec),
            start_microseconds: u32::try_from(info.pbi_start_tvusec).ok(),
        })
    }
}

#[cfg(not(target_os = "macos"))]
impl ServerPeerIdentityProbe for SystemServerPeerIdentityProbe {
    fn snapshot(&self, _stream: &UnixStream) -> Result<ServerPeerSnapshot, HelperClientError> {
        Err(HelperClientError::PeerRejected)
    }
}

fn validate_server_peer(
    first: ServerPeerSnapshot,
    second: ServerPeerSnapshot,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<(), HelperClientError> {
    if first != second {
        return Err(HelperClientError::PeerRejected);
    }
    let socket_uid = first.socket_uid.ok_or(HelperClientError::PeerRejected)?;
    let socket_gid = first.socket_gid.ok_or(HelperClientError::PeerRejected)?;
    let socket_pid = first.socket_pid.ok_or(HelperClientError::PeerRejected)?;
    let process_uid = first.process_uid.ok_or(HelperClientError::PeerRejected)?;
    let process_gid = first.process_gid.ok_or(HelperClientError::PeerRejected)?;
    let process_pid = first.process_pid.ok_or(HelperClientError::PeerRejected)?;
    let start_seconds = first.start_seconds.ok_or(HelperClientError::PeerRejected)?;
    let start_microseconds = first
        .start_microseconds
        .ok_or(HelperClientError::PeerRejected)?;
    if expected_uid == 0
        || expected_gid == 0
        || socket_uid != expected_uid
        || socket_gid != expected_gid
        || socket_uid != process_uid
        || socket_gid != process_gid
        || socket_pid <= 0
        || socket_pid != process_pid
        || start_seconds == 0
        || start_microseconds >= 1_000_000
    {
        Err(HelperClientError::PeerRejected)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

impl From<libc::stat> for FileIdentity {
    fn from(metadata: libc::stat) -> Self {
        Self {
            device: metadata.st_dev,
            inode: metadata.st_ino,
        }
    }
}

struct HeldDevEndpoint {
    root: OwnedFd,
    root_identity: FileIdentity,
    run: OwnedFd,
    run_identity: FileIdentity,
    socket_identity: FileIdentity,
    root_path: PathBuf,
    uid: u32,
    gid: u32,
}

impl HeldDevEndpoint {
    fn open(root_path: &Path) -> Result<Self, HelperClientError> {
        // SAFETY: identity calls only read current process credentials.
        let uid = unsafe { libc::geteuid() };
        // SAFETY: see the identity-only note above.
        let gid = unsafe { libc::getegid() };
        if uid == 0 || gid == 0 {
            return Err(HelperClientError::PeerRejected);
        }
        let root = open_absolute_directory_nofollow(root_path)?;
        let root_metadata = stat_fd(root.as_raw_fd())?;
        validate_private_directory(&root_metadata, uid, gid)?;
        let run = open_directory_component(root.as_raw_fd(), RUN_COMPONENT)?;
        let run_metadata = stat_fd(run.as_raw_fd())?;
        validate_private_directory(&run_metadata, uid, gid)?;
        let socket_metadata = metadata_at(run.as_raw_fd(), SOCKET_COMPONENT)?
            .ok_or(HelperClientError::InvalidFrame)?;
        validate_socket_metadata(&socket_metadata, uid, gid)?;
        let endpoint = Self {
            root,
            root_identity: FileIdentity::from(root_metadata),
            run,
            run_identity: FileIdentity::from(run_metadata),
            socket_identity: FileIdentity::from(socket_metadata),
            root_path: root_path.to_path_buf(),
            uid,
            gid,
        };
        endpoint.revalidate()?;
        Ok(endpoint)
    }

    fn revalidate(&self) -> Result<(), HelperClientError> {
        let held_root = stat_fd(self.root.as_raw_fd())?;
        validate_private_directory(&held_root, self.uid, self.gid)?;
        require_identity(held_root, self.root_identity)?;
        let held_run = stat_fd(self.run.as_raw_fd())?;
        validate_private_directory(&held_run, self.uid, self.gid)?;
        require_identity(held_run, self.run_identity)?;
        let held_socket = metadata_at(self.run.as_raw_fd(), SOCKET_COMPONENT)?
            .ok_or(HelperClientError::InvalidFrame)?;
        validate_socket_metadata(&held_socket, self.uid, self.gid)?;
        require_identity(held_socket, self.socket_identity)?;

        let path_root = open_absolute_directory_nofollow(&self.root_path)?;
        let path_root_metadata = stat_fd(path_root.as_raw_fd())?;
        validate_private_directory(&path_root_metadata, self.uid, self.gid)?;
        require_identity(path_root_metadata, self.root_identity)?;
        let path_run = open_directory_component(path_root.as_raw_fd(), RUN_COMPONENT)?;
        let path_run_metadata = stat_fd(path_run.as_raw_fd())?;
        validate_private_directory(&path_run_metadata, self.uid, self.gid)?;
        require_identity(path_run_metadata, self.run_identity)?;
        let path_socket = metadata_at(path_run.as_raw_fd(), SOCKET_COMPONENT)?
            .ok_or(HelperClientError::InvalidFrame)?;
        validate_socket_metadata(&path_socket, self.uid, self.gid)?;
        require_identity(path_socket, self.socket_identity)
    }
}

pub(super) fn next_request_id() -> Result<RequestId, HelperClientError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HelperClientError::RequestIdUnavailable)?
        .as_millis();
    let milliseconds =
        u64::try_from(milliseconds).map_err(|_| HelperClientError::RequestIdUnavailable)?;
    if milliseconds > 0x0000_ffff_ffff_ffff {
        return Err(HelperClientError::RequestIdUnavailable);
    }

    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| HelperClientError::RandomnessUnavailable)?;
    let timestamp = milliseconds.to_be_bytes();
    bytes[..6].copy_from_slice(&timestamp[2..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    RequestId::parse(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    ))
    .map_err(|_| HelperClientError::RequestIdUnavailable)
}

fn write_frame_until(
    stream: &mut UnixStream,
    body: &[u8],
    deadline: Deadline,
) -> Result<(), HelperClientError> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(HelperClientError::InvalidFrame);
    }
    let length = u32::try_from(body.len()).map_err(|_| HelperClientError::InvalidFrame)?;
    write_all_until(stream, &length.to_be_bytes(), deadline)?;
    write_all_until(stream, body, deadline)
}

fn read_frame_until(
    stream: &mut UnixStream,
    deadline: Deadline,
) -> Result<Vec<u8>, HelperClientError> {
    let mut prefix = [0_u8; 4];
    read_exact_until(stream, &mut prefix, deadline)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(HelperClientError::InvalidFrame);
    }
    let mut body = vec![0_u8; length];
    read_exact_until(stream, &mut body, deadline)?;
    let mut trailing = [0_u8; 1];
    loop {
        deadline.ensure_remaining()?;
        match stream.read(&mut trailing) {
            Ok(0) => {
                deadline.ensure_remaining()?;
                return Ok(body);
            }
            Ok(_) => {
                deadline.ensure_remaining()?;
                return Err(HelperClientError::InvalidFrame);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) => return Err(map_io_error(error)),
        }
    }
}

#[cfg(test)]
fn read_frame_with_timeout_for_testing(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<Vec<u8>, HelperClientError> {
    set_nonblocking(stream.as_raw_fd())?;
    read_frame_until(stream, Deadline::after(timeout))
}

#[cfg(test)]
fn connect_path_with_timeout_for_testing(
    path: &Path,
    timeout: Duration,
) -> Result<UnixStream, HelperClientError> {
    connect_path_until(path, Deadline::after(timeout))
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

    fn ensure_remaining(self) -> Result<(), HelperClientError> {
        if Instant::now() < self.expires_at {
            Ok(())
        } else {
            Err(HelperClientError::Deadline)
        }
    }

    fn poll_timeout_millis(self) -> Result<i32, HelperClientError> {
        let remaining = self
            .expires_at
            .checked_duration_since(Instant::now())
            .ok_or(HelperClientError::Deadline)?;
        if remaining.is_zero() {
            return Err(HelperClientError::Deadline);
        }
        let rounded_up = remaining.as_millis().saturating_add(1);
        Ok(i32::try_from(rounded_up.min(i32::MAX as u128)).unwrap_or(i32::MAX))
    }
}

fn read_exact_until(
    stream: &mut UnixStream,
    mut destination: &mut [u8],
    deadline: Deadline,
) -> Result<(), HelperClientError> {
    while !destination.is_empty() {
        deadline.ensure_remaining()?;
        match stream.read(destination) {
            Ok(0) => return Err(HelperClientError::InvalidFrame),
            Ok(read) => {
                let (_, remaining) = destination.split_at_mut(read);
                destination = remaining;
                deadline.ensure_remaining()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLIN, deadline)?;
            }
            Err(error) => return Err(map_io_error(error)),
        }
    }
    Ok(())
}

fn write_all_until(
    stream: &mut UnixStream,
    mut source: &[u8],
    deadline: Deadline,
) -> Result<(), HelperClientError> {
    while !source.is_empty() {
        deadline.ensure_remaining()?;
        match stream.write(source) {
            Ok(0) => {
                return Err(HelperClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "development helper socket wrote zero bytes",
                )))
            }
            Ok(written) => {
                source = &source[written..];
                deadline.ensure_remaining()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLOUT, deadline)?;
            }
            Err(error) => return Err(map_io_error(error)),
        }
    }
    Ok(())
}

fn wait_fd(file: RawFd, events: i16, deadline: Deadline) -> Result<(), HelperClientError> {
    loop {
        let mut descriptor = libc::pollfd {
            fd: file,
            events,
            revents: 0,
        };
        let timeout = deadline.poll_timeout_millis()?;
        // SAFETY: descriptor points to one initialized pollfd for the call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(HelperClientError::Io(std::io::Error::from_raw_os_error(
                    libc::EBADF,
                )));
            }
            if descriptor.revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            continue;
        }
        if result == 0 {
            return Err(HelperClientError::Deadline);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(map_io_error(error));
        }
    }
}

fn set_nonblocking(file: RawFd) -> Result<(), HelperClientError> {
    // SAFETY: F_GETFL/F_SETFL operate only on flags for the live descriptor.
    let flags = unsafe { libc::fcntl(file, libc::F_GETFL) };
    if flags < 0 {
        return Err(map_io_error(std::io::Error::last_os_error()));
    }
    if flags & libc::O_NONBLOCK == 0 {
        // SAFETY: existing flags are preserved and only O_NONBLOCK is added.
        if unsafe { libc::fcntl(file, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(map_io_error(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn set_close_on_exec(file: RawFd) -> Result<(), HelperClientError> {
    // SAFETY: F_GETFD/F_SETFD operate only on descriptor-local flags.
    let flags = unsafe { libc::fcntl(file, libc::F_GETFD) };
    if flags < 0 {
        return Err(map_io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: existing flags are preserved and only FD_CLOEXEC is added.
    if unsafe { libc::fcntl(file, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        Err(map_io_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn connect_path_until(path: &Path, deadline: Deadline) -> Result<UnixStream, HelperClientError> {
    let path = path.as_os_str().as_bytes();
    if path.is_empty() || path.contains(&0) {
        return Err(HelperClientError::InvalidFrame);
    }
    // SAFETY: socket has no pointer arguments and returns a fresh descriptor.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(map_io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: raw is a fresh owned descriptor.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    set_close_on_exec(socket.as_raw_fd())?;
    set_nonblocking(socket.as_raw_fd())?;

    // SAFETY: sockaddr_un is plain data and zero is valid initialization.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if path.len() >= address.sun_path.len() {
        return Err(HelperClientError::InvalidFrame);
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
        .ok_or(HelperClientError::InvalidFrame)?;
    let address_length =
        libc::socklen_t::try_from(address_length).map_err(|_| HelperClientError::InvalidFrame)?;
    #[cfg(target_os = "macos")]
    {
        address.sun_len =
            u8::try_from(address_length).map_err(|_| HelperClientError::InvalidFrame)?;
    }

    deadline.ensure_remaining()?;
    // SAFETY: address and its exact initialized length remain live for connect.
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address_length,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS)
            | Some(libc::EALREADY)
            | Some(libc::EWOULDBLOCK)
            | Some(libc::EINTR) => {
                wait_fd(socket.as_raw_fd(), libc::POLLOUT, deadline)?;
                let mut pending_error: libc::c_int = 0;
                let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                // SAFETY: pending_error and size describe one SO_ERROR output.
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
                {
                    return Err(map_io_error(std::io::Error::last_os_error()));
                }
                if pending_error != 0 {
                    return Err(map_io_error(std::io::Error::from_raw_os_error(
                        pending_error,
                    )));
                }
            }
            Some(libc::EISCONN) => {}
            _ => return Err(map_io_error(error)),
        }
    }
    deadline.ensure_remaining()?;
    // SAFETY: ownership moves from OwnedFd to UnixStream exactly once.
    Ok(unsafe { UnixStream::from_raw_fd(socket.into_raw_fd()) })
}

fn open_absolute_directory_nofollow(path: &Path) -> Result<OwnedFd, HelperClientError> {
    if !path.is_absolute() {
        return Err(HelperClientError::InvalidFrame);
    }
    let root = CString::new("/").expect("fixed root path");
    let mut directory = open_directory_path(&root)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                let component = CString::new(component.as_bytes())
                    .map_err(|_| HelperClientError::InvalidFrame)?;
                directory = open_directory_component(directory.as_raw_fd(), &component)?;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(HelperClientError::InvalidFrame)
            }
        }
    }
    Ok(directory)
}

fn directory_open_flags() -> libc::c_int {
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
}

fn open_directory_path(path: &CStr) -> Result<OwnedFd, HelperClientError> {
    // SAFETY: path is NUL-terminated and flags create no file.
    let raw = unsafe { libc::open(path.as_ptr(), directory_open_flags()) };
    owned_fd(raw)
}

fn open_directory_component(parent: RawFd, component: &CStr) -> Result<OwnedFd, HelperClientError> {
    // SAFETY: component is one NUL-terminated name and O_NOFOLLOW rejects links.
    let raw = unsafe {
        libc::openat(
            parent,
            component.as_ptr(),
            directory_open_flags(),
            0 as libc::c_uint,
        )
    };
    owned_fd(raw)
}

fn owned_fd(raw: RawFd) -> Result<OwnedFd, HelperClientError> {
    if raw < 0 {
        Err(HelperClientError::InvalidFrame)
    } else {
        // SAFETY: raw was just returned as a fresh descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

fn stat_fd(file: RawFd) -> Result<libc::stat, HelperClientError> {
    // SAFETY: metadata is a valid output and file is held by the caller.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(file, &mut metadata) } == 0 {
        Ok(metadata)
    } else {
        Err(HelperClientError::InvalidFrame)
    }
}

fn metadata_at(
    directory: RawFd,
    component: &CStr,
) -> Result<Option<libc::stat>, HelperClientError> {
    // SAFETY: metadata is valid output and component is resolved without links
    // beneath a held directory descriptor.
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
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(HelperClientError::InvalidFrame)
    }
}

fn validate_private_directory(
    metadata: &libc::stat,
    uid: u32,
    gid: u32,
) -> Result<(), HelperClientError> {
    if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
        || u32::from(metadata.st_mode) & 0o7777 != DIRECTORY_MODE
        || metadata.st_uid != uid
        || metadata.st_gid != gid
    {
        Err(HelperClientError::InvalidFrame)
    } else {
        Ok(())
    }
}

fn validate_socket_metadata(
    metadata: &libc::stat,
    uid: u32,
    gid: u32,
) -> Result<(), HelperClientError> {
    if metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || u32::from(metadata.st_mode) & 0o7777 != SOCKET_MODE
        || metadata.st_uid != uid
        || metadata.st_gid != gid
        || metadata.st_nlink != 1
    {
        Err(HelperClientError::InvalidFrame)
    } else {
        Ok(())
    }
}

fn require_identity(metadata: libc::stat, identity: FileIdentity) -> Result<(), HelperClientError> {
    if FileIdentity::from(metadata) == identity {
        Ok(())
    } else {
        Err(HelperClientError::InvalidFrame)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, Instant};

    use jarvis_power_core::protocol::{
        decode_request, encode_response, Request, Response, ResponseEnvelope, PROTOCOL_VERSION,
    };

    use super::{
        connect_path_with_timeout_for_testing, next_request_id,
        read_frame_with_timeout_for_testing, ClientConnectStage, DevUdsClient,
        ServerPeerIdentityProbe, ServerPeerSnapshot, SOCKET_NAME,
    };
    use crate::power::helper::client::{HelperClient, HelperClientError, HelperTrust};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestTempDirectory(PathBuf);

    impl TestTempDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let base = fs::canonicalize(Path::new("/tmp")).unwrap();
            let path = base.join(format!("jpc-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: path_bytes names the directory just created by this
            // process; uid_t::MAX preserves its current owner.
            assert_eq!(
                unsafe {
                    libc::chown(
                        path_bytes.as_ptr(),
                        libc::uid_t::MAX,
                        current_gid() as libc::gid_t,
                    )
                },
                0
            );
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestTempDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn request_ids_are_locally_generated_canonical_uuid_v7_values() {
        for _ in 0..32 {
            let request_id = next_request_id().unwrap();
            assert_eq!(request_id.as_str().as_bytes()[14], b'7');
            assert!(matches!(request_id.as_str().as_bytes()[19], b'8'..=b'b'));
        }
    }

    fn current_uid() -> u32 {
        // SAFETY: reads only the current process identity.
        unsafe { libc::geteuid() }
    }

    fn current_gid() -> u32 {
        // SAFETY: reads only the current process identity.
        unsafe { libc::getegid() }
    }

    fn stable_server_peer() -> ServerPeerSnapshot {
        ServerPeerSnapshot {
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

    struct ScriptedServerPeer {
        snapshots: Mutex<VecDeque<ServerPeerSnapshot>>,
    }

    impl ScriptedServerPeer {
        fn stable() -> Self {
            let snapshot = stable_server_peer();
            Self {
                snapshots: Mutex::new(VecDeque::from([snapshot, snapshot])),
            }
        }
    }

    impl ServerPeerIdentityProbe for ScriptedServerPeer {
        fn snapshot(
            &self,
            _stream: &std::os::unix::net::UnixStream,
        ) -> Result<ServerPeerSnapshot, HelperClientError> {
            self.snapshots
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(HelperClientError::PeerRejected)
        }
    }

    #[test]
    fn client_rejects_nonexact_parent_socket_modes_and_hardlinks() {
        let temp = TestTempDirectory::new();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run.join(SOCKET_NAME);

        let special_mode = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o4600)).unwrap();
        assert!(matches!(
            DevUdsClient::new(temp.path())
                .connect_with_peer_for_testing(&ScriptedServerPeer::stable()),
            Err(HelperClientError::InvalidFrame)
        ));
        assert_eq!(
            fs::symlink_metadata(&socket).unwrap().mode() & 0o7777,
            0o4600
        );
        drop(special_mode);
        fs::remove_file(&socket).unwrap();

        let linked = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let sibling = run.join("hardlink");
        fs::hard_link(&socket, &sibling).unwrap();
        assert_eq!(fs::symlink_metadata(&socket).unwrap().nlink(), 2);
        assert!(matches!(
            DevUdsClient::new(temp.path())
                .connect_with_peer_for_testing(&ScriptedServerPeer::stable()),
            Err(HelperClientError::InvalidFrame)
        ));
        drop(linked);
        fs::remove_file(&sibling).unwrap();
        fs::remove_file(&socket).unwrap();

        fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
        let permissive_parent = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            DevUdsClient::new(temp.path())
                .connect_with_peer_for_testing(&ScriptedServerPeer::stable()),
            Err(HelperClientError::InvalidFrame)
        ));
        drop(permissive_parent);
    }

    #[test]
    fn client_rejects_changed_or_inconsistent_connected_server_identity() {
        let temp = TestTempDirectory::new();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run.join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

        let first = stable_server_peer();
        let mut second = first;
        second.start_microseconds = Some(8);
        let peer = ScriptedServerPeer {
            snapshots: Mutex::new(VecDeque::from([first, second])),
        };
        let result = DevUdsClient::new(temp.path()).connect_with_peer_for_testing(&peer);
        assert!(
            matches!(result, Err(HelperClientError::PeerRejected)),
            "unexpected connect result: {result:?}"
        );
        drop(listener);
    }

    #[test]
    fn client_rejects_root_replacement_after_connect_before_public_use() {
        let temp = TestTempDirectory::new();
        let original = temp.path().join("jarvis");
        fs::create_dir(&original).unwrap();
        fs::set_permissions(&original, fs::Permissions::from_mode(0o700)).unwrap();
        let run = original.join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run.join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let moved = temp.path().join("moved");

        let result = DevUdsClient::new(&original).connect_with_hook_for_testing(
            &ScriptedServerPeer::stable(),
            |stage| {
                assert_eq!(stage, ClientConnectStage::AfterConnectBeforeRevalidation);
                fs::rename(&original, &moved).unwrap();
                fs::create_dir(&original).unwrap();
                fs::set_permissions(&original, fs::Permissions::from_mode(0o700)).unwrap();
                let replacement_run = original.join("run");
                fs::create_dir(&replacement_run).unwrap();
                fs::set_permissions(&replacement_run, fs::Permissions::from_mode(0o700)).unwrap();
                let replacement = UnixListener::bind(replacement_run.join(SOCKET_NAME)).unwrap();
                fs::set_permissions(
                    replacement_run.join(SOCKET_NAME),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                drop(replacement);
            },
        );
        assert!(matches!(result, Err(HelperClientError::InvalidFrame)));
        assert!(moved.exists(), "connect hook was not reached");
        drop(listener);
    }

    #[test]
    fn nonblocking_connect_to_a_full_backlog_never_exceeds_its_deadline() {
        let temp = TestTempDirectory::new();
        let socket = temp.path().join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket).unwrap();
        // SAFETY: listener is a live AF_UNIX listener and lowering backlog
        // does not transfer ownership of its descriptor.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let mut blockers = Vec::new();
        let mut observed_full_backlog = false;
        for _ in 0..256 {
            match connect_path_with_timeout_for_testing(&socket, Duration::from_millis(20)) {
                Ok(stream) => blockers.push(stream),
                Err(HelperClientError::Deadline) => {
                    observed_full_backlog = true;
                    break;
                }
                Err(HelperClientError::Io(error))
                    if error.raw_os_error() == Some(libc::ECONNREFUSED) =>
                {
                    observed_full_backlog = true;
                    break;
                }
                Err(error) => panic!("unexpected backlog setup error: {error:?}"),
            }
        }
        assert!(observed_full_backlog, "listen backlog did not fill");

        let started = Instant::now();
        let result = connect_path_with_timeout_for_testing(&socket, Duration::from_millis(125));
        let safely_bounded = matches!(result, Err(HelperClientError::Deadline))
            || matches!(
                result,
                Err(HelperClientError::Io(ref error))
                    if error.raw_os_error() == Some(libc::ECONNREFUSED)
            );
        assert!(safely_bounded, "unexpected connect result: {result:?}");
        assert!(started.elapsed() < Duration::from_millis(700));
        drop((blockers, listener));
    }

    #[test]
    fn response_slow_drip_is_bounded_by_one_absolute_deadline() {
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut bytes = Vec::from(8_u32.to_be_bytes());
        bytes.extend_from_slice(b"12345678");
        let writer = thread::spawn(move || {
            for byte in bytes {
                if writer.write_all(&[byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(80));
            }
            let _ = writer.shutdown(std::net::Shutdown::Write);
        });

        let started = Instant::now();
        assert!(matches!(
            read_frame_with_timeout_for_testing(&mut reader, Duration::from_millis(250)),
            Err(HelperClientError::Deadline)
        ));
        assert!(started.elapsed() < Duration::from_millis(700));
        writer.join().unwrap();
    }

    #[test]
    fn client_uses_one_bounded_frame_and_marks_reply_development_only() {
        let temp = TestTempDirectory::new();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run.join(SOCKET_NAME);
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut prefix = [0_u8; 4];
            stream.read_exact(&mut prefix).unwrap();
            let length = u32::from_be_bytes(prefix) as usize;
            let mut body = vec![0_u8; length];
            stream.read_exact(&mut body).unwrap();
            let mut eof = [0_u8; 1];
            assert_eq!(stream.read(&mut eof).unwrap(), 0);
            let request = decode_request(body).unwrap();
            assert_eq!(request.request, Request::Status);
            let response = encode_response(&ResponseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                response: Response::Status {
                    active_leases: 0,
                    mutation_active: false,
                    recovery_required: false,
                },
            })
            .unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });

        let reply = DevUdsClient::new(temp.path())
            .send(Request::Status)
            .unwrap();
        assert_eq!(reply.trust, HelperTrust::DevelopmentOnly);
        assert!(matches!(reply.response.response, Response::Status { .. }));
        server.join().unwrap();
    }

    #[test]
    fn app_helper_never_writes_or_names_the_helper_state_file() {
        let client = include_str!("client.rs");
        let transport = include_str!("dev_uds.rs");
        let state_file = ["dev-helper-v2", ".json"].concat();
        assert!(!client.contains(&state_file));
        assert!(!transport.contains(&state_file));
    }
}
