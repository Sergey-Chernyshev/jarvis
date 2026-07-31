use std::time::Duration;

use jarvis_power_helper::pmset::SystemPmset;
use jarvis_power_helper::watchdog::WATCHDOG_INTERVAL;

#[test]
fn production_pmset_and_watchdog_policy_is_closed() {
    let policy = SystemPmset::policy();
    assert_eq!(policy.program(), "/usr/bin/pmset");
    assert_eq!(policy.timeout(), Duration::from_secs(8));
    assert_eq!(policy.read_args(), ["-g"]);
    assert_eq!(policy.write_args(false), ["-a", "disablesleep", "0"]);
    assert_eq!(policy.write_args(true), ["-a", "disablesleep", "1"]);
    assert!(policy.stdin_is_null());
    assert!(policy.environment_is_cleared());
    assert!(policy.output_is_bounded());
    assert_eq!(WATCHDOG_INTERVAL, Duration::from_secs(1));
}
