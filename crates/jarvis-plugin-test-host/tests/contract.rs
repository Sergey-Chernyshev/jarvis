use jarvis_plugin_protocol::process::{Heartbeat, PluginFrame};
use jarvis_plugin_test_host::{TestHost, MAX_QUEUED_COMMANDS};
use serde_json::json;

#[test]
fn test_host_rejects_stale_generation_and_replays_nothing() {
    let mut host = TestHost::new("dev.example.echo", 4);
    assert_eq!(
        host.register_fixture(3).unwrap_err().code(),
        "stale_activation_generation"
    );
    assert!(host.commands_after(0).is_empty());
}

#[test]
fn test_host_registers_exact_identity_and_generation() {
    let mut host = TestHost::new("dev.example.echo", 4);
    host.register_fixture(4).unwrap();
    assert!(host.is_registered());

    let error = host.register_plugin_fixture("other", 4).unwrap_err();
    assert_eq!(error.code(), "plugin_identity_mismatch");
}

#[test]
fn command_queue_is_bounded_and_replayable_after_sequence() {
    let mut host = TestHost::new("dev.example.echo", 4);
    host.register_fixture(4).unwrap();
    for number in 0..MAX_QUEUED_COMMANDS {
        host.queue_command("echo", json!({"number": number}))
            .unwrap();
    }
    let error = host
        .queue_command("echo", json!({"overflow": true}))
        .unwrap_err();
    assert_eq!(error.code(), "command_queue_full");

    let replay = host.commands_after((MAX_QUEUED_COMMANDS - 2) as u64);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].sequence, (MAX_QUEUED_COMMANDS - 1) as u64);
}

#[test]
fn lifecycle_frames_are_recorded_without_tokens() {
    let mut host = TestHost::new("dev.example.echo", 4);
    host.register_fixture(4).unwrap();
    host.record_lifecycle(PluginFrame::Heartbeat(Heartbeat {
        plugin_id: "dev.example.echo".into(),
        package_digest: host.package_digest().into(),
        activation_generation: 4,
        sequence: 1,
        emitted_at_ms: 100,
    }))
    .unwrap();

    assert_eq!(host.lifecycle_frames().len(), 1);
    assert!(!format!("{:?}", host.lifecycle_frames()).contains("token"));
}
