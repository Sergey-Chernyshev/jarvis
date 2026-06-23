fn main() {
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
