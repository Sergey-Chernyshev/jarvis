use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    STATE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};

use crate::{HelperEvent, HelperEventSink, NoopEventSink};

pub const PRODUCTION_STATE_DIRECTORY: &str = "/Library/Application Support/Jarvis/Power/v2";
pub const MAX_STATE_BYTES: usize = 64 * 1024;

const STATE_FILE: &CStr = c"state.json";
const LOCK_FILE: &CStr = c"state.lock";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorePolicy {
    directory_uid: u32,
    directory_gid: u32,
    directory_mode: u32,
    file_uid: u32,
    file_gid: u32,
    file_mode: u32,
}

impl StorePolicy {
    const PRODUCTION: Self = Self {
        directory_uid: 0,
        directory_gid: 0,
        directory_mode: DIRECTORY_MODE,
        file_uid: 0,
        file_gid: 0,
        file_mode: FILE_MODE,
    };

    #[cfg(test)]
    fn for_testing(directory_uid: u32, directory_gid: u32, file_uid: u32, file_gid: u32) -> Self {
        Self {
            directory_uid,
            directory_gid,
            directory_mode: DIRECTORY_MODE,
            file_uid,
            file_gid,
            file_mode: FILE_MODE,
        }
    }

    pub const fn directory_uid(self) -> u32 {
        self.directory_uid
    }

    pub const fn directory_gid(self) -> u32 {
        self.directory_gid
    }

    pub const fn directory_mode(self) -> u32 {
        self.directory_mode
    }

    pub const fn file_uid(self) -> u32 {
        self.file_uid
    }

    pub const fn file_gid(self) -> u32 {
        self.file_gid
    }

    pub const fn file_mode(self) -> u32 {
        self.file_mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    Unavailable,
    UnsafeMetadata,
    StateTooLarge,
    CorruptState,
    DurabilityUnknown,
    LockTimeout,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "power state storage is unavailable",
            Self::UnsafeMetadata => "power state storage metadata is unsafe",
            Self::StateTooLarge => "power state exceeds its size limit",
            Self::CorruptState => "power state is invalid",
            Self::DurabilityUnknown => "power state durability is unknown",
            Self::LockTimeout => "power state lock timed out",
        })
    }
}

impl std::error::Error for StoreError {}

struct RootStoreInner {
    directory: OwnedFd,
    policy: StorePolicy,
    events: Arc<dyn HelperEventSink>,
    process_lock: Mutex<()>,
    test_fault: Mutex<Option<StoreFault>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoreFault {
    PartialTempWrite,
    TempFsync,
    BeforeRename,
    ParentFsync,
    ClearParentFsync,
}

#[derive(Clone)]
pub struct RootStore {
    inner: Arc<RootStoreInner>,
}

impl fmt::Debug for RootStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootStore")
            .field("policy", &self.inner.policy)
            .finish_non_exhaustive()
    }
}

impl RootStore {
    pub const fn production_policy() -> StorePolicy {
        StorePolicy::PRODUCTION
    }

    /// Opens the fixed production hierarchy component-by-component.
    ///
    /// No caller path or owner policy is accepted by this constructor.
    pub fn open_production() -> Result<Self, StoreError> {
        let root = open_path(Path::new("/"), directory_open_flags())?;
        validate_intermediate_directory(root.as_raw_fd())?;
        let library = open_directory_component(root.as_raw_fd(), cstr(b"Library\0"))?;
        validate_intermediate_directory(library.as_raw_fd())?;
        let application_support =
            open_directory_component(library.as_raw_fd(), cstr(b"Application Support\0"))?;
        validate_intermediate_directory(application_support.as_raw_fd())?;
        let jarvis = open_directory_component(application_support.as_raw_fd(), cstr(b"Jarvis\0"))?;
        validate_intermediate_directory(jarvis.as_raw_fd())?;
        let power = open_directory_component(jarvis.as_raw_fd(), cstr(b"Power\0"))?;
        validate_intermediate_directory(power.as_raw_fd())?;
        let directory = open_directory_component(power.as_raw_fd(), cstr(b"v2\0"))?;
        validate_directory(directory.as_raw_fd(), StorePolicy::PRODUCTION)?;
        Ok(Self::from_open_directory(
            directory,
            StorePolicy::PRODUCTION,
            Arc::new(NoopEventSink),
        ))
    }

    /// Test-only policy injection. Production code must use [`Self::open_production`].
    #[cfg(test)]
    pub(crate) fn open_for_testing(
        path: &Path,
        directory_uid: u32,
        directory_gid: u32,
        file_uid: u32,
        file_gid: u32,
    ) -> Result<Self, StoreError> {
        Self::open_for_testing_with_sink(
            path,
            directory_uid,
            directory_gid,
            file_uid,
            file_gid,
            Arc::new(NoopEventSink),
        )
    }

    /// Test-only policy and finite event injection.
    #[cfg(test)]
    pub(crate) fn open_for_testing_with_sink(
        path: &Path,
        directory_uid: u32,
        directory_gid: u32,
        file_uid: u32,
        file_gid: u32,
        events: Arc<dyn HelperEventSink>,
    ) -> Result<Self, StoreError> {
        let policy = StorePolicy::for_testing(directory_uid, directory_gid, file_uid, file_gid);
        let directory = open_path(path, directory_open_flags())?;
        validate_directory(directory.as_raw_fd(), policy)?;
        Ok(Self::from_open_directory(directory, policy, events))
    }

    fn from_open_directory(
        directory: OwnedFd,
        policy: StorePolicy,
        events: Arc<dyn HelperEventSink>,
    ) -> Self {
        Self {
            inner: Arc::new(RootStoreInner {
                directory,
                policy,
                events,
                process_lock: Mutex::new(()),
                test_fault: Mutex::new(None),
            }),
        }
    }

    /// Read-only diagnostic access. Mutations remain crate-private and require
    /// the same exclusive transaction lock as coordinator operations.
    pub fn load(&self) -> Result<Option<HelperState>, StoreError> {
        self.lock()?.load()
    }

    pub(crate) fn lock(&self) -> Result<LockedRootStore<'_>, StoreError> {
        let process_guard = self
            .inner
            .process_lock
            .lock()
            .map_err(|_| StoreError::Unavailable)?;
        let lock = open_lock_file(self.inner.directory.as_raw_fd(), self.inner.policy)?;
        acquire_bounded_flock(lock.as_raw_fd())?;
        validate_open_file(
            self.inner.directory.as_raw_fd(),
            LOCK_FILE,
            lock.as_raw_fd(),
            self.inner.policy,
        )?;
        self.inner.events.record(HelperEvent::LockAcquired);
        Ok(LockedRootStore {
            store: self,
            lock,
            _process_guard: process_guard,
        })
    }

    pub(crate) fn events(&self) -> Arc<dyn HelperEventSink> {
        self.inner.events.clone()
    }

    #[cfg(test)]
    pub(crate) fn arm_fault(&self, fault: StoreFault) {
        *self.inner.test_fault.lock().expect("store fault lock") = Some(fault);
    }

    fn take_fault(&self, fault: StoreFault) -> bool {
        let Ok(mut armed) = self.inner.test_fault.lock() else {
            return true;
        };
        if *armed == Some(fault) {
            *armed = None;
            true
        } else {
            false
        }
    }
}

pub(crate) struct LockedRootStore<'a> {
    store: &'a RootStore,
    lock: OwnedFd,
    _process_guard: MutexGuard<'a, ()>,
}

impl LockedRootStore<'_> {
    pub(crate) fn load(&self) -> Result<Option<HelperState>, StoreError> {
        let Some(file) = open_existing_validated(
            self.store.inner.directory.as_raw_fd(),
            STATE_FILE,
            self.store.inner.policy,
        )?
        else {
            return Ok(None);
        };
        let initial = stat_fd(file.as_raw_fd())?;
        if initial.st_size < 0 || initial.st_size as u64 > MAX_STATE_BYTES as u64 {
            return Err(StoreError::StateTooLarge);
        }

        let mut bytes = Vec::with_capacity(initial.st_size as usize);
        let mut bounded = File::from(file).take((MAX_STATE_BYTES + 1) as u64);
        bounded
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError::Unavailable)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(StoreError::StateTooLarge);
        }
        validate_file_stat(
            &stat_fd(bounded.get_ref().as_raw_fd())?,
            self.store.inner.policy,
        )?;
        decode_state(&bytes).map(Some)
    }

    pub(crate) fn persist(&mut self, state: &HelperState) -> Result<(), StoreError> {
        state.validate().map_err(|_| StoreError::CorruptState)?;
        let bytes = encode_state(state)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(StoreError::StateTooLarge);
        }

        self.store
            .inner
            .events
            .record(HelperEvent::StateWriteStarted(state.phase));
        let (temporary_name, temporary) = create_temporary(
            self.store.inner.directory.as_raw_fd(),
            self.store.inner.policy,
        )?;
        let mut cleanup = TemporaryCleanup {
            directory: self.store.inner.directory.as_raw_fd(),
            name: temporary_name.clone(),
            armed: true,
        };
        let mut temporary = File::from(temporary);
        if self.store.take_fault(StoreFault::PartialTempWrite) {
            let partial = bytes.len().saturating_div(2).max(1).min(bytes.len());
            temporary
                .write_all(&bytes[..partial])
                .map_err(|_| StoreError::Unavailable)?;
            return Err(StoreError::Unavailable);
        }
        temporary
            .write_all(&bytes)
            .map_err(|_| StoreError::Unavailable)?;
        if self.store.take_fault(StoreFault::TempFsync) {
            return Err(StoreError::Unavailable);
        }
        temporary.sync_all().map_err(|_| StoreError::Unavailable)?;
        let temporary_metadata = stat_fd(temporary.as_raw_fd())?;
        validate_file_stat(&temporary_metadata, self.store.inner.policy)?;
        self.store.inner.events.record(HelperEvent::TempFileSynced);

        // Refuse to replace a symlink or any other unsafe existing entry.
        let _ = open_existing_validated(
            self.store.inner.directory.as_raw_fd(),
            STATE_FILE,
            self.store.inner.policy,
        )?;
        if self.store.take_fault(StoreFault::BeforeRename) {
            return Err(StoreError::Unavailable);
        }

        // SAFETY: both names are fixed or helper-generated C strings, both
        // directory descriptors refer to the already-open private directory,
        // and neither path is re-resolved from an absolute caller path.
        let renamed = unsafe {
            libc::renameat(
                self.store.inner.directory.as_raw_fd(),
                temporary_name.as_ptr(),
                self.store.inner.directory.as_raw_fd(),
                STATE_FILE.as_ptr(),
            )
        };
        if renamed != 0 {
            return Err(map_metadata_or_unavailable(io::Error::last_os_error()));
        }
        cleanup.armed = false;
        self.store.inner.events.record(HelperEvent::StateRenamed);
        let destination = inspect_entry(
            self.store.inner.directory.as_raw_fd(),
            STATE_FILE,
            self.store.inner.policy,
        )
        .map_err(|_| StoreError::DurabilityUnknown)?
        .ok_or(StoreError::DurabilityUnknown)?;
        require_same_file(temporary_metadata, destination)
            .map_err(|_| StoreError::DurabilityUnknown)?;
        if self.store.take_fault(StoreFault::ParentFsync) {
            return Err(StoreError::DurabilityUnknown);
        }
        fsync_fd(self.store.inner.directory.as_raw_fd())
            .map_err(|_| StoreError::DurabilityUnknown)?;
        self.store
            .inner
            .events
            .record(HelperEvent::ParentDirectorySynced);
        Ok(())
    }

    pub(crate) fn clear(&mut self) -> Result<(), StoreError> {
        if open_existing_validated(
            self.store.inner.directory.as_raw_fd(),
            STATE_FILE,
            self.store.inner.policy,
        )?
        .is_none()
        {
            return Ok(());
        }
        // SAFETY: STATE_FILE is a fixed single component resolved relative to
        // the already-open private directory.
        if unsafe {
            libc::unlinkat(
                self.store.inner.directory.as_raw_fd(),
                STATE_FILE.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(map_metadata_or_unavailable(io::Error::last_os_error()));
        }
        if self.store.take_fault(StoreFault::ClearParentFsync) {
            return Err(StoreError::DurabilityUnknown);
        }
        fsync_fd(self.store.inner.directory.as_raw_fd())
            .map_err(|_| StoreError::DurabilityUnknown)?;
        self.store.inner.events.record(HelperEvent::StateCleared);
        self.store
            .inner
            .events
            .record(HelperEvent::ParentDirectorySynced);
        Ok(())
    }
}

impl Drop for LockedRootStore<'_> {
    fn drop(&mut self) {
        // SAFETY: the guard owns a valid descriptor. Closing the descriptor
        // also releases the lock if this best-effort explicit unlock fails.
        let _ = unsafe { libc::flock(self.lock.as_raw_fd(), libc::LOCK_UN) };
        self.store.inner.events.record(HelperEvent::LockReleased);
    }
}

struct TemporaryCleanup {
    directory: RawFd,
    name: CString,
    armed: bool,
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: name was generated by this module and directory remains
            // owned by RootStore for longer than the transaction.
            let _ = unsafe { libc::unlinkat(self.directory, self.name.as_ptr(), 0) };
        }
    }
}

fn open_lock_file(directory: RawFd, policy: StorePolicy) -> Result<OwnedFd, StoreError> {
    match inspect_entry(directory, LOCK_FILE, policy) {
        Ok(Some(expected)) => {
            let file = openat_owned(
                directory,
                LOCK_FILE,
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?;
            validate_file_stat(&stat_fd(file.as_raw_fd())?, policy)?;
            require_same_file(expected, stat_fd(file.as_raw_fd())?)?;
            Ok(file)
        }
        Ok(None) => {
            let file = openat_owned(
                directory,
                LOCK_FILE,
                libc::O_RDWR
                    | libc::O_NONBLOCK
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_CREAT
                    | libc::O_EXCL,
                FILE_MODE,
            )?;
            validate_file_stat(&stat_fd(file.as_raw_fd())?, policy)?;
            fsync_fd(file.as_raw_fd())?;
            fsync_fd(directory)?;
            Ok(file)
        }
        Err(error) => Err(error),
    }
}

fn open_existing_validated(
    directory: RawFd,
    name: &CStr,
    policy: StorePolicy,
) -> Result<Option<OwnedFd>, StoreError> {
    let Some(expected) = inspect_entry(directory, name, policy)? else {
        return Ok(None);
    };
    let file = openat_owned(
        directory,
        name,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )?;
    let actual = stat_fd(file.as_raw_fd())?;
    validate_file_stat(&actual, policy)?;
    require_same_file(expected, actual)?;
    Ok(Some(file))
}

fn inspect_entry(
    directory: RawFd,
    name: &CStr,
    policy: StorePolicy,
) -> Result<Option<libc::stat>, StoreError> {
    // SAFETY: `name` is a NUL-terminated single component and `metadata` is a
    // valid output pointer. AT_SYMLINK_NOFOLLOW prevents traversal.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        validate_file_stat(&metadata, policy)?;
        Ok(Some(metadata))
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(map_metadata_or_unavailable(error))
        }
    }
}

fn create_temporary(
    directory: RawFd,
    policy: StorePolicy,
) -> Result<(CString, OwnedFd), StoreError> {
    for _ in 0..8 {
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random).map_err(|_| StoreError::Unavailable)?;
        let mut text = String::with_capacity(43);
        text.push_str(".state.tmp-");
        for byte in random {
            use std::fmt::Write as _;
            write!(&mut text, "{byte:02x}").map_err(|_| StoreError::Unavailable)?;
        }
        let name = CString::new(text).map_err(|_| StoreError::Unavailable)?;
        match openat_owned(
            directory,
            &name,
            libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_CREAT | libc::O_EXCL,
            FILE_MODE,
        ) {
            Ok(file) => {
                validate_file_stat(&stat_fd(file.as_raw_fd())?, policy)?;
                return Ok((name, file));
            }
            Err(StoreError::Unavailable)
                if io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Err(StoreError::Unavailable)
}

fn validate_open_file(
    directory: RawFd,
    name: &CStr,
    file: RawFd,
    policy: StorePolicy,
) -> Result<(), StoreError> {
    let expected = inspect_entry(directory, name, policy)?.ok_or(StoreError::UnsafeMetadata)?;
    let actual = stat_fd(file)?;
    validate_file_stat(&actual, policy)?;
    require_same_file(expected, actual)
}

fn require_same_file(expected: libc::stat, actual: libc::stat) -> Result<(), StoreError> {
    if expected.st_dev == actual.st_dev && expected.st_ino == actual.st_ino {
        Ok(())
    } else {
        Err(StoreError::UnsafeMetadata)
    }
}

fn validate_directory(file: RawFd, policy: StorePolicy) -> Result<(), StoreError> {
    validate_cloexec(file)?;
    let metadata = stat_fd(file)?;
    if file_kind(&metadata) != libc::S_IFDIR
        || metadata.st_uid != policy.directory_uid
        || metadata.st_gid != policy.directory_gid
        || permission_bits(&metadata) != policy.directory_mode
    {
        return Err(StoreError::UnsafeMetadata);
    }
    Ok(())
}

fn validate_intermediate_directory(file: RawFd) -> Result<(), StoreError> {
    validate_cloexec(file)?;
    let metadata = stat_fd(file)?;
    if file_kind(&metadata) != libc::S_IFDIR
        || metadata.st_uid != 0
        || permission_bits(&metadata) & 0o022 != 0
    {
        return Err(StoreError::UnsafeMetadata);
    }
    Ok(())
}

fn validate_file_stat(metadata: &libc::stat, policy: StorePolicy) -> Result<(), StoreError> {
    if file_kind(metadata) != libc::S_IFREG
        || metadata.st_uid != policy.file_uid
        || metadata.st_gid != policy.file_gid
        || permission_bits(metadata) != policy.file_mode
        || metadata.st_nlink != 1
    {
        return Err(StoreError::UnsafeMetadata);
    }
    Ok(())
}

fn validate_cloexec(file: RawFd) -> Result<(), StoreError> {
    // SAFETY: F_GETFD reads descriptor flags and does not access caller
    // memory.
    let flags = unsafe { libc::fcntl(file, libc::F_GETFD) };
    if flags >= 0 && flags & libc::FD_CLOEXEC != 0 {
        Ok(())
    } else {
        Err(StoreError::UnsafeMetadata)
    }
}

fn file_kind(metadata: &libc::stat) -> libc::mode_t {
    metadata.st_mode & libc::S_IFMT
}

fn permission_bits(metadata: &libc::stat) -> u32 {
    u32::from(metadata.st_mode) & 0o7777
}

fn stat_fd(file: RawFd) -> Result<libc::stat, StoreError> {
    validate_cloexec(file)?;
    // SAFETY: metadata is valid writable storage and file is an open
    // descriptor owned by this module.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe { libc::fstat(file, &mut metadata) } == 0 {
        Ok(metadata)
    } else {
        Err(StoreError::Unavailable)
    }
}

fn fsync_fd(file: RawFd) -> Result<(), StoreError> {
    // SAFETY: file is a valid open descriptor owned by RootStore.
    if unsafe { libc::fsync(file) } == 0 {
        Ok(())
    } else {
        Err(StoreError::Unavailable)
    }
}

fn acquire_bounded_flock(file: RawFd) -> Result<(), StoreError> {
    let started = Instant::now();
    loop {
        // SAFETY: file is a valid lock descriptor. LOCK_NB guarantees an XPC
        // worker cannot wait forever behind a wedged peer.
        if unsafe { libc::flock(file, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        let raw = error.raw_os_error();
        if raw != Some(libc::EWOULDBLOCK) && raw != Some(libc::EAGAIN) {
            return Err(StoreError::Unavailable);
        }
        if started.elapsed() >= LOCK_WAIT_TIMEOUT {
            return Err(StoreError::LockTimeout);
        }
        thread::sleep(LOCK_RETRY_INTERVAL);
    }
}

fn open_path(path: &Path, flags: libc::c_int) -> Result<OwnedFd, StoreError> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| StoreError::Unavailable)?;
    // SAFETY: path is NUL-terminated and flags do not require a mode.
    let file = unsafe { libc::open(path.as_ptr(), flags) };
    owned_fd(file)
}

fn open_directory_component(parent: RawFd, name: &CStr) -> Result<OwnedFd, StoreError> {
    openat_owned(parent, name, directory_open_flags(), 0)
}

fn directory_open_flags() -> libc::c_int {
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
}

fn openat_owned(
    directory: RawFd,
    name: &CStr,
    flags: libc::c_int,
    mode: u32,
) -> Result<OwnedFd, StoreError> {
    // SAFETY: name is NUL-terminated. Supplying a mode is valid with and
    // without O_CREAT; libc ignores it when creation is not requested.
    let file = unsafe { libc::openat(directory, name.as_ptr(), flags, mode as libc::c_uint) };
    owned_fd(file)
}

fn owned_fd(file: libc::c_int) -> Result<OwnedFd, StoreError> {
    if file < 0 {
        Err(map_metadata_or_unavailable(io::Error::last_os_error()))
    } else {
        // SAFETY: a non-negative result from open/openat is a new owned fd.
        Ok(unsafe { OwnedFd::from_raw_fd(file) })
    }
}

fn map_metadata_or_unavailable(error: io::Error) -> StoreError {
    match error.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::EMLINK) => StoreError::UnsafeMetadata,
        _ => StoreError::Unavailable,
    }
}

const fn cstr(bytes: &'static [u8]) -> &'static CStr {
    // SAFETY: every call site supplies one trailing NUL and no interior NUL.
    unsafe { CStr::from_bytes_with_nul_unchecked(bytes) }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredState {
    schema_version: u32,
    service_version: u64,
    minimum_client_build: u64,
    boot_id: String,
    baseline: bool,
    applied: bool,
    did_mutate: bool,
    mutation_generation: u64,
    phase: StoredMutationPhase,
    leases: Vec<StoredLease>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum StoredMutationPhase {
    Prepared,
    Applied,
    RestorePending,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredLease {
    lease_id: String,
    profile: String,
    owner_generation: String,
    principal: StoredPrincipal,
    deadline_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredPrincipal {
    uid: u32,
    pid: i32,
    process_identity: StoredProcessIdentity,
    bundle_id: String,
    team_id: String,
    requirement_digest: [u8; 32],
    signed_build: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredProcessIdentity {
    version: u16,
    start_seconds: u64,
    start_microseconds: u32,
}

fn encode_state(state: &HelperState) -> Result<Vec<u8>, StoreError> {
    let stored = StoredState::from(state);
    serde_json::to_vec(&stored).map_err(|_| StoreError::CorruptState)
}

fn decode_state(bytes: &[u8]) -> Result<HelperState, StoreError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let stored =
        StoredState::deserialize(&mut deserializer).map_err(|_| StoreError::CorruptState)?;
    deserializer.end().map_err(|_| StoreError::CorruptState)?;
    HelperState::try_from(stored)
}

impl From<&HelperState> for StoredState {
    fn from(state: &HelperState) -> Self {
        Self {
            schema_version: state.schema_version,
            service_version: state.service_version,
            minimum_client_build: state.minimum_client_build,
            boot_id: state.boot_id.clone(),
            baseline: state.baseline,
            applied: state.applied,
            did_mutate: state.did_mutate,
            mutation_generation: state.mutation_generation,
            phase: match state.phase {
                MutationPhase::Prepared => StoredMutationPhase::Prepared,
                MutationPhase::Applied => StoredMutationPhase::Applied,
                MutationPhase::RestorePending => StoredMutationPhase::RestorePending,
            },
            leases: state.leases.iter().map(StoredLease::from).collect(),
        }
    }
}

impl From<&Lease> for StoredLease {
    fn from(lease: &Lease) -> Self {
        Self {
            lease_id: lease.lease_id.as_str().to_owned(),
            profile: lease.profile.clone(),
            owner_generation: lease.owner_generation.clone(),
            principal: StoredPrincipal::from(&lease.principal),
            deadline_ms: lease.deadline.as_millis(),
        }
    }
}

impl From<&Principal> for StoredPrincipal {
    fn from(principal: &Principal) -> Self {
        let identity = principal.process_identity();
        Self {
            uid: principal.uid(),
            pid: principal.pid(),
            process_identity: StoredProcessIdentity {
                version: identity.version(),
                start_seconds: identity.start_seconds(),
                start_microseconds: identity.start_microseconds(),
            },
            bundle_id: principal.bundle_id().to_owned(),
            team_id: principal.team_id().to_owned(),
            requirement_digest: *principal.requirement_digest(),
            signed_build: principal.signed_build(),
        }
    }
}

impl TryFrom<StoredState> for HelperState {
    type Error = StoreError;

    fn try_from(stored: StoredState) -> Result<Self, Self::Error> {
        if stored.schema_version != STATE_SCHEMA_VERSION {
            return Err(StoreError::CorruptState);
        }
        let leases = stored
            .leases
            .into_iter()
            .map(Lease::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let state = Self {
            schema_version: stored.schema_version,
            service_version: stored.service_version,
            minimum_client_build: stored.minimum_client_build,
            boot_id: stored.boot_id,
            baseline: stored.baseline,
            applied: stored.applied,
            did_mutate: stored.did_mutate,
            mutation_generation: stored.mutation_generation,
            phase: match stored.phase {
                StoredMutationPhase::Prepared => MutationPhase::Prepared,
                StoredMutationPhase::Applied => MutationPhase::Applied,
                StoredMutationPhase::RestorePending => MutationPhase::RestorePending,
            },
            leases,
        };
        state.validate().map_err(|_| StoreError::CorruptState)?;
        Ok(state)
    }
}

impl TryFrom<StoredLease> for Lease {
    type Error = StoreError;

    fn try_from(stored: StoredLease) -> Result<Self, Self::Error> {
        let identity = DarwinProcessIdentity::new(
            stored.principal.process_identity.version,
            stored.principal.process_identity.start_seconds,
            stored.principal.process_identity.start_microseconds,
        )
        .map_err(|_| StoreError::CorruptState)?;
        let principal = Principal::from_helper_attestation(
            stored.principal.uid,
            stored.principal.pid,
            identity,
            stored.principal.bundle_id,
            stored.principal.team_id,
            stored.principal.requirement_digest,
            stored.principal.signed_build,
        )
        .map_err(|_| StoreError::CorruptState)?;
        Ok(Self {
            lease_id: LeaseId::parse(stored.lease_id).map_err(|_| StoreError::CorruptState)?,
            profile: stored.profile,
            owner_generation: stored.owner_generation,
            principal,
            deadline: MonotonicTime::from_millis(stored.deadline_ms),
        })
    }
}
