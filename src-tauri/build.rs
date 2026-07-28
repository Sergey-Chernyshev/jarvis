fn main() {
    ensure_external_bin_placeholder();

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
    tauri_build::build()
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
