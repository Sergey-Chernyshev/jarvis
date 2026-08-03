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
use rusqlite::{Connection, OpenFlags, OptionalExtension, ToSql};
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
            None,
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
            None,
            Arc::new(OperationJournalFailpoints::default()),
        )
    }

    #[cfg(test)]
    fn open_after_journal_mode(
        path: impl Into<PathBuf>,
        after_journal_mode: fn(&Path),
    ) -> Result<Self, StorageError> {
        Self::open_internal(
            path.into(),
            Option::<fn(&Path)>::None,
            Some(after_journal_mode),
            Arc::new(OperationJournalFailpoints::default()),
        )
    }

    #[cfg(test)]
    fn open_with_failpoints(
        path: impl Into<PathBuf>,
        failpoints: Arc<OperationJournalFailpoints>,
    ) -> Result<Self, StorageError> {
        Self::open_internal(path.into(), Option::<fn(&Path)>::None, None, failpoints)
    }

    fn open_internal<F>(
        path: PathBuf,
        after_inspect: Option<F>,
        after_journal_mode: Option<fn(&Path)>,
        #[cfg(test)] failpoints: Arc<OperationJournalFailpoints>,
    ) -> Result<Self, StorageError>
    where
        F: FnOnce(&Path),
    {
        let (parent, main_file, preflight_sidecars) = prepare_database_path(&path, after_inspect)?;
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
        if let Some(after_journal_mode) = after_journal_mode {
            after_journal_mode(&path);
        }
        protect_database_files(&parent, &path)?;
        drop(preflight_sidecars);
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
        let next_state = state_name(&next);
        let updated_at_ms = crate::util::now_ms();
        let parameters: [&dyn ToSql; 6] = [
            &id,
            &next_state,
            &phase,
            &error_code,
            &error_message,
            &updated_at_ms,
        ];
        transaction
            .execute(
                "UPDATE operations
                 SET state = ?2, phase = ?3, error_code = ?4, error_message = ?5,
                     updated_at_ms = ?6
                 WHERE id = ?1",
                parameters,
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
        let operation_state = state_name(&state);
        let parameters: [&dyn ToSql; 9] = [
            &id,
            &kind,
            &plugin_id,
            &operation_state,
            &phase,
            &payload_json,
            &error_code,
            &error_message,
            &now,
        ];
        transaction
            .execute(
                "INSERT INTO operations (
                    id, kind, plugin_id, state, phase, payload_json,
                    error_code, error_message, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                parameters,
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
) -> Result<(File, File, Vec<File>), StorageError>
where
    F: FnOnce(&Path),
{
    let parent_path = path.parent().ok_or_else(|| {
        StorageError::new(
            "operation_db_path",
            format!("{} has no parent", path.display()),
        )
    })?;
    ensure_real_directory(parent_path, 0o700)?;
    let parent = open_real_directory(parent_path)?;
    let specs = database_candidate_specs(path)?;
    let inspected = specs
        .iter()
        .map(|spec| inspect_database_candidate(&parent, spec))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(after_inspect) = after_inspect {
        after_inspect(path);
    }
    let mut opened = Vec::with_capacity(specs.len());
    for (spec, inspected) in specs.into_iter().zip(inspected) {
        let file = match inspected {
            Some(inspected) => open_inspected_database_candidate(&parent, &spec, &inspected)?,
            None if spec.required => create_database_candidate(&parent, &spec)?,
            None => continue,
        };
        opened.push((spec, file));
    }
    protect_opened_database_candidates(&parent, &opened)?;

    let main_index = opened
        .iter()
        .position(|(spec, _)| spec.required)
        .ok_or_else(|| StorageError::new("operation_db_create", "database file was not opened"))?;
    let (_, main) = opened.swap_remove(main_index);
    let sidecars = opened.into_iter().map(|(_, file)| file).collect();
    Ok((parent, main, sidecars))
}

fn protect_database_files(parent: &File, path: &Path) -> Result<(), StorageError> {
    let specs = database_candidate_specs(path)?;
    let inspected = specs
        .iter()
        .map(|spec| inspect_database_candidate(parent, spec))
        .collect::<Result<Vec<_>, _>>()?;
    let mut opened = Vec::with_capacity(specs.len());
    for (spec, inspected) in specs.into_iter().zip(inspected) {
        match inspected {
            Some(inspected) => {
                let file = open_inspected_database_candidate(parent, &spec, &inspected)?;
                opened.push((spec, file));
            }
            None if spec.required => {
                return Err(StorageError::new(
                    "operation_db_path",
                    format!("{} disappeared", spec.path.display()),
                ));
            }
            None => {}
        }
    }
    protect_opened_database_candidates(parent, &opened)
}

struct DatabaseCandidateSpec {
    name: CString,
    path: PathBuf,
    required: bool,
}

fn database_candidate_specs(path: &Path) -> Result<Vec<DatabaseCandidateSpec>, StorageError> {
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
    [(base.to_os_string(), true), (wal, false), (shm, false)]
        .into_iter()
        .map(|(candidate, required)| {
            let candidate_path = path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(&candidate);
            let name = CString::new(candidate.as_bytes()).map_err(|_| {
                StorageError::new("operation_db_path", "database filename contains NUL")
            })?;
            Ok(DatabaseCandidateSpec {
                name,
                path: candidate_path,
                required,
            })
        })
        .collect()
}

fn inspect_database_candidate(
    parent: &File,
    spec: &DatabaseCandidateSpec,
) -> Result<Option<libc::stat>, StorageError> {
    match secure_fs::entry_metadata(parent, &spec.name) {
        Ok(metadata) => {
            validate_database_metadata(&metadata, &spec.path)?;
            Ok(Some(metadata))
        }
        Err(error) if !spec.required && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if spec.required && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::new(
            "operation_db_path",
            format!("cannot inspect {}: {error}", spec.path.display()),
        )),
    }
}

fn open_inspected_database_candidate(
    parent: &File,
    spec: &DatabaseCandidateSpec,
    inspected: &libc::stat,
) -> Result<File, StorageError> {
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            spec.name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(open_database_candidate_error(
            &spec.path,
            std::io::Error::last_os_error(),
        ));
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let opened = regular_file_metadata(&file, &spec.path)?;
    if !secure_fs::same_identity(inspected, &opened) {
        return Err(StorageError::new(
            "operation_db_type",
            format!("{} changed while it was opened", spec.path.display()),
        ));
    }
    verify_anchored_file(parent, &spec.name, &opened, &spec.path)?;
    Ok(file)
}

fn create_database_candidate(
    parent: &File,
    spec: &DatabaseCandidateSpec,
) -> Result<File, StorageError> {
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            spec.name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(open_database_candidate_error(
            &spec.path,
            std::io::Error::last_os_error(),
        ));
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let opened = regular_file_metadata(&file, &spec.path)?;
    verify_anchored_file(parent, &spec.name, &opened, &spec.path)?;
    Ok(file)
}

fn open_database_candidate_error(path: &Path, error: std::io::Error) -> StorageError {
    StorageError::new(
        if matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        ) {
            "operation_db_type"
        } else {
            "operation_db_create"
        },
        format!("cannot open {}: {error}", path.display()),
    )
}

fn protect_opened_database_candidates(
    parent: &File,
    opened: &[(DatabaseCandidateSpec, File)],
) -> Result<(), StorageError> {
    for (spec, file) in opened {
        if let Err(error) = secure_fs::chmod(file, 0o600) {
            return Err(StorageError::new(
                "operation_db_permissions",
                format!("cannot protect {}: {error}", spec.path.display()),
            ));
        }
    }
    for (spec, file) in opened {
        let protected = regular_file_metadata(file, &spec.path)?;
        if protected.st_mode & 0o777 != 0o600 {
            return Err(StorageError::new(
                "operation_db_permissions",
                format!("{} is not mode 0600", spec.path.display()),
            ));
        }
        verify_anchored_file(parent, &spec.name, &protected, &spec.path)?;
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
    validate_database_metadata(&metadata, path)?;
    Ok(metadata)
}

fn validate_database_metadata(metadata: &libc::stat, path: &Path) -> Result<(), StorageError> {
    if !secure_fs::is_type(metadata, libc::S_IFREG) {
        return Err(StorageError::new(
            "operation_db_type",
            format!("{} is not a regular database file", path.display()),
        ));
    }
    if metadata.st_uid != unsafe { libc::geteuid() } {
        return Err(StorageError::new(
            "operation_db_owner",
            format!("{} is not owned by the effective user", path.display()),
        ));
    }
    if metadata.st_nlink != 1 {
        return Err(StorageError::new(
            "operation_db_links",
            format!("{} must have exactly one hard link", path.display()),
        ));
    }
    if metadata.st_mode & 0o077 != 0 {
        return Err(StorageError::new(
            "operation_db_permissions",
            format!("{} grants group or other access", path.display()),
        ));
    }
    Ok(())
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
    use super::{
        validate_database_metadata, OperationJournal, OperationJournalFailpoints, OperationState,
    };
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

    fn write_owner_only(path: &std::path::Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn sidecar_path(path: &std::path::Path, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}{suffix}", path.display()))
    }

    fn inject_sidecar_symlink(path: &std::path::Path, suffix: &str) {
        let candidate = sidecar_path(path, suffix);
        if fs::symlink_metadata(&candidate).is_ok() {
            fs::remove_file(&candidate).unwrap();
        }
        let outside = path
            .parent()
            .unwrap()
            .join(format!("outside-post-journal-{}", &suffix[1..]));
        write_owner_only(&outside, b"post-journal-target-must-not-change");
        symlink(outside, candidate).unwrap();
    }

    fn inject_wal_symlink(path: &std::path::Path) {
        inject_sidecar_symlink(path, "-wal");
    }

    fn inject_shm_symlink(path: &std::path::Path) {
        inject_sidecar_symlink(path, "-shm");
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
        write_owner_only(&path, b"");
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
    fn database_swap_to_regular_decoy_is_rejected_before_sqlite_open() {
        let path = journal_path("regular-decoy-swap");
        let original = path.with_extension("original");
        write_owner_only(&path, b"");

        let error = OperationJournal::open_after_inspect(&path, |_| {
            fs::rename(&path, &original).unwrap();
            write_owner_only(&path, b"");
        })
        .unwrap_err();

        assert_eq!(error.code(), "operation_db_type");
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(fs::read(&original).unwrap(), b"");
    }

    #[test]
    fn database_parent_swap_to_real_decoy_is_rejected_before_schema_mutation() {
        let path = journal_path("parent-swap");
        let parent = path.parent().unwrap().to_path_buf();
        let original_parent = parent.with_extension("original");
        write_owner_only(&path, b"");

        let error = OperationJournal::open_after_inspect(&path, |_| {
            fs::rename(&parent, &original_parent).unwrap();
            fs::create_dir(&parent).unwrap();
            write_owner_only(&path, b"");
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
    fn database_with_group_or_other_access_is_rejected_before_chmod() {
        let path = journal_path("database-open-permissions");
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

        let error = OperationJournal::open(&path).unwrap_err();

        assert_eq!(error.code(), "operation_db_permissions");
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
    }

    #[test]
    fn database_metadata_owned_by_another_euid_is_rejected() {
        let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
        metadata.st_mode = (libc::S_IFREG | 0o600) as _;
        metadata.st_uid = unsafe { libc::geteuid() }.wrapping_add(1);
        metadata.st_nlink = 1;

        let error =
            validate_database_metadata(&metadata, std::path::Path::new("operations.sqlite3"))
                .unwrap_err();

        assert_eq!(error.code(), "operation_db_owner");
    }

    #[test]
    fn hardlinked_database_is_rejected_without_mutating_external_target() {
        let path = journal_path("database-hardlink");
        let outside = path.parent().unwrap().join("outside-database-hardlink");
        write_owner_only(&outside, b"external-database-must-not-change");
        fs::hard_link(&outside, &path).unwrap();

        let error = OperationJournal::open(&path).unwrap_err();

        assert_eq!(error.code(), "operation_db_links");
        assert_eq!(
            fs::read(&outside).unwrap(),
            b"external-database-must-not-change"
        );
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn wal_and_shm_symlinks_are_rejected_before_sqlite_open() {
        for suffix in ["-wal", "-shm"] {
            let path = journal_path(&format!("sidecar-symlink-{}", &suffix[1..]));
            let candidate = sidecar_path(&path, suffix);
            let outside = path
                .parent()
                .unwrap()
                .join(format!("outside-sidecar-{}", &suffix[1..]));
            write_owner_only(&path, b"");
            write_owner_only(&outside, b"external-sidecar-must-not-change");
            symlink(&outside, &candidate).unwrap();

            let error = OperationJournal::open(&path).unwrap_err();

            assert_eq!(error.code(), "operation_db_type", "{suffix}");
            assert_eq!(fs::read(&path).unwrap(), b"", "{suffix}");
            assert_eq!(
                fs::read(&outside).unwrap(),
                b"external-sidecar-must-not-change",
                "{suffix}"
            );
        }
    }

    #[test]
    fn wal_and_shm_non_regular_entries_are_rejected_before_sqlite_open() {
        for suffix in ["-wal", "-shm"] {
            let path = journal_path(&format!("sidecar-directory-{}", &suffix[1..]));
            let candidate = sidecar_path(&path, suffix);
            write_owner_only(&path, b"");
            fs::create_dir(&candidate).unwrap();

            let error = OperationJournal::open(&path).unwrap_err();

            assert_eq!(error.code(), "operation_db_type", "{suffix}");
            assert_eq!(fs::read(&path).unwrap(), b"", "{suffix}");
            assert!(candidate.is_dir(), "{suffix}");
        }
    }

    #[test]
    fn wal_and_shm_hardlinks_are_rejected_without_external_mutation() {
        for suffix in ["-wal", "-shm"] {
            let path = journal_path(&format!("sidecar-hardlink-{}", &suffix[1..]));
            let candidate = sidecar_path(&path, suffix);
            let outside = path
                .parent()
                .unwrap()
                .join(format!("outside-hardlink-{}", &suffix[1..]));
            write_owner_only(&path, b"");
            write_owner_only(&outside, b"external-hardlink-must-not-change");
            fs::hard_link(&outside, &candidate).unwrap();

            let error = OperationJournal::open(&path).unwrap_err();

            assert_eq!(error.code(), "operation_db_links", "{suffix}");
            assert_eq!(fs::read(&path).unwrap(), b"", "{suffix}");
            assert_eq!(
                fs::read(&outside).unwrap(),
                b"external-hardlink-must-not-change",
                "{suffix}"
            );
            assert_eq!(
                fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
                0o600,
                "{suffix}"
            );
        }
    }

    #[test]
    fn wal_and_shm_open_permissions_are_rejected_before_chmod() {
        for suffix in ["-wal", "-shm"] {
            let path = journal_path(&format!("sidecar-permissions-{}", &suffix[1..]));
            let candidate = sidecar_path(&path, suffix);
            write_owner_only(&path, b"");
            fs::write(&candidate, b"sidecar-must-not-change").unwrap();
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o666)).unwrap();

            let error = OperationJournal::open(&path).unwrap_err();

            assert_eq!(error.code(), "operation_db_permissions", "{suffix}");
            assert_eq!(fs::read(&path).unwrap(), b"", "{suffix}");
            assert_eq!(
                fs::read(&candidate).unwrap(),
                b"sidecar-must-not-change",
                "{suffix}"
            );
            assert_eq!(
                fs::metadata(&candidate).unwrap().permissions().mode() & 0o777,
                0o666,
                "{suffix}"
            );
        }
    }

    #[test]
    fn wal_and_shm_are_rechecked_after_journal_mode_before_schema() {
        for (label, suffix, inject) in [
            ("wal", "-wal", inject_wal_symlink as fn(&std::path::Path)),
            ("shm", "-shm", inject_shm_symlink as fn(&std::path::Path)),
        ] {
            let path = journal_path(&format!("post-journal-{label}"));
            let outside = path
                .parent()
                .unwrap()
                .join(format!("outside-post-journal-{label}"));

            let error = OperationJournal::open_after_journal_mode(&path, inject).unwrap_err();

            assert_eq!(error.code(), "operation_db_type", "{suffix}");
            assert_eq!(
                fs::read(&outside).unwrap(),
                b"post-journal-target-must-not-change",
                "{suffix}"
            );
            fs::remove_file(sidecar_path(&path, suffix)).unwrap();
            let connection = rusqlite::Connection::open_with_flags(
                format!("file:{}?immutable=1", path.display()),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .unwrap();
            let schema_rows: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'operations'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(schema_rows, 0, "{suffix}");
        }
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
