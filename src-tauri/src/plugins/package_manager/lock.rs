use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::paths::PluginPaths;
use super::StorageError;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub struct ManagerLock {
    file: File,
}

impl ManagerLock {
    pub fn acquire(paths: &PluginPaths) -> Result<Self, StorageError> {
        Self::acquire_with_timeout(paths, DEFAULT_TIMEOUT)
    }

    pub fn acquire_with_timeout(
        paths: &PluginPaths,
        requested_timeout: Duration,
    ) -> Result<Self, StorageError> {
        paths.prepare()?;
        let path = paths.manager_lock();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(StorageError::new(
                    "manager_lock_type",
                    format!("{} is not a regular lock file", path.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(
                    "manager_lock_io",
                    format!("cannot inspect {}: {error}", path.display()),
                ));
            }
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                StorageError::new(
                    "manager_lock_io",
                    format!("cannot open {}: {error}", path.display()),
                )
            })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            StorageError::new(
                "manager_lock_permissions",
                format!("cannot protect {}: {error}", path.display()),
            )
        })?;

        let timeout = requested_timeout.min(DEFAULT_TIMEOUT);
        let started = Instant::now();
        loop {
            match try_lock(&file) {
                Ok(true) => break,
                Ok(false) if started.elapsed() >= timeout => {
                    return Err(StorageError::new(
                        "manager_lock_busy",
                        format!(
                            "{} remained locked for {} ms",
                            path.display(),
                            timeout.as_millis()
                        ),
                    ));
                }
                Ok(false) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    thread::sleep(POLL_INTERVAL.min(remaining));
                }
                Err(error) => {
                    return Err(StorageError::new(
                        "manager_lock_io",
                        format!("cannot lock {}: {error}", path.display()),
                    ));
                }
            }
        }

        let record = LockOwnerRecord {
            pid: std::process::id(),
            process_start_identity: current_process_start_identity()?,
        };
        let bytes = serde_json_canonicalizer::to_vec(&record).map_err(|error| {
            StorageError::new(
                "manager_lock_record",
                format!("cannot serialize lock owner: {error}"),
            )
        })?;
        file.set_len(0).map_err(|error| {
            StorageError::new(
                "manager_lock_record",
                format!("cannot truncate {}: {error}", path.display()),
            )
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            StorageError::new(
                "manager_lock_record",
                format!("cannot seek {}: {error}", path.display()),
            )
        })?;
        file.write_all(&bytes).map_err(|error| {
            StorageError::new(
                "manager_lock_record",
                format!("cannot write {}: {error}", path.display()),
            )
        })?;
        file.sync_all().map_err(|error| {
            StorageError::new(
                "manager_lock_record",
                format!("cannot sync {}: {error}", path.display()),
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for ManagerLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockOwnerRecord {
    pid: u32,
    process_start_identity: String,
}

fn try_lock(file: &File) -> io::Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(target_os = "macos")]
fn current_process_start_identity() -> Result<String, StorageError> {
    let pid = i32::try_from(std::process::id())
        .map_err(|_| StorageError::new("manager_lock_identity", "PID exceeds Darwin pid_t"))?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let expected = std::mem::size_of::<libc::proc_bsdinfo>();
    let received = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            i32::try_from(expected).expect("proc_bsdinfo fits c_int"),
        )
    };
    if received < 0 || usize::try_from(received).ok() != Some(expected) {
        return Err(StorageError::new(
            "manager_lock_identity",
            format!(
                "cannot read process start identity for PID {pid}: {}",
                io::Error::last_os_error()
            ),
        ));
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != u32::try_from(pid).unwrap_or_default()
        || info.pbi_start_tvsec == 0
        || info.pbi_start_tvusec >= 1_000_000
    {
        return Err(StorageError::new(
            "manager_lock_identity",
            format!("invalid process start identity for PID {pid}"),
        ));
    }
    Ok(format!(
        "uid={}:start={}.{}",
        info.pbi_uid, info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(target_os = "linux")]
fn current_process_start_identity() -> Result<String, StorageError> {
    let stat = fs::read_to_string("/proc/self/stat").map_err(|error| {
        StorageError::new(
            "manager_lock_identity",
            format!("cannot read /proc/self/stat: {error}"),
        )
    })?;
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields.split_whitespace().collect::<Vec<_>>())
        .ok_or_else(|| StorageError::new("manager_lock_identity", "invalid /proc/self/stat"))?;
    let start_ticks = fields
        .get(19)
        .ok_or_else(|| StorageError::new("manager_lock_identity", "missing process start time"))?;
    Ok(format!(
        "pid={}:start-ticks={start_ticks}",
        std::process::id()
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn current_process_start_identity() -> Result<String, StorageError> {
    Ok(format!(
        "pid={}:acquired-at-ms={}",
        std::process::id(),
        crate::util::now_ms()
    ))
}

#[cfg(test)]
mod tests {
    use super::ManagerLock;
    use crate::plugins::package_manager::paths::PluginPaths;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn fixture_paths() -> PluginPaths {
        let root = std::env::temp_dir().join(format!(
            "jarvis-plugin-manager-lock-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let paths = PluginPaths::new(root.join("profile"));
        paths.prepare().unwrap();
        paths
    }

    #[test]
    fn contended_lock_times_out_without_deleting_the_owner_record() {
        let paths = fixture_paths();
        let first = ManagerLock::acquire_with_timeout(&paths, Duration::from_millis(50)).unwrap();
        let owner_record = fs::read_to_string(paths.manager_lock()).unwrap();

        assert!(owner_record.contains(&format!("\"pid\":{}", std::process::id())));
        assert!(owner_record.contains("\"processStartIdentity\":"));
        assert_eq!(
            ManagerLock::acquire_with_timeout(&paths, Duration::from_millis(30))
                .unwrap_err()
                .code(),
            "manager_lock_busy"
        );
        assert_eq!(
            fs::read_to_string(paths.manager_lock()).unwrap(),
            owner_record
        );
        assert_eq!(
            fs::metadata(paths.manager_lock())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        drop(first);
        ManagerLock::acquire_with_timeout(&paths, Duration::from_millis(50)).unwrap();
    }
}
