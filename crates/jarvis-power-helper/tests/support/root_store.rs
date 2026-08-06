#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fs::{self, File};
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Barrier};
#[cfg(target_os = "macos")]
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    STATE_SCHEMA_VERSION,
};
use jarvis_power_helper::root_store::{
    is_expected_creation_race_metadata_for_testing,
    validate_new_private_directory_metadata_for_testing,
    validate_trusted_anchor_metadata_for_testing, verify_entry_identity_for_testing, RootStore,
    StoreError, StoreFault, MAX_STATE_BYTES,
};
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

struct ProvisionFixture {
    _temp: TempDir,
    library: std::path::PathBuf,
    anchor: std::path::PathBuf,
    uid: u32,
    gid: u32,
}

#[cfg(target_os = "macos")]
fn supplementary_gid_other_than(primary_gid: u32) -> u32 {
    // SAFETY: a null first call queries the required group count.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(count > 0);
    let mut groups = vec![0 as libc::gid_t; usize::try_from(count).unwrap()];
    // SAFETY: groups has exactly `count` writable gid_t entries.
    let read = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    assert_eq!(read, count);
    groups
        .into_iter()
        .find(|gid| *gid != primary_gid)
        .expect("macOS test user must have a supplementary group")
}

#[cfg(target_os = "macos")]
fn set_group(path: &Path, gid: u32) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is NUL terminated; uid_t::MAX preserves the current owner.
    assert_eq!(
        unsafe { libc::chown(path.as_ptr(), libc::uid_t::MAX, gid) },
        0
    );
}

impl ProvisionFixture {
    fn empty() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let anchor = library.join("Application Support");
        fs::create_dir(&library).unwrap();
        fs::set_permissions(&library, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(&anchor).unwrap();
        fs::set_permissions(&anchor, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _temp: temp,
            library,
            anchor,
            // SAFETY: these libc calls only query the current test process.
            uid: unsafe { libc::geteuid() },
            // SAFETY: these libc calls only query the current test process.
            gid: unsafe { libc::getegid() },
        }
    }

    fn library_fd(&self) -> OwnedFd {
        File::open(&self.library).unwrap().into()
    }

    fn provision(&self) -> Result<RootStore, StoreError> {
        self.provision_with_private_owner(self.uid, self.gid)
    }

    fn provision_with_private_owner(
        &self,
        directory_uid: u32,
        directory_gid: u32,
    ) -> Result<RootStore, StoreError> {
        RootStore::provision_from_parent_for_testing(
            self.library_fd(),
            self.uid,
            directory_uid,
            directory_gid,
            directory_uid,
            directory_gid,
        )
    }

    fn private_components(&self) -> [std::path::PathBuf; 3] {
        let jarvis = self.anchor.join("Jarvis");
        let power = jarvis.join("Power");
        let v2 = power.join("v2");
        [jarvis, power, v2]
    }
}

#[test]
fn trusted_anchor_accepts_nonwheel_0755_metadata_and_rejects_writable_or_substituted() {
    // SAFETY: libc::stat is plain old data and this value is used only by the
    // pure metadata validator.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    metadata.st_mode = libc::S_IFDIR | 0o755;
    metadata.st_uid = 501;
    metadata.st_gid = 80;
    assert_eq!(
        validate_trusted_anchor_metadata_for_testing(&metadata, 501),
        Ok(())
    );

    metadata.st_mode = libc::S_IFDIR | 0o775;
    assert_eq!(
        validate_trusted_anchor_metadata_for_testing(&metadata, 501),
        Err(StoreError::UnsafeMetadata)
    );
    assert_eq!(
        verify_entry_identity_for_testing((1, 2), (1, 3)),
        Err(StoreError::UnsafeMetadata)
    );
}

#[test]
fn clean_install_provisions_only_private_descendants_from_the_anchor_fd() {
    let fixture = ProvisionFixture::empty();
    let store = fixture.provision().unwrap();

    for component in fixture.private_components() {
        let metadata = fs::symlink_metadata(component).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), fixture.uid);
        assert_eq!(metadata.gid(), fixture.gid);
    }
    assert_eq!(store.load().unwrap(), None);
    assert_eq!(
        fs::symlink_metadata(&fixture.anchor).unwrap().mode() & 0o7777,
        0o755
    );
}

#[test]
fn clean_install_allows_anchor_gid_only_before_new_fd_owner_normalization() {
    // Darwin inherits the parent directory group for mkdirat. A normal
    // root:admin Application Support anchor therefore yields root:admin here,
    // even though the private policy must finish as root:wheel.
    // SAFETY: libc::stat is plain old data and this value is used only by the
    // pure pre-normalization metadata validator.
    let mut inherited = unsafe { std::mem::zeroed::<libc::stat>() };
    inherited.st_mode = libc::S_IFDIR | 0o700;
    inherited.st_uid = 0;
    inherited.st_gid = 80;
    assert_eq!(
        validate_new_private_directory_metadata_for_testing(&inherited, 0, 0, 80, 0o700),
        Ok(())
    );
    assert!(is_expected_creation_race_metadata_for_testing(
        &inherited, 0, 0, 80, 0o700
    ));

    inherited.st_uid = 501;
    assert_eq!(
        validate_new_private_directory_metadata_for_testing(&inherited, 0, 0, 80, 0o700),
        Err(StoreError::UnsafeMetadata)
    );
    assert!(!is_expected_creation_race_metadata_for_testing(
        &inherited, 0, 0, 80, 0o700
    ));
    inherited.st_uid = 0;
    inherited.st_gid = 81;
    assert_eq!(
        validate_new_private_directory_metadata_for_testing(&inherited, 0, 0, 80, 0o700),
        Err(StoreError::UnsafeMetadata)
    );
    assert!(!is_expected_creation_race_metadata_for_testing(
        &inherited, 0, 0, 80, 0o700
    ));
    inherited.st_gid = 80;
    inherited.st_mode = libc::S_IFDIR | 0o755;
    assert_eq!(
        validate_new_private_directory_metadata_for_testing(&inherited, 0, 0, 80, 0o700),
        Err(StoreError::UnsafeMetadata)
    );
    assert!(!is_expected_creation_race_metadata_for_testing(
        &inherited, 0, 0, 80, 0o700
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn concurrent_clean_install_retries_inherited_anchor_gid_then_normalizes_created_fds() {
    let fixture = ProvisionFixture::empty();
    let inherited_gid = supplementary_gid_other_than(fixture.gid);
    set_group(&fixture.anchor, inherited_gid);
    assert_eq!(
        fs::symlink_metadata(&fixture.anchor).unwrap().gid(),
        inherited_gid
    );

    let first_library = fixture.library.clone();
    let uid = fixture.uid;
    let private_gid = fixture.gid;
    let paused_once = Arc::new(AtomicBool::new(false));
    let pause_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (created_tx, created_rx) = mpsc::sync_channel(1);
    let first = {
        let paused_once = paused_once.clone();
        let pause_gate = pause_gate.clone();
        thread::spawn(move || {
            let parent: OwnedFd = File::open(first_library).unwrap().into();
            RootStore::provision_from_parent_with_creation_hook_for_testing(
                parent,
                uid,
                uid,
                private_gid,
                uid,
                private_gid,
                move || {
                    if !paused_once.swap(true, Ordering::SeqCst) {
                        created_tx.send(()).unwrap();
                        let released = pause_gate.0.lock().unwrap();
                        drop(
                            pause_gate
                                .1
                                .wait_timeout_while(released, Duration::from_secs(2), |released| {
                                    !*released
                                })
                                .unwrap(),
                        );
                    }
                },
            )
        })
    };
    let creator_paused = created_rx.recv_timeout(Duration::from_millis(500)).is_ok();
    if !creator_paused {
        let mut released = pause_gate.0.lock().unwrap();
        *released = true;
        pause_gate.1.notify_all();
        drop(released);
        let _ = first.join();
        panic!("first provisioner did not pause after mkdirat");
    }
    assert_eq!(
        fs::symlink_metadata(fixture.anchor.join("Jarvis"))
            .unwrap()
            .gid(),
        inherited_gid
    );

    let second_library = fixture.library.clone();
    let (eexist_tx, eexist_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::sync_channel(1);
    let second = thread::spawn(move || {
        let parent: OwnedFd = File::open(second_library).unwrap().into();
        let result = RootStore::provision_from_parent_with_eexist_hook_for_testing(
            parent,
            uid,
            uid,
            private_gid,
            uid,
            private_gid,
            move || {
                let _ = eexist_tx.send(());
            },
        );
        second_tx.send(result).unwrap();
    });
    let second_observed_eexist = eexist_rx.recv_timeout(Duration::from_millis(500)).is_ok();
    let before_normalization = if second_observed_eexist {
        second_rx.recv_timeout(Duration::from_millis(30))
    } else {
        Err(mpsc::RecvTimeoutError::Timeout)
    };
    let second_waited_for_normalization =
        matches!(&before_normalization, Err(mpsc::RecvTimeoutError::Timeout));

    {
        let mut released = pause_gate.0.lock().unwrap();
        *released = true;
        pause_gate.1.notify_all();
    }
    let first_result = first.join().unwrap();
    let second_result = match before_normalization {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            second_rx.recv_timeout(Duration::from_millis(500)).unwrap()
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("second provisioner disconnected"),
    };
    second.join().unwrap();

    assert!(second_observed_eexist);
    assert!(second_waited_for_normalization);
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
    for component in fixture.private_components() {
        let metadata = fs::symlink_metadata(component).unwrap();
        assert_eq!(metadata.uid(), fixture.uid);
        assert_eq!(metadata.gid(), fixture.gid);
        assert_eq!(metadata.mode() & 0o7777, 0o700);
    }
}

#[test]
fn unsafe_existing_descendant_or_anchor_is_rejected_without_repair_or_following() {
    let writable_anchor = ProvisionFixture::empty();
    fs::set_permissions(&writable_anchor.anchor, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(matches!(
        writable_anchor.provision(),
        Err(StoreError::UnsafeMetadata)
    ));
    assert_eq!(
        fs::symlink_metadata(&writable_anchor.anchor)
            .unwrap()
            .mode()
            & 0o7777,
        0o775
    );

    let file = ProvisionFixture::empty();
    fs::write(file.anchor.join("Jarvis"), b"sentinel").unwrap();
    assert!(matches!(file.provision(), Err(StoreError::UnsafeMetadata)));
    assert_eq!(fs::read(file.anchor.join("Jarvis")).unwrap(), b"sentinel");

    let mode = ProvisionFixture::empty();
    fs::create_dir(mode.anchor.join("Jarvis")).unwrap();
    fs::set_permissions(
        mode.anchor.join("Jarvis"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(mode.provision(), Err(StoreError::UnsafeMetadata)));
    assert_eq!(
        fs::symlink_metadata(mode.anchor.join("Jarvis"))
            .unwrap()
            .mode()
            & 0o7777,
        0o755
    );

    let owner = ProvisionFixture::empty();
    fs::create_dir(owner.anchor.join("Jarvis")).unwrap();
    fs::set_permissions(
        owner.anchor.join("Jarvis"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(matches!(
        owner.provision_with_private_owner(owner.uid.wrapping_add(1), owner.gid),
        Err(StoreError::UnsafeMetadata)
    ));
    assert_eq!(
        fs::symlink_metadata(owner.anchor.join("Jarvis"))
            .unwrap()
            .uid(),
        owner.uid
    );

    let symlinked = ProvisionFixture::empty();
    let outside = symlinked._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside").unwrap();
    symlink(&outside, symlinked.anchor.join("Jarvis")).unwrap();
    assert!(matches!(
        symlinked.provision(),
        Err(StoreError::UnsafeMetadata)
    ));
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    assert!(fs::symlink_metadata(symlinked.anchor.join("Jarvis"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn concurrent_clean_install_converges_on_one_safe_tree() {
    let fixture = ProvisionFixture::empty();
    let barrier = Arc::new(Barrier::new(4));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let library = fixture.library.clone();
        let barrier = barrier.clone();
        let uid = fixture.uid;
        let gid = fixture.gid;
        workers.push(thread::spawn(move || {
            let parent: OwnedFd = File::open(library).unwrap().into();
            barrier.wait();
            RootStore::provision_from_parent_for_testing(parent, uid, uid, gid, uid, gid)
        }));
    }
    for worker in workers {
        assert!(worker.join().unwrap().is_ok());
    }
    for component in fixture.private_components() {
        let metadata = fs::symlink_metadata(component).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), fixture.uid);
        assert_eq!(metadata.gid(), fixture.gid);
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
