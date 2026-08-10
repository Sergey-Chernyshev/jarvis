fn main() {
    if let Err(error) = jarvis_power_helper::dev_uds::run_from_environment() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
