use jarvis_power_helper::root_store::{RootStore, PRODUCTION_STATE_DIRECTORY};

#[test]
fn production_store_surface_is_fixed_to_the_root_owned_location() {
    let policy = RootStore::production_policy();
    assert_eq!(
        PRODUCTION_STATE_DIRECTORY,
        "/Library/Application Support/Jarvis/Power/v2"
    );
    assert_eq!(policy.directory_uid(), 0);
    assert_eq!(policy.directory_gid(), 0);
    assert_eq!(policy.directory_mode(), 0o700);
    assert_eq!(policy.file_uid(), 0);
    assert_eq!(policy.file_gid(), 0);
    assert_eq!(policy.file_mode(), 0o600);
}
