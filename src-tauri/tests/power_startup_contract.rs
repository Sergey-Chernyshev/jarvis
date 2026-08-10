#[test]
fn power_startup_respects_both_persisted_plugin_enable_flags() {
    let source = include_str!("../src/power/mod.rs");
    let init = source
        .split_once("pub fn init")
        .expect("Power::init")
        .1
        .split_once("fn activate_keep_awake")
        .expect("init boundary")
        .0;

    assert!(init.contains("ka_settings(d)[\"enabled\"]"));
    assert!(init.contains("cs_settings(d)[\"enabled\"]"));
    assert!(init.contains("activate_keep_awake"));
    assert!(init.contains("activate_clamshell"));
}
