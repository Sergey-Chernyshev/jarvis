use std::env;
use std::path::PathBuf;

const UNSIGNED_TEST_TEAM_ID: &str = "UNSIGNEDT5";

fn main() {
    println!("cargo:rerun-if-env-changed=APPLE_TEAM_ID");
    println!("cargo:rerun-if-env-changed=JARVIS_APP_BUILD");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rustc-check-cfg=cfg(jarvis_power_unsigned_test)");

    let configured_build = configured_app_build();
    if env::var_os("CARGO_FEATURE_PRODUCTION_XPC").is_none() {
        return;
    }

    let unsigned_test = env::var_os("CARGO_FEATURE_UNSIGNED_TEST").is_some();
    let team_id = if unsigned_test {
        println!("cargo:rustc-cfg=jarvis_power_unsigned_test");
        UNSIGNED_TEST_TEAM_ID.to_owned()
    } else {
        env::var("APPLE_TEAM_ID")
            .expect("APPLE_TEAM_ID is required for a production power-helper build")
    };
    let minimum_build = if unsigned_test {
        configured_build.clone()
    } else {
        let supplied = env::var("JARVIS_APP_BUILD")
            .expect("JARVIS_APP_BUILD is required for a production power-helper build");
        assert_eq!(
            supplied, configured_build,
            "JARVIS_APP_BUILD must equal bundle.macOS.bundleVersion in src-tauri/tauri.conf.json"
        );
        supplied
    };

    assert!(
        valid_team_id(&team_id),
        "APPLE_TEAM_ID must be exactly 10 uppercase ASCII letters or digits"
    );
    assert!(
        minimum_build.parse::<u64>().is_ok_and(|value| value > 0),
        "JARVIS_APP_BUILD must be a positive decimal integer"
    );
    println!("cargo:rustc-env=JARVIS_POWER_TEAM_ID={team_id}");
    println!("cargo:rustc-env=JARVIS_POWER_MINIMUM_CLIENT_BUILD={minimum_build}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let deployment_target =
        env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "13.0".to_owned());
    assert_eq!(
        deployment_target, "13.0",
        "production power-helper requires MACOSX_DEPLOYMENT_TARGET=13.0"
    );

    println!("cargo:rerun-if-changed=native/xpc_server.h");
    println!("cargo:rerun-if-changed=native/xpc_server.m");
    cc::Build::new()
        .file("native/xpc_server.m")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=13.0")
        .compile("jarvis_power_xpc_server");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=Security");
    println!("cargo:rustc-link-lib=framework=System");
}

fn valid_team_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn configured_app_build() -> String {
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is required"));
    let config_path = manifest.join("../../src-tauri/tauri.conf.json");
    println!("cargo:rerun-if-changed={}", config_path.display());
    let config = std::fs::read(&config_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", config_path.display()));
    let config: serde_json::Value = serde_json::from_slice(&config)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", config_path.display()));
    let value = config
        .pointer("/bundle/macOS/bundleVersion")
        .and_then(serde_json::Value::as_str)
        .expect("bundle.macOS.bundleVersion must be a string");
    let parsed = value
        .parse::<u64>()
        .ok()
        .filter(|build| *build > 0)
        .expect("bundle.macOS.bundleVersion must be a positive decimal integer");
    assert_eq!(
        parsed.to_string(),
        value,
        "bundle.macOS.bundleVersion must be canonical decimal without leading zeroes"
    );
    value.to_owned()
}
