use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    STATE_SCHEMA_VERSION,
};
use jarvis_power_helper::root_store::StoreFault;
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
        RootStore::open_for_testing(&self.directory, self.uid, self.gid, self.uid, self.gid)
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
fn unsafe_lock_metadata_is_rejected_before_flock_or_state_access() {
    let symlinked = Fixture::new();
    let outside = symlinked._temp.path().join("lock-outside");
    fs::write(&outside, b"sentinel").unwrap();
    symlink(&outside, symlinked.directory.join("state.lock")).unwrap();
    assert_eq!(symlinked.store().load(), Err(StoreError::UnsafeMetadata));
    assert_eq!(fs::read(outside).unwrap(), b"sentinel");

    let mode = Fixture::new();
    fs::write(mode.directory.join("state.lock"), b"").unwrap();
    fs::set_permissions(
        mode.directory.join("state.lock"),
        fs::Permissions::from_mode(0o620),
    )
    .unwrap();
    assert_eq!(mode.store().load(), Err(StoreError::UnsafeMetadata));

    let hardlink = Fixture::new();
    fs::write(hardlink.directory.join("state.lock"), b"").unwrap();
    fs::set_permissions(
        hardlink.directory.join("state.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::hard_link(
        hardlink.directory.join("state.lock"),
        hardlink.directory.join("lock-link"),
    )
    .unwrap();
    assert_eq!(hardlink.store().load(), Err(StoreError::UnsafeMetadata));

    let kind = Fixture::new();
    fs::create_dir(kind.directory.join("state.lock")).unwrap();
    assert_eq!(kind.store().load(), Err(StoreError::UnsafeMetadata));

    let owner = Fixture::new();
    fs::write(owner.directory.join("state.lock"), b"").unwrap();
    fs::set_permissions(
        owner.directory.join("state.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let wrong_owner = RootStore::open_for_testing(
        &owner.directory,
        owner.uid,
        owner.gid,
        owner.uid.wrapping_add(1),
        owner.gid,
    )
    .unwrap();
    assert_eq!(wrong_owner.load(), Err(StoreError::UnsafeMetadata));
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
    duplicate.write_state(br#"{"schemaVersion":2,"schemaVersion":2}"#, 0o600);
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
    fs::rename(&fixture.directory, moved).unwrap();
    let decoy = fixture._temp.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(decoy.join("state.json"), vec![b'x'; MAX_STATE_BYTES + 1]).unwrap();
    fs::set_permissions(decoy.join("state.json"), fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&decoy, &fixture.directory).unwrap();

    assert_eq!(store.load(), Err(StoreError::CorruptState));
}

#[test]
fn stale_temporary_files_are_ignored_and_left_as_evidence() {
    let fixture = Fixture::new();
    let stale = fixture.directory.join(".state.tmp-stale");
    fs::write(&stale, b"interrupted").unwrap();
    fs::set_permissions(&stale, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(fixture.store().load().unwrap(), None);
    assert_eq!(fs::read(stale).unwrap(), b"interrupted");
}

#[test]
fn interrupted_temp_write_fsync_and_rename_keep_the_old_state() {
    for fault in [
        StoreFault::PartialTempWrite,
        StoreFault::TempFsync,
        StoreFault::BeforeRename,
    ] {
        let fixture = Fixture::new();
        let store = fixture.store();
        let old = valid_state(7, 1);
        persist(&store, &old).unwrap();
        store.arm_fault(fault);

        assert_eq!(
            persist(&store, &valid_state(8, 2)),
            Err(StoreError::Unavailable)
        );
        assert_eq!(store.load().unwrap(), Some(old));
        assert!(!fs::read_dir(&fixture.directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".state.tmp-")
        }));
    }
}

#[test]
fn post_rename_or_clear_fsync_failure_is_reported_as_durability_unknown() {
    let fixture = Fixture::new();
    let store = fixture.store();
    persist(&store, &valid_state(7, 1)).unwrap();
    let replacement = valid_state(8, 2);
    store.arm_fault(StoreFault::ParentFsync);
    assert_eq!(
        persist(&store, &replacement),
        Err(StoreError::DurabilityUnknown)
    );
    assert_eq!(store.load().unwrap(), Some(replacement));

    store.arm_fault(StoreFault::ClearParentFsync);
    let mut transaction = store.lock().unwrap();
    assert_eq!(transaction.clear(), Err(StoreError::DurabilityUnknown));
    drop(transaction);
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn separate_processes_serialize_on_the_same_held_flock() {
    let fixture = Fixture::new();
    let ready = fixture._temp.path().join("child-ready");
    let release = fixture._temp.path().join("child-release");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "root_store_contract_tests::child_holds_store_lock",
            "--ignored",
            "--nocapture",
        ])
        .env("JARVIS_TEST_STORE_DIR", &fixture.directory)
        .env("JARVIS_TEST_STORE_UID", fixture.uid.to_string())
        .env("JARVIS_TEST_STORE_GID", fixture.gid.to_string())
        .env("JARVIS_TEST_STORE_READY", &ready)
        .env("JARVIS_TEST_STORE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "child never acquired the store lock");

    let second = fixture.store();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || sender.send(second.load()).unwrap());
    thread::sleep(Duration::from_millis(100));
    assert_eq!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty));

    fs::write(&release, b"go").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(None)
    );
    waiter.join().unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
#[ignore = "spawned by separate_processes_serialize_on_the_same_held_flock"]
fn child_holds_store_lock() {
    let Ok(directory) = std::env::var("JARVIS_TEST_STORE_DIR") else {
        return;
    };
    let uid = std::env::var("JARVIS_TEST_STORE_UID")
        .unwrap()
        .parse()
        .unwrap();
    let gid = std::env::var("JARVIS_TEST_STORE_GID")
        .unwrap()
        .parse()
        .unwrap();
    let ready = std::env::var("JARVIS_TEST_STORE_READY").unwrap();
    let release = std::env::var("JARVIS_TEST_STORE_RELEASE").unwrap();
    let store = RootStore::open_for_testing(Path::new(&directory), uid, gid, uid, gid).unwrap();
    let _lock = store.lock().unwrap();
    fs::write(ready, b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !Path::new(&release).exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

fn valid_state(mutation_generation: u64, lease_number: u128) -> HelperState {
    let principal = Principal::from_helper_attestation(
        503,
        42,
        DarwinProcessIdentity::new(1, 100, 20).unwrap(),
        "app.jarvis.monitor",
        "ABCDE12345",
        [9; 32],
        1,
    )
    .unwrap();
    HelperState {
        schema_version: STATE_SCHEMA_VERSION,
        service_version: 1,
        minimum_client_build: 1,
        boot_id: "boot-a".to_owned(),
        baseline: false,
        applied: true,
        did_mutate: true,
        mutation_generation,
        phase: MutationPhase::Applied,
        leases: vec![Lease {
            lease_id: LeaseId::parse(format!("{lease_number:032x}")).unwrap(),
            profile: "prod".to_owned(),
            owner_generation: "generation-a".to_owned(),
            principal,
            deadline: MonotonicTime::from_millis(46_000),
        }],
    }
}

fn persist(store: &RootStore, state: &HelperState) -> Result<(), StoreError> {
    store.lock()?.persist(state)
}
