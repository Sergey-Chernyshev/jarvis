use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jarvis_power_core::protocol::{
    decode_response, encode_request, Request, RequestEnvelope, RequestId, MAX_FRAME_BYTES,
    PROTOCOL_VERSION,
};

use super::client::{map_io_error, HelperClient, HelperClientError, HelperReply, HelperTrust};

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const SOCKET_NAME: &str = "power-helper-dev.sock";

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

    fn connect(&self) -> Result<UnixStream, HelperClientError> {
        let socket_path = self.socket_path();
        let metadata = fs::symlink_metadata(&socket_path).map_err(map_io_error)?;
        // SAFETY: `geteuid` has no preconditions and only reads process credentials.
        let effective_uid = unsafe { libc::geteuid() };
        if !metadata.file_type().is_socket()
            || metadata.uid() != effective_uid
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(HelperClientError::InvalidFrame);
        }
        let stream = UnixStream::connect(socket_path).map_err(map_io_error)?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(map_io_error)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(map_io_error)?;
        Ok(stream)
    }
}

impl HelperClient for DevUdsClient {
    fn send(&self, request: Request) -> Result<HelperReply, HelperClientError> {
        let request_id = next_request_id()?;
        let envelope = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            request,
        };
        let body = encode_request(&envelope).map_err(HelperClientError::Protocol)?;
        let mut stream = self.connect()?;
        write_frame(&mut stream, &body)?;
        stream.shutdown(Shutdown::Write).map_err(map_io_error)?;
        let body = read_frame(&mut stream)?;
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

fn write_frame(stream: &mut UnixStream, body: &[u8]) -> Result<(), HelperClientError> {
    if body.is_empty() || body.len() > MAX_FRAME_BYTES {
        return Err(HelperClientError::InvalidFrame);
    }
    let length = u32::try_from(body.len()).map_err(|_| HelperClientError::InvalidFrame)?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(map_io_error)?;
    stream.write_all(body).map_err(map_io_error)
}

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, HelperClientError> {
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix).map_err(map_io_error)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(HelperClientError::InvalidFrame);
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).map_err(map_io_error)?;
    let mut trailing = [0_u8; 1];
    match stream.read(&mut trailing) {
        Ok(0) => Ok(body),
        Ok(_) => Err(HelperClientError::InvalidFrame),
        Err(error) => Err(map_io_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{Read, Write};
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
        next_request_id, read_frame_with_timeout_for_testing, DevUdsClient,
        ServerPeerIdentityProbe, ServerPeerSnapshot, SOCKET_NAME,
    };
    use crate::power::helper::client::{HelperClient, HelperClientError, HelperTrust};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestTempDirectory(PathBuf);

    impl TestTempDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let base = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = base.join(format!(
                "jarvis-power-client-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
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
        assert!(matches!(
            DevUdsClient::new(temp.path()).connect_with_peer_for_testing(&peer),
            Err(HelperClientError::PeerRejected)
        ));
        drop(listener);
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
