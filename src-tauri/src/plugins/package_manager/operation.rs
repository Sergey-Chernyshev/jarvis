use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Arc;

use super::paths::{ensure_real_directory, open_real_directory};
use super::secure_fs;
use super::{random_storage_id, StorageError};
use jarvis_plugin_protocol::operation::Operation;
pub use jarvis_plugin_protocol::operation::OperationState;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFailure {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct OperationJournalFailpoints {
    protection_before_mutation: AtomicBool,
}

#[cfg(test)]
impl OperationJournalFailpoints {
    fn fail_next_protection(&self) {
        self.protection_before_mutation
            .store(true, Ordering::SeqCst);
    }

    fn take_protection_failure(&self) -> bool {
        self.protection_before_mutation
            .swap(false, Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct OperationJournal {
    path: PathBuf,
    parent: File,
    connection: Mutex<Connection>,
    #[cfg(test)]
    failpoints: Arc<OperationJournalFailpoints>,
}

impl OperationJournal {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        Self::open_internal(
            path.into(),
            Option::<fn(&Path)>::None,
            #[cfg(test)]
            Arc::new(OperationJournalFailpoints::default()),
        )
    }

    #[cfg(test)]
    fn open_after_inspect(
        path: impl Into<PathBuf>,
        after_inspect: impl FnOnce(&Path),
    ) -> Result<Self, StorageError> {
        Self::open_internal(
            path.into(),
            Some(after_inspect),
            Arc::new(OperationJournalFailpoints::default()),
        )
    }

    #[cfg(test)]
    fn open_with_failpoints(
        path: impl Into<PathBuf>,
        failpoints: Arc<OperationJournalFailpoints>,
    ) -> Result<Self, StorageError> {
        Self::open_internal(path.into(), Option::<fn(&Path)>::None, failpoints)
    }

    fn open_internal<F>(
        path: PathBuf,
        after_inspect: Option<F>,
        #[cfg(test)] failpoints: Arc<OperationJournalFailpoints>,
    ) -> Result<Self, StorageError>
    where
        F: FnOnce(&Path),
    {
        let (parent, main_file) = prepare_database_path(&path, after_inspect)?;
        if rusqlite::version_number() < 3_031_000 {
            return Err(StorageError::new(
                "operation_db_nofollow",
                format!(
                    "SQLite {} does not support SQLITE_OPEN_NOFOLLOW",
                    rusqlite::version()
                ),
            ));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&path, flags).map_err(database_error)?;
        let parent_path = path.parent().ok_or_else(|| {
            StorageError::new(
                "operation_db_path",
                format!("{} has no parent", path.display()),
            )
        })?;
        let reopened_parent = open_real_directory(parent_path)?;
        let held_parent_metadata = secure_fs::metadata(&parent).map_err(|error| {
            StorageError::new(
                "operation_db_path",
                format!("cannot inspect held {}: {error}", parent_path.display()),
            )
        })?;
        let reopened_parent_metadata = secure_fs::metadata(&reopened_parent).map_err(|error| {
            StorageError::new(
                "operation_db_path",
                format!("cannot inspect reopened {}: {error}", parent_path.display()),
            )
        })?;
        if !secure_fs::same_identity(&held_parent_metadata, &reopened_parent_metadata) {
            return Err(StorageError::new(
                "operation_db_path",
                format!("{} changed before SQLite opened", parent_path.display()),
            ));
        }
        let main_name = path.file_name().ok_or_else(|| {
            StorageError::new(
                "operation_db_path",
                format!("{} has no database filename", path.display()),
            )
        })?;
        let main_name = CString::new(main_name.as_bytes()).map_err(|_| {
            StorageError::new(
                "operation_db_path",
                format!("{} contains NUL", path.display()),
            )
        })?;
        let main_metadata = regular_file_metadata(&main_file, &path)?;
        verify_anchored_file(&parent, &main_name, &main_metadata, &path)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(database_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(database_error)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(database_error)?;
        {
            let transaction = connection.transaction().map_err(database_error)?;
            transaction
                .execute_batch(include_str!("schema.sql"))
                .map_err(database_error)?;
            transaction.commit().map_err(database_error)?;
        }
        protect_database_files(&parent, &path)?;
        Ok(Self {
            path,
            parent,
            connection: Mutex::new(connection),
            #[cfg(test)]
            failpoints,
        })
    }

    pub fn begin(&self, kind: &str, plugin_id: &str) -> Result<String, StorageError> {
        let id = random_storage_id()?;
        self.insert(
            &id,
            kind,
            plugin_id,
            OperationState::Queued,
            "queued",
            &serde_json::json!({}),
            None,
        )?;
        Ok(id)
    }

    pub fn transition(
        &self,
        id: &str,
        next: OperationState,
        phase: &str,
        failure: Option<OperationFailure>,
    ) -> Result<(), StorageError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| StorageError::new("operation_db", "operation database lock poisoned"))?;
        let transaction = connection.transaction().map_err(database_error)?;
        let current = transaction
            .query_row("SELECT state FROM operations WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(database_error)?
            .ok_or_else(|| StorageError::new("operation_not_found", id))?;
        let current = parse_state(&current)?;
        if is_terminal(&current) {
            return Err(StorageError::new(
                "operation_terminal",
                format!("operation {id} is terminal"),
            ));
        }
        if !legal_transition(&current, &next) {
            return Err(StorageError::new(
                "operation_transition",
                format!(
                    "illegal transition {} -> {}",
                    state_name(&current),
                    state_name(&next)
                ),
            ));
        }
        self.protect_before_mutation()?;
        let (error_code, error_message) = failure
            .map(|failure| (Some(failure.code), Some(failure.message)))
            .unwrap_or((None, None));
        transaction
            .execute(
                "UPDATE operations
                 SET state = ?2, phase = ?3, error_code = ?4, error_message = ?5,
                     updated_at_ms = ?6
                 WHERE id = ?1",
                params![
                    id,
                    state_name(&next),
                    phase,
                    error_code,
                    error_message,
                    crate::util::now_ms()
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(())
    }

    pub fn recoverable(&self) -> Result<Vec<Operation>, StorageError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| StorageError::new("operation_db", "operation database lock poisoned"))?;
        let mut statement = connection
            .prepare(
                "SELECT id, kind, plugin_id, state, phase, created_at_ms, updated_at_ms,
                        error_code, error_message
                 FROM operations
                 WHERE state IN ('queued', 'running', 'waiting-for-consent')
                 ORDER BY id ASC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                let state: String = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    state,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(database_error)?;
        let mut operations = Vec::new();
        for row in rows {
            let (
                id,
                kind,
                plugin_id,
                state,
                phase,
                created_at_ms,
                updated_at_ms,
                error_code,
                error_message,
            ) = row.map_err(database_error)?;
            operations.push(Operation {
                id,
                kind,
                plugin_id,
                state: parse_state(&state)?,
                phase,
                created_at_ms,
                updated_at_ms,
                error_code,
                error_message,
            });
        }
        Ok(operations)
    }

    fn insert(
        &self,
        id: &str,
        kind: &str,
        plugin_id: &str,
        state: OperationState,
        phase: &str,
        payload: &Value,
        failure: Option<OperationFailure>,
    ) -> Result<(), StorageError> {
        if kind.is_empty() || plugin_id.is_empty() || phase.is_empty() {
            return Err(StorageError::new(
                "operation_schema",
                "kind, plugin_id and phase are required",
            ));
        }
        let payload_json = serde_json_canonicalizer::to_string(payload).map_err(|error| {
            StorageError::new(
                "operation_payload",
                format!("cannot serialize operation payload: {error}"),
            )
        })?;
        let (error_code, error_message) = failure
            .map(|failure| (Some(failure.code), Some(failure.message)))
            .unwrap_or((None, None));
        let now = crate::util::now_ms();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| StorageError::new("operation_db", "operation database lock poisoned"))?;
        self.protect_before_mutation()?;
        let transaction = connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO operations (
                    id, kind, plugin_id, state, phase, payload_json,
                    error_code, error_message, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    id,
                    kind,
                    plugin_id,
                    state_name(&state),
                    phase,
                    payload_json,
                    error_code,
                    error_message,
                    now
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(())
    }

    fn protect_before_mutation(&self) -> Result<(), StorageError> {
        #[cfg(test)]
        if self.failpoints.take_protection_failure() {
            return Err(StorageError::new(
                "operation_db_permissions",
                "injected database protection failure before mutation",
            ));
        }
        protect_database_files(&self.parent, &self.path)
    }

    #[cfg(test)]
    fn journal_mode(&self) -> Result<String, StorageError> {
        self.pragma_string("journal_mode")
    }

    #[cfg(test)]
    fn foreign_keys_enabled(&self) -> Result<bool, StorageError> {
        Ok(self.pragma_i64("foreign_keys")? == 1)
    }

    #[cfg(test)]
    fn busy_timeout_ms(&self) -> Result<i64, StorageError> {
        self.pragma_i64("busy_timeout")
    }

    #[cfg(test)]
    fn pragma_string(&self, name: &str) -> Result<String, StorageError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| StorageError::new("operation_db", "operation database lock poisoned"))?;
        connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(database_error)
    }

    #[cfg(test)]
    fn pragma_i64(&self, name: &str) -> Result<i64, StorageError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| StorageError::new("operation_db", "operation database lock poisoned"))?;
        connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(database_error)
    }
}

fn prepare_database_path<F>(
    path: &Path,
    after_inspect: Option<F>,
) -> Result<(File, File), StorageError>
where
    F: FnOnce(&Path),
{
    let parent_path = path.parent().ok_or_else(|| {
        StorageError::new(
            "operation_db_path",
            format!("{} has no parent", path.display()),
        )
    })?;
    let leaf = path.file_name().ok_or_else(|| {
        StorageError::new(
            "operation_db_path",
            format!("{} has no database filename", path.display()),
        )
    })?;
    let leaf = CString::new(leaf.as_bytes()).map_err(|_| {
        StorageError::new(
            "operation_db_path",
            format!("{} contains NUL", path.display()),
        )
    })?;
    ensure_real_directory(parent_path, 0o700)?;
    let parent = open_real_directory(parent_path)?;
    match secure_fs::entry_metadata(&parent, &leaf) {
        Ok(inspected) => {
            if !secure_fs::is_type(&inspected, libc::S_IFREG) {
                return Err(StorageError::new(
                    "operation_db_type",
                    format!("{} is not a regular database file", path.display()),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StorageError::new(
                "operation_db_path",
                format!("cannot inspect {}: {error}", path.display()),
            ));
        }
    }
    if let Some(after_inspect) = after_inspect {
        after_inspect(path);
    }
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        return Err(StorageError::new(
            if matches!(
                error.raw_os_error(),
                Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ) {
                "operation_db_type"
            } else {
                "operation_db_create"
            },
            format!("cannot open {}: {error}", path.display()),
        ));
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let opened = regular_file_metadata(&file, path)?;
    if let Err(error) = secure_fs::chmod(&file, 0o600) {
        return Err(StorageError::new(
            "operation_db_permissions",
            format!("cannot protect {}: {error}", path.display()),
        ));
    }
    verify_anchored_file(&parent, &leaf, &opened, path)?;
    Ok((parent, file))
}

fn protect_database_files(parent: &File, path: &Path) -> Result<(), StorageError> {
    let base = path.file_name().ok_or_else(|| {
        StorageError::new(
            "operation_db_path",
            format!("{} has no database filename", path.display()),
        )
    })?;
    let mut wal = base.to_os_string();
    wal.push("-wal");
    let mut shm = base.to_os_string();
    shm.push("-shm");
    for (candidate, required) in [(base.to_os_string(), true), (wal, false), (shm, false)] {
        let name = CString::new(candidate.as_bytes()).map_err(|_| {
            StorageError::new("operation_db_path", "database filename contains NUL")
        })?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            if !required && error.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            return Err(StorageError::new(
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ELOOP) | Some(libc::ENOTDIR)
                ) {
                    "operation_db_type"
                } else {
                    "operation_db_permissions"
                },
                format!(
                    "cannot open {}: {error}",
                    path.parent()
                        .unwrap_or_else(|| Path::new(""))
                        .join(&candidate)
                        .display()
                ),
            ));
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let candidate_path = path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(&candidate);
        let opened = regular_file_metadata(&file, &candidate_path)?;
        if let Err(error) = secure_fs::chmod(&file, 0o600) {
            return Err(StorageError::new(
                "operation_db_permissions",
                format!("cannot protect {}: {error}", candidate_path.display()),
            ));
        }
        verify_anchored_file(parent, &name, &opened, &candidate_path)?;
    }
    Ok(())
}

fn regular_file_metadata(file: &File, path: &Path) -> Result<libc::stat, StorageError> {
    let metadata = secure_fs::metadata(file).map_err(|error| {
        StorageError::new(
            "operation_db_path",
            format!("cannot inspect opened {}: {error}", path.display()),
        )
    })?;
    if !secure_fs::is_type(&metadata, libc::S_IFREG) {
        return Err(StorageError::new(
            "operation_db_type",
            format!("{} is not a regular database file", path.display()),
        ));
    }
    Ok(metadata)
}

fn verify_anchored_file(
    parent: &File,
    name: &CString,
    opened: &libc::stat,
    path: &Path,
) -> Result<(), StorageError> {
    let anchored = secure_fs::entry_metadata(parent, name).map_err(|error| {
        StorageError::new(
            "operation_db_path",
            format!("cannot recheck {}: {error}", path.display()),
        )
    })?;
    if !secure_fs::is_type(&anchored, libc::S_IFREG) || !secure_fs::same_identity(&anchored, opened)
    {
        return Err(StorageError::new(
            "operation_db_type",
            format!("{} changed while it was opened", path.display()),
        ));
    }
    Ok(())
}

fn database_error(error: rusqlite::Error) -> StorageError {
    StorageError::new("operation_db", error.to_string())
}

fn state_name(state: &OperationState) -> &'static str {
    match state {
        OperationState::Queued => "queued",
        OperationState::Running => "running",
        OperationState::WaitingForConsent => "waiting-for-consent",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::Cancelled => "cancelled",
    }
}

fn parse_state(value: &str) -> Result<OperationState, StorageError> {
    match value {
        "queued" => Ok(OperationState::Queued),
        "running" => Ok(OperationState::Running),
        "waiting-for-consent" => Ok(OperationState::WaitingForConsent),
        "succeeded" => Ok(OperationState::Succeeded),
        "failed" => Ok(OperationState::Failed),
        "cancelled" => Ok(OperationState::Cancelled),
        _ => Err(StorageError::new(
            "operation_state",
            format!("unknown operation state {value}"),
        )),
    }
}

fn is_terminal(state: &OperationState) -> bool {
    matches!(
        state,
        OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
    )
}

fn legal_transition(current: &OperationState, next: &OperationState) -> bool {
    matches!(
        (current, next),
        (OperationState::Queued, OperationState::Running)
            | (OperationState::Queued, OperationState::WaitingForConsent)
            | (OperationState::Queued, OperationState::Failed)
            | (OperationState::Queued, OperationState::Cancelled)
            | (OperationState::Running, OperationState::WaitingForConsent)
            | (OperationState::Running, OperationState::Succeeded)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Cancelled)
            | (OperationState::WaitingForConsent, OperationState::Running)
            | (OperationState::WaitingForConsent, OperationState::Failed)
            | (OperationState::WaitingForConsent, OperationState::Cancelled)
    )
}

#[cfg(test)]
mod tests {
    use super::{OperationJournal, OperationJournalFailpoints, OperationState};
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn journal_path(label: &str) -> PathBuf {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "jarvis-plugin-operations-{label}-{}-{}",
                std::process::id(),
                NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.join("operations.sqlite3")
    }

    fn fixture_journal() -> OperationJournal {
        OperationJournal::open(journal_path("fixture")).unwrap()
    }

    fn seed_operation(journal: &OperationJournal, id: &str, state: OperationState, phase: &str) {
        journal
            .insert(
                id,
                "install",
                "dev.example.echo",
                state,
                phase,
                &json!({}),
                None,
            )
            .unwrap();
    }

    #[test]
    fn operation_transitions_are_durable_and_terminal_is_final() {
        let journal = fixture_journal();
        let id = journal.begin("install", "dev.example.echo").unwrap();
        journal
            .transition(&id, OperationState::Running, "verify", None)
            .unwrap();
        journal
            .transition(&id, OperationState::Succeeded, "complete", None)
            .unwrap();

        assert_eq!(
            journal
                .transition(&id, OperationState::Running, "retry", None)
                .unwrap_err()
                .code(),
            "operation_terminal"
        );
    }

    #[test]
    fn restart_lists_only_recoverable_non_terminal_operations() {
        let path = journal_path("restart");
        {
            let journal = OperationJournal::open(&path).unwrap();
            seed_operation(&journal, "op-running", OperationState::Running, "extract");
            seed_operation(
                &journal,
                "op-consent",
                OperationState::WaitingForConsent,
                "consent",
            );
            seed_operation(&journal, "op-done", OperationState::Succeeded, "complete");
        }
        let reopened = OperationJournal::open(&path).unwrap();

        assert_eq!(
            reopened
                .recoverable()
                .unwrap()
                .iter()
                .map(|operation| operation.id.as_str())
                .collect::<Vec<_>>(),
            ["op-consent", "op-running"]
        );
    }

    #[test]
    fn journal_uses_required_pragmas_and_owner_only_files() {
        let path = journal_path("pragmas");
        let journal = OperationJournal::open(&path).unwrap();
        journal.begin("install", "dev.example.echo").unwrap();

        assert_eq!(journal.journal_mode().unwrap(), "wal");
        assert!(journal.foreign_keys_enabled().unwrap());
        assert_eq!(journal.busy_timeout_ms().unwrap(), 5_000);
        for file in [
            path.clone(),
            PathBuf::from(format!("{}-wal", path.display())),
            PathBuf::from(format!("{}-shm", path.display())),
        ] {
            if file.exists() {
                assert_eq!(
                    fs::metadata(file).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn database_swap_to_symlink_is_rejected_before_sqlite_open() {
        let path = journal_path("swap");
        let original = path.with_extension("original");
        let outside = path.parent().unwrap().join("outside-database-target");
        fs::write(&path, []).unwrap();
        fs::write(&outside, b"outside-must-not-change").unwrap();

        let error = OperationJournal::open_after_inspect(&path, |inspected| {
            assert_eq!(inspected, path);
            fs::rename(&path, &original).unwrap();
            symlink(&outside, &path).unwrap();
        })
        .unwrap_err();

        assert_eq!(error.code(), "operation_db_type");
        assert_eq!(fs::read(&outside).unwrap(), b"outside-must-not-change");
        assert_eq!(fs::read(&original).unwrap(), b"");
    }

    #[test]
    fn database_parent_swap_to_real_decoy_is_rejected_before_schema_mutation() {
        let path = journal_path("parent-swap");
        let parent = path.parent().unwrap().to_path_buf();
        let original_parent = parent.with_extension("original");
        fs::write(&path, []).unwrap();

        let error = OperationJournal::open_after_inspect(&path, |_| {
            fs::rename(&parent, &original_parent).unwrap();
            fs::create_dir(&parent).unwrap();
            fs::write(&path, []).unwrap();
        })
        .unwrap_err();

        assert_eq!(error.code(), "operation_db_path");
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(
            fs::read(original_parent.join("operations.sqlite3")).unwrap(),
            b""
        );
    }

    #[test]
    fn begin_protection_failure_leaves_no_durable_insert() {
        let path = journal_path("begin-protection-failure");
        let failpoints = Arc::new(OperationJournalFailpoints::default());
        let journal =
            OperationJournal::open_with_failpoints(&path, Arc::clone(&failpoints)).unwrap();
        seed_operation(
            &journal,
            "existing-operation",
            OperationState::Running,
            "extract",
        );
        let before = journal.recoverable().unwrap();

        failpoints.fail_next_protection();
        let error = journal.begin("install", "dev.example.new").unwrap_err();
        assert_eq!(error.code(), "operation_db_permissions");
        drop(journal);

        let reopened = OperationJournal::open(&path).unwrap();
        assert_eq!(reopened.recoverable().unwrap(), before);
    }

    #[test]
    fn transition_protection_failure_leaves_full_row_unchanged() {
        let path = journal_path("transition-protection-failure");
        let failpoints = Arc::new(OperationJournalFailpoints::default());
        let journal =
            OperationJournal::open_with_failpoints(&path, Arc::clone(&failpoints)).unwrap();
        seed_operation(
            &journal,
            "operation-to-transition",
            OperationState::Queued,
            "queued",
        );
        seed_operation(
            &journal,
            "existing-operation",
            OperationState::Running,
            "download",
        );
        let before = journal.recoverable().unwrap();

        failpoints.fail_next_protection();
        let error = journal
            .transition(
                "operation-to-transition",
                OperationState::Running,
                "verify",
                None,
            )
            .unwrap_err();
        assert_eq!(error.code(), "operation_db_permissions");
        drop(journal);

        let reopened = OperationJournal::open(&path).unwrap();
        assert_eq!(reopened.recoverable().unwrap(), before);
    }
}
