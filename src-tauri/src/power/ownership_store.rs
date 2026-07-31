use crate::power::ownership::OwnershipState;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);
const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub fn global_registry_path() -> PathBuf {
    crate::util::home_dir().join("Library/Application Support/Jarvis/power/ownership.json")
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    LockTimeout { path: PathBuf, timeout: Duration },
    Corrupt(String),
    Serialize(serde_json::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "ownership store I/O failed: {error}"),
            Self::LockTimeout { path, timeout } => write!(
                formatter,
                "ownership lock {} was not acquired within {} ms",
                path.display(),
                timeout.as_millis()
            ),
            Self::Corrupt(error) => write!(formatter, "ownership registry is corrupt: {error}"),
            Self::Serialize(error) => {
                write!(
                    formatter,
                    "ownership registry serialization failed: {error}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Serialize(error) => Some(error),
            Self::LockTimeout { .. } | Self::Corrupt(_) => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct OwnershipStore {
    path: PathBuf,
}

impl OwnershipStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn global() -> Self {
        Self::at(global_registry_path())
    }

    /// Holds the machine-wide registry lock until the returned guard is dropped.
    /// Multi-step power transactions must use the guard methods throughout;
    /// calling the store-level convenience methods while holding it would relock.
    pub fn lock(&self) -> Result<OwnershipStoreGuard<'_>, StoreError> {
        self.lock_with_timeout(DEFAULT_LOCK_TIMEOUT)
    }

    pub fn lock_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<OwnershipStoreGuard<'_>, StoreError> {
        let lock_path = self.path.with_extension("lock");
        let lock_file = self.open_lock_file(&lock_path)?;
        let started = Instant::now();

        loop {
            match try_exclusive_lock(&lock_file) {
                Ok(true) => {
                    return Ok(OwnershipStoreGuard {
                        store: self,
                        lock_file,
                    });
                }
                Ok(false) if started.elapsed() >= timeout => {
                    return Err(StoreError::LockTimeout {
                        path: lock_path,
                        timeout,
                    });
                }
                Ok(false) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    thread::sleep(LOCK_POLL_INTERVAL.min(remaining));
                }
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
    }

    pub fn try_lock(&self) -> Result<Option<OwnershipStoreGuard<'_>>, StoreError> {
        let lock_path = self.path.with_extension("lock");
        let lock_file = self.open_lock_file(&lock_path)?;
        match try_exclusive_lock(&lock_file) {
            Ok(true) => Ok(Some(OwnershipStoreGuard {
                store: self,
                lock_file,
            })),
            Ok(false) => Ok(None),
            Err(error) => Err(StoreError::Io(error)),
        }
    }

    pub fn read(&self) -> Result<Option<OwnershipState>, StoreError> {
        self.lock()?.read()
    }

    pub fn write(&self, state: &OwnershipState) -> Result<(), StoreError> {
        self.lock()?.write(state)
    }

    pub fn clear(&self) -> Result<(), StoreError> {
        self.lock()?.clear()
    }

    fn open_lock_file(&self, lock_path: &Path) -> Result<File, StoreError> {
        let parent = parent_dir(&self.path);
        fs::create_dir_all(parent)?;
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?)
    }
}

fn try_exclusive_lock(lock_file: &File) -> io::Result<bool> {
    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

#[derive(Debug)]
pub struct OwnershipStoreGuard<'a> {
    store: &'a OwnershipStore,
    lock_file: File,
}

impl OwnershipStoreGuard<'_> {
    pub fn read(&self) -> Result<Option<OwnershipState>, StoreError> {
        let mut file = match File::open(&self.store.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StoreError::Io(error)),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let state: OwnershipState = serde_json::from_slice(&bytes).map_err(|error| {
            StoreError::Corrupt(format!("{}: {error}", self.store.path.display()))
        })?;
        if state.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(StoreError::Corrupt(format!(
                "{}: unsupported schema version {}; expected {}",
                self.store.path.display(),
                state.schema_version,
                SUPPORTED_SCHEMA_VERSION
            )));
        }
        Ok(Some(state))
    }

    pub fn write(&self, state: &OwnershipState) -> Result<(), StoreError> {
        let mut bytes = serde_json::to_vec_pretty(state).map_err(StoreError::Serialize)?;
        bytes.push(b'\n');

        let parent = parent_dir(&self.store.path);
        let file_name = self
            .store
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let temp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ));

        let result = (|| -> Result<(), StoreError> {
            let mut temp = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)?;
            temp.write_all(&bytes)?;
            temp.sync_all()?;
            drop(temp);
            fs::rename(&temp_path, &self.store.path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// Removes a registry whose restore has already been confirmed by the
    /// transaction holding this guard, then durably records the removal.
    pub fn clear(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.store.path) {
            Ok(()) => File::open(parent_dir(&self.store.path))?.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::Io(error)),
        }
        Ok(())
    }
}

impl Drop for OwnershipStoreGuard<'_> {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.lock_file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::{OwnershipStore, StoreError};
    use crate::power::ownership::OwnershipState;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn unique_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "jarvis-ownership-store-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn atomic_round_trip_preserves_registry() {
        let dir = unique_test_dir("round-trip");
        let store = OwnershipStore::at(dir.join("ownership.json"));
        let expected = OwnershipState::new(false, "boot-a", 3);

        store.write(&expected).unwrap();

        assert_eq!(store.read().unwrap(), Some(expected));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_registry_is_fail_closed() {
        let dir = unique_test_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let registry_path = dir.join("ownership.json");
        std::fs::write(&registry_path, b"{").unwrap();
        let store = OwnershipStore::at(&registry_path);

        assert!(matches!(store.read(), Err(StoreError::Corrupt(_))));
        assert_eq!(std::fs::read(registry_path).unwrap(), b"{");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_schema_is_fail_closed() {
        let dir = unique_test_dir("unsupported-schema");
        std::fs::create_dir_all(&dir).unwrap();
        let registry_path = dir.join("ownership.json");
        let mut value = serde_json::to_value(OwnershipState::new(false, "boot-a", 3)).unwrap();
        value["schemaVersion"] = 2.into();
        std::fs::write(&registry_path, serde_json::to_vec(&value).unwrap()).unwrap();
        let store = OwnershipStore::at(&registry_path);

        assert!(matches!(store.read(), Err(StoreError::Corrupt(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn locked_guard_supports_read_write_and_clear() {
        let dir = unique_test_dir("guard");
        let store = OwnershipStore::at(dir.join("ownership.json"));
        let expected = OwnershipState::new(true, "boot-b", 4);
        let guard = store.lock().unwrap();

        guard.write(&expected).unwrap();
        assert_eq!(guard.read().unwrap(), Some(expected));
        guard.clear().unwrap();
        assert_eq!(guard.read().unwrap(), None);

        drop(guard);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exclusive_lock_has_a_nonblocking_probe() {
        let dir = unique_test_dir("exclusive-lock");
        let path = dir.join("ownership.json");
        let owner = OwnershipStore::at(&path);
        let contender = OwnershipStore::at(&path);
        let owner_guard = owner.lock().unwrap();

        assert!(contender.try_lock().unwrap().is_none());

        drop(owner_guard);
        assert!(contender.try_lock().unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn held_lock_times_out_without_modifying_registry() {
        let dir = unique_test_dir("lock-timeout");
        let path = dir.join("ownership.json");
        let owner = OwnershipStore::at(&path);
        let contender = OwnershipStore::at(&path);
        let expected = OwnershipState::new(false, "boot-a", 7);
        owner.write(&expected).unwrap();
        let owner_guard = owner.lock().unwrap();

        assert!(matches!(
            contender.lock_with_timeout(Duration::ZERO),
            Err(StoreError::LockTimeout { .. })
        ));
        assert_eq!(owner_guard.read().unwrap(), Some(expected));

        drop(owner_guard);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
