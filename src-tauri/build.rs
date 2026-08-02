#[path = "src/app_command_inventory.rs"]
mod app_command_inventory;

macro_rules! define_app_command_names {
    ($(($name:literal, $handler:path, $webviews:expr)),* $(,)?) => {
        const APP_COMMAND_NAMES: &[&str] = &[$($name),*];
    };
}

crate::app_command_inventory::with_app_commands!(define_app_command_names);

fn main() {
    ensure_external_bin_placeholder();
    build_power_helper_client();

    // Dev-only: встроить Info.plist (с NSMicrophoneUsageDescription) в RAW-бинарь
    // `jarvis`, чтобы macOS мог показать диалог разрешения микрофона при запуске
    // через `cargo run` (без .app-бандла). Гейтим переменной JARVIS_DEV_SIGN, чтобы
    // нотаризованный бандл (со своим Info.plist) остался нетронутым.
    println!("cargo:rerun-if-env-changed=JARVIS_DEV_SIGN");
    #[cfg(target_os = "macos")]
    if std::env::var_os("JARVIS_DEV_SIGN").is_some() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
        println!("cargo:rerun-if-changed=dev-Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=jarvis=-Wl,-sectcreate,__TEXT,__info_plist,{manifest}/dev-Info.plist"
        );
    }
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMAND_NAMES)),
    )
    .expect("tauri build with explicit app command manifest");
}

fn build_power_helper_client() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=native/power_helper_client.h");
    println!("cargo:rerun-if-changed=native/power_helper_client.m");
    cc::Build::new()
        .file("native/power_helper_client.m")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=13.0")
        .compile("jarvis_power_helper_client");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=ServiceManagement");
    println!("cargo:rustc-link-lib=framework=System");
}

/// `tauri-build` валидирует externalBin даже для `cargo test`. Release/dev
/// workflows заранее кладут настоящий sidecar через prepare script; для чистого
/// compile/test checkout достаточно неисполняемой по смыслу shell-заглушки.
/// Каталог gitignored, в bundle она не попадёт: beforeBuildCommand заменяет её.
fn ensure_external_bin_placeholder() {
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let Ok(target) = std::env::var("TARGET") else {
        return;
    };
    let directory = std::path::Path::new(&manifest_dir).join("binaries");
    let path = directory.join(format!("jarvis-agent-vm-plugin-{target}"));
    if path.exists() || std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    if std::fs::write(
        &path,
        b"#!/bin/sh\necho '[agent-vm] sidecar was not prepared' >&2\nexit 127\n",
    )
    .is_err()
    {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
}
