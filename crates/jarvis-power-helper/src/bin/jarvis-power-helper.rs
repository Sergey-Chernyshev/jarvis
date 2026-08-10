#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the production power-helper is supported only on macOS 13 or newer");
    std::process::exit(78);
}

#[cfg(all(target_os = "macos", jarvis_power_unsigned_test))]
fn main() {
    eprintln!("unsigned-test power-helper builds are compile-only and cannot serve requests");
    std::process::exit(78);
}

#[cfg(all(target_os = "macos", not(jarvis_power_unsigned_test)))]
fn main() {
    if let Err(error) = jarvis_power_helper::xpc_server::run_production() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
