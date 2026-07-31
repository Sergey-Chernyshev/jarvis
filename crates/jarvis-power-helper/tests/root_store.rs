use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use jarvis_power_helper::root_store::{RootStore, StoreError, MAX_STATE_BYTES};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    directory: std::path::PathBuf,
    uid: u32,
    gid: u32,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("v2");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _temp: temp,
            directory,
            // SAFETY: these libc calls only query the current test process.
            uid: unsafe { libc::geteuid() },
            // SAFETY: these libc calls only query the current test process.
            gid: unsafe { libc::getegid() },
        }
    }

    fn store(&self) -> RootStore {
        RootStore::open_for_testing(
            &self.directory,
            self.uid,
            self.gid,
            self.uid,
            self.gid,
        )
        .unwrap()
    }

    fn state_path(&self) -> std::path::PathBuf {
        self.directory.join("state.json")
    }

    fn write_state(&self, bytes: &[u8], mode: u32) {
        fs::write(self.state_path(), bytes).unwrap();
        fs::set_permissions(self.state_path(), fs::Permissions::from_mode(mode)).unwrap();
    }
}

#[test]
fn symlink_is_rejected_without_following_or_overwriting_outside_bytes() {
    let fixture = Fixture::new();
    let outside = fixture._temp.path().join("outside");
    fs::write(&outside, b"sentinel").unwrap();
    symlink(&outside, fixture.state_path()).unwrap();

    assert_eq!(fixture.store().load(), Err(StoreError::UnsafeMetadata));
    assert_eq!(fs::read(outside).unwrap(), b"sentinel");
}

#[test]
fn unexpected_owner_mode_hardlink_and_kind_are_rejected() {
    let owner = Fixture::new();
    owner.write_state(b"{}", 0o600);
    let wrong_owner = RootStore::open_for_testing(
        &owner.directory,
        owner.uid,
        owner.gid,
        owner.uid.wrapping_add(1),
        owner.gid,
    )
    .unwrap();
    assert_eq!(wrong_owner.load(), Err(StoreError::UnsafeMetadata));

    let mode = Fixture::new();
    mode.write_state(b"{}", 0o620);
    assert_eq!(mode.store().load(), Err(StoreError::UnsafeMetadata));

    let hardlink = Fixture::new();
    hardlink.write_state(b"{}", 0o600);
    fs::hard_link(
        hardlink.state_path(),
        hardlink.directory.join("second-link"),
    )
    .unwrap();
    assert_eq!(hardlink.store().load(), Err(StoreError::UnsafeMetadata));

    let kind = Fixture::new();
    fs::create_dir(kind.state_path()).unwrap();
    assert_eq!(kind.store().load(), Err(StoreError::UnsafeMetadata));
}

#[test]
fn production_policy_is_fixed_to_root_wheel_and_private_modes() {
    let policy = RootStore::production_policy();
    assert_eq!(policy.directory_uid(), 0);
    assert_eq!(policy.directory_gid(), 0);
    assert_eq!(policy.directory_mode(), 0o700);
    assert_eq!(policy.file_uid(), 0);
    assert_eq!(policy.file_gid(), 0);
    assert_eq!(policy.file_mode(), 0o600);
}

#[test]
fn unbounded_duplicate_unknown_and_trailing_state_fail_closed() {
    let oversized = Fixture::new();
    oversized.write_state(&vec![b'x'; MAX_STATE_BYTES + 1], 0o600);
    assert_eq!(oversized.store().load(), Err(StoreError::StateTooLarge));

    let duplicate = Fixture::new();
    duplicate.write_state(
        br#"{"schemaVersion":2,"schemaVersion":2}"#,
        0o600,
    );
    assert_eq!(duplicate.store().load(), Err(StoreError::CorruptState));

    let unknown = Fixture::new();
    unknown.write_state(br#"{"unexpected":"field"}"#, 0o600);
    assert_eq!(unknown.store().load(), Err(StoreError::CorruptState));

    let trailing = Fixture::new();
    trailing.write_state(b"{}not-json", 0o600);
    assert_eq!(trailing.store().load(), Err(StoreError::CorruptState));
}

#[test]
fn opened_directory_fd_is_used_after_the_original_path_is_replaced() {
    let fixture = Fixture::new();
    fixture.write_state(b"{}", 0o600);
    let store = fixture.store();

    let moved = fixture._temp.path().join("moved-v2");
    fs::rename(&fixture.directory, &moved).unwrap();
    let decoy = fixture._temp.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(decoy.join("state.json"), vec![b'x'; MAX_STATE_BYTES + 1]).unwrap();
    fs::set_permissions(
        decoy.join("state.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    symlink(&decoy, &fixture.directory).unwrap();

    assert_eq!(store.load(), Err(StoreError::CorruptState));
}

