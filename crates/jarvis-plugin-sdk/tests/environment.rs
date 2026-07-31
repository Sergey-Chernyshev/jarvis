use jarvis_plugin_protocol::process::{PluginFrame, PLUGIN_PROCESS_PROTOCOL};
use jarvis_plugin_sdk::{PluginClient, PluginEnvironment, Transport};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn vars() -> [(&'static str, &'static str); 6] {
    [
        ("JARVIS_PLUGIN_ID", "dev.example.echo"),
        ("JARVIS_PLUGIN_TOKEN", "super-secret-token"),
        ("JARVIS_PLUGIN_PROTOCOL", "2"),
        ("JARVIS_PLUGIN_PACKAGE_DIGEST", DIGEST),
        ("JARVIS_PLUGIN_ACTIVATION_GENERATION", "9"),
        ("JARVIS_SOCKET", "/tmp/jarvis.sock"),
    ]
}

#[test]
fn environment_rejects_cross_plugin_identity() {
    let env = PluginEnvironment::from_pairs(vars()).unwrap();
    assert_eq!(env.plugin_id, "dev.example.echo");
    assert_eq!(env.activation_generation, 9);
    assert!(env.assert_hello_identity("other", 9).is_err());
}

#[test]
fn environment_requires_all_six_values_and_protocol_v2() {
    let missing = PluginEnvironment::from_pairs(vars().into_iter().take(5)).unwrap_err();
    assert_eq!(missing.code(), "plugin_environment_missing");
    assert!(missing.to_string().contains("JARVIS_SOCKET"));

    let mut incompatible = vars();
    incompatible[2].1 = "1";
    let error = PluginEnvironment::from_pairs(incompatible).unwrap_err();
    assert_eq!(error.code(), "plugin_protocol_incompatible");
    assert!(error
        .to_string()
        .contains(&PLUGIN_PROCESS_PROTOCOL.to_string()));
}

#[test]
fn environment_debug_and_errors_never_expose_token_value() {
    let env = PluginEnvironment::from_pairs(vars()).unwrap();
    assert!(!format!("{env:?}").contains("super-secret-token"));

    let error = env.assert_hello_identity("wrong", 9).unwrap_err();
    assert!(!error.to_string().contains("super-secret-token"));
}

#[derive(Default)]
struct RecordingTransport {
    sent: Vec<PluginFrame>,
}

impl Transport for RecordingTransport {
    fn send(&mut self, frame: &PluginFrame) -> Result<(), String> {
        self.sent.push(frame.clone());
        Ok(())
    }

    fn receive(&mut self) -> Result<PluginFrame, String> {
        Err("no fixture response".into())
    }
}

#[test]
fn client_sends_hello_from_validated_environment() {
    let env = PluginEnvironment::from_pairs(vars()).unwrap();
    let mut client = PluginClient::new(env, RecordingTransport::default());
    client.send_hello(42).unwrap();
    let transport = client.into_transport();

    let PluginFrame::PluginHello(hello) = &transport.sent[0] else {
        panic!("expected plugin hello");
    };
    assert_eq!(hello.protocol_version, PLUGIN_PROCESS_PROTOCOL);
    assert_eq!(hello.plugin_id, "dev.example.echo");
    assert_eq!(hello.package_digest, DIGEST);
    assert_eq!(hello.activation_generation, 9);
    assert_eq!(hello.pid, 42);
}
