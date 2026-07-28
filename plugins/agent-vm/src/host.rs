use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_HTTP_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HTTP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub token: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait Transport: Clone + Send + Sync + 'static {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String>;
}

#[derive(Clone, Debug)]
pub struct UnixSocketTransport {
    socket: PathBuf,
}

impl UnixSocketTransport {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
}

impl Transport for UnixSocketTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let bytes = encode_http_request(&request)?;
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|_| "Jarvis plugin socket недоступен".to_string())?;
        let timeout = Some(Duration::from_secs(35));
        stream
            .set_read_timeout(timeout)
            .map_err(|_| "не задать plugin socket timeout".to_string())?;
        stream
            .set_write_timeout(timeout)
            .map_err(|_| "не задать plugin socket timeout".to_string())?;
        stream
            .write_all(&bytes)
            .map_err(|_| "не отправить plugin socket request".to_string())?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| "не завершить plugin socket request".to_string())?;
        let mut response = Vec::new();
        stream
            .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut response)
            .map_err(|_| "не прочитать plugin socket response".to_string())?;
        parse_http_response(&response)
    }
}

pub fn encode_http_request(request: &HttpRequest) -> Result<Vec<u8>, String> {
    if !matches!(request.method.as_str(), "GET" | "POST") {
        return Err("неподдерживаемый HTTP method".into());
    }
    if !request.path.starts_with('/')
        || request.path.contains('\r')
        || request.path.contains('\n')
        || request.token.is_empty()
        || request.token.contains('\r')
        || request.token.contains('\n')
    {
        return Err("некорректный plugin HTTP request".into());
    }
    if request.body.len() > MAX_HTTP_REQUEST_BYTES {
        return Err("plugin HTTP request превышает limit".into());
    }
    let headers = format!(
        "{} {} HTTP/1.1\r\nhost: localhost\r\nx-jarvis-token: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        request.method,
        request.path,
        request.token,
        request.body.len()
    );
    let mut bytes = Vec::with_capacity(headers.len() + request.body.len());
    bytes.extend_from_slice(headers.as_bytes());
    bytes.extend_from_slice(&request.body);
    Ok(bytes)
}

pub fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, String> {
    if bytes.len() > MAX_HTTP_RESPONSE_BYTES {
        return Err("plugin HTTP response превышает limit".into());
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "некорректный plugin HTTP response".to_string())?;
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| "некорректный plugin HTTP header".to_string())?;
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "некорректный plugin HTTP status".to_string())?;
    Ok(HttpResponse {
        status,
        body: bytes[header_end + 4..].to_vec(),
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandPayload {
    pub request_id: String,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HostEvent {
    pub seq: u64,
    pub kind: String,
    pub payload: CommandPayload,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollResponse {
    #[allow(dead_code)]
    pub ok: bool,
    #[serde(default)]
    pub events: Vec<HostEvent>,
    pub next_seq: u64,
}

pub trait HostApi: Clone + Send + Sync + 'static {
    fn register(&self, pid: u32) -> Result<(), String>;
    fn poll(&self, after: u64) -> Result<PollResponse, String>;
    fn publish_entity(
        &self,
        op: &str,
        kind: &str,
        object_id: &str,
        state: &str,
        attrs: Value,
    ) -> Result<(), String>;
}

#[derive(Clone)]
pub struct HostClient<T: Transport> {
    transport: T,
    token: String,
    protocol_version: u32,
}

impl<T: Transport> HostClient<T> {
    pub fn new(transport: T, token: String, protocol_version: u32) -> Self {
        Self {
            transport,
            token,
            protocol_version,
        }
    }

    fn json_request(&self, method: &str, path: &str, body: Value) -> Result<Value, String> {
        let response = self.transport.send(HttpRequest {
            method: method.into(),
            path: path.into(),
            token: self.token.clone(),
            body: if method == "GET" {
                Vec::new()
            } else {
                serde_json::to_vec(&body)
                    .map_err(|_| "не сериализовать plugin request".to_string())?
            },
        })?;
        if !(200..300).contains(&response.status) {
            return Err(format!(
                "Jarvis plugin request отклонён (HTTP {})",
                response.status
            ));
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| "Jarvis вернул некорректный plugin JSON".to_string())
    }
}

impl<T: Transport> HostApi for HostClient<T> {
    fn register(&self, pid: u32) -> Result<(), String> {
        let value = self.json_request(
            "POST",
            "/plugin/register",
            json!({"protocolVersion": self.protocol_version, "pid": pid}),
        )?;
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err("Jarvis отклонил plugin registration".into())
        }
    }

    fn poll(&self, after: u64) -> Result<PollResponse, String> {
        let value = self.json_request(
            "GET",
            &format!("/plugin/events?after={after}&limit=64&waitMs=25000"),
            Value::Null,
        )?;
        serde_json::from_value(value)
            .map_err(|_| "Jarvis вернул некорректный event batch".to_string())
    }

    fn publish_entity(
        &self,
        op: &str,
        kind: &str,
        object_id: &str,
        state: &str,
        attrs: Value,
    ) -> Result<(), String> {
        if !matches!(op, "upsert" | "remove") {
            return Err("некорректный entity operation".into());
        }
        let value = self.json_request(
            "POST",
            "/capability",
            json!({
                "id": "entities.publish",
                "args": {
                    "op": op,
                    "kind": kind,
                    "id": object_id,
                    "state": state,
                    "attrs": attrs
                }
            }),
        )?;
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err("Jarvis отклонил entity publication".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[derive(Clone, Default)]
    struct MockTransport {
        calls: Arc<Mutex<Vec<HttpRequest>>>,
        replies: Arc<Mutex<VecDeque<HttpResponse>>>,
    }

    impl MockTransport {
        fn with_replies(replies: Vec<HttpResponse>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                replies: Arc::new(Mutex::new(replies.into())),
            }
        }
    }

    impl Transport for MockTransport {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.calls.lock().unwrap().push(request);
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "mock response exhausted".into())
        }
    }

    fn response(status: u16, body: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status,
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    #[test]
    fn http_request_puts_token_in_uds_headers_not_process_argv() {
        let bytes = encode_http_request(&HttpRequest {
            method: "POST".into(),
            path: "/plugin/register".into(),
            token: "synthetic-token".into(),
            body: br#"{"protocolVersion":1,"pid":42}"#.to_vec(),
        })
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.starts_with("POST /plugin/register HTTP/1.1\r\n"));
        assert!(text.contains("\r\nx-jarvis-token: synthetic-token\r\n"));
        assert!(text.contains("\r\nconnection: close\r\n"));
        assert!(text.ends_with(r#"{"protocolVersion":1,"pid":42}"#));
    }

    #[test]
    fn parser_rejects_non_success_and_oversized_responses() {
        let denied = b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\n\r\n{}";
        assert!(parse_http_response(denied).unwrap().status == 401);

        let huge = vec![b'x'; MAX_HTTP_RESPONSE_BYTES + 1];
        let err = parse_http_response(&huge).unwrap_err();
        assert!(err.contains("limit"));
    }

    #[test]
    fn host_client_uses_versioned_register_poll_and_capability_wires() {
        let transport = MockTransport::with_replies(vec![
            response(200, json!({"ok": true})),
            response(
                200,
                json!({
                    "ok": true,
                    "events": [{
                        "seq": 4,
                        "kind": "command",
                        "payload": {
                            "requestId": "agent-vm-4",
                            "name": "runtime.status",
                            "args": {"cwd": "/synthetic/project"}
                        }
                    }],
                    "nextSeq": 4
                }),
            ),
            response(
                200,
                json!({"ok": true, "value": {"entity": {"id": "vm.synthetic"}}}),
            ),
        ]);
        let host = HostClient::new(transport.clone(), "synthetic-token".into(), 1);

        host.register(42).unwrap();
        let events = host.poll(0).unwrap();
        host.publish_entity(
            "upsert",
            "vm",
            "synthetic",
            "running",
            json!({"management": "managed"}),
        )
        .unwrap();

        assert_eq!(events.next_seq, 4);
        assert_eq!(events.events[0].payload.name, "runtime.status");
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls[0].path, "/plugin/register");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&calls[0].body).unwrap(),
            json!({"protocolVersion": 1, "pid": 42})
        );
        assert_eq!(
            calls[1].path,
            "/plugin/events?after=0&limit=64&waitMs=25000"
        );
        assert_eq!(calls[2].path, "/capability");
        let publish: serde_json::Value = serde_json::from_slice(&calls[2].body).unwrap();
        assert_eq!(publish["id"], "entities.publish");
        assert_eq!(publish["args"]["kind"], "vm");
        assert_eq!(publish["args"]["id"], "synthetic");
    }
}
