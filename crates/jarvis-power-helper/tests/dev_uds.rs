#![cfg(feature = "dev-uds")]

use std::fs;

use jarvis_power_helper::coordinator::CoordinatorError;
use jarvis_power_helper::watchdog::ProductionStartup;

#[test]
fn production_startup_surface_still_accepts_no_path_owner_or_backend() {
    let _zero_argument_factory: fn() -> Result<ProductionStartup, CoordinatorError> =
        ProductionStartup::open;
}

#[test]
fn dev_binary_is_required_feature_only_and_production_protocol_stays_closed() {
    let manifest = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(manifest.contains("default = []"));
    assert!(manifest.contains("dev-uds = []"));
    assert!(manifest.contains("name = \"jarvis-power-helper-dev\""));
    assert!(manifest.contains("required-features = [\"dev-uds\"]"));

    let protocol = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../jarvis-power-core/src/protocol.rs"
    ))
    .unwrap();
    assert!(!protocol.contains("DevelopmentOnly"));
    assert!(!protocol.contains("RecoverExpired"));
}
