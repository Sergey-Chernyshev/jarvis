#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

fn temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "jarvis-agent-vm-live-plugin-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}

fn read_request(stream: &UnixStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            content_length = value;
        }
        headers.push_str(&line);
    }
    let mut bytes = vec![0; content_length];
    reader.read_exact(&mut bytes).unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (headers, body)
}

fn respond(mut stream: UnixStream, body: Value) {
    let body = serde_json::to_vec(&body).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

/// Local-only real-process smoke. It uses installed `avm`/`limactl`, but an
/// empty synthetic HOME/LIMA_HOME derived by the plugin from this temp profile.
#[test]
#[ignore = "requires locally installed avm and limactl"]
fn real_sidecar_handshakes_polls_and_publishes_inventory_operation() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let socket = root.join("run.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token = "a".repeat(64);
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();

    let server = std::thread::spawn(move || {
        let mut publications = Vec::new();
        for step in 0..4 {
            let (stream, _) = listener.accept().unwrap();
            let (headers, body) = read_request(&stream);
            assert!(headers.contains("x-jarvis-token: "));
            match step {
                0 => {
                    assert!(headers.starts_with("POST /plugin/register HTTP/1.1"));
                    assert_eq!(body["protocolVersion"], 1);
                    respond(stream, json!({"ok": true}));
                }
                1 => {
                    assert!(headers.starts_with("GET /plugin/events?"));
                    respond(
                        stream,
                        json!({
                            "ok": true,
                            "events": [{
                                "seq": 1,
                                "kind": "command",
                                "payload": {
                                    "requestId": "agent-vm-1",
                                    "name": "runtime.inventory",
                                    "args": {}
                                }
                            }],
                            "nextSeq": 1
                        }),
                    );
                }
                _ => {
                    assert!(headers.starts_with("POST /capability HTTP/1.1"));
                    publications.push(body);
                    respond(stream, json!({"ok": true, "value": {}}));
                }
            }
        }
        published_tx.send(publications).unwrap();
        let _ = stop_rx.recv();
    });

    let binary = env!("CARGO_BIN_EXE_jarvis-agent-vm-plugin");
    let mut child = Command::new(binary)
        .env_clear()
        .env("JARVIS_SOCKET", &socket)
        .env("JARVIS_PLUGIN_ID", "agent-vm")
        .env("JARVIS_PLUGIN_TOKEN", token)
        .env("JARVIS_PLUGIN_PROTOCOL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let publications = published_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let _ = stop_tx.send(());
    server.join().unwrap();

    assert_eq!(publications.len(), 2);
    assert_eq!(publications[0]["id"], "entities.publish");
    assert_eq!(publications[0]["args"]["kind"], "operation");
    assert_eq!(publications[0]["args"]["state"], "started");
    assert_eq!(publications[1]["args"]["state"], "done");
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "healthy sidecar stays silent"
    );
    fs::remove_dir_all(root).unwrap();
}
