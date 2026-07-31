#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::thread;

    use jarvis_power_core::protocol::{
        decode_request, encode_response, Request, Response, ResponseEnvelope, PROTOCOL_VERSION,
    };

    use super::{next_request_id, DevUdsClient};
    use crate::power::helper::client::{HelperClient, HelperTrust};

    #[test]
    fn request_ids_are_locally_generated_canonical_uuid_v7_values() {
        for _ in 0..32 {
            let request_id = next_request_id().unwrap();
            assert_eq!(request_id.as_str().as_bytes()[14], b'7');
            assert!(matches!(request_id.as_str().as_bytes()[19], b'8'..=b'b'));
        }
    }

    #[test]
    fn client_uses_one_bounded_frame_and_marks_reply_development_only() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run.join("power-helper-dev.sock");
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
        assert!(!client.contains("dev-helper-v2.json"));
        assert!(!transport.contains("dev-helper-v2.json"));
    }
}
