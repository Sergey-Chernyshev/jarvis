use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use jarvis_plugin_protocol::operation::Operation;
pub use jarvis_plugin_protocol::operation::OperationState;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

use super::paths::ensure_real_directory;
use super::StorageError;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct OperationJournal {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl OperationJournal {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        prepare_database_path(&path)?;
        let mut connection = Connection::open(&path).map_err(database_error)?;
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
        protect_database_files(&path)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub fn begin(&self, kind: &str, plugin_id: &str) -> Result<String, StorageError> {
        let id = Uuid::new_v4().to_string();
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
        protect_database_files(&self.path)
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
        protect_database_files(&self.path)
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

fn prepare_database_path(path: &Path) -> Result<(), StorageError> {
    let parent = path.parent().ok_or_else(|| {
        StorageError::new(
            "operation_db_path",
            format!("{} has no parent", path.display()),
        )
    })?;
    ensure_real_directory(parent, 0o700)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(StorageError::new(
                "operation_db_type",
                format!("{} is not a regular database file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| {
                    StorageError::new(
                        "operation_db_create",
                        format!("cannot create {}: {error}", path.display()),
                    )
                })?;
        }
        Err(error) => {
            return Err(StorageError::new(
                "operation_db_path",
                format!("cannot inspect {}: {error}", path.display()),
            ));
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        StorageError::new(
            "operation_db_permissions",
            format!("cannot protect {}: {error}", path.display()),
        )
    })
}

fn protect_database_files(path: &Path) -> Result<(), StorageError> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(StorageError::new(
                    "operation_db_type",
                    format!("{} is not a regular database file", candidate.display()),
                ));
            }
            Ok(_) => fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).map_err(
                |error| {
                    StorageError::new(
                        "operation_db_permissions",
                        format!("cannot protect {}: {error}", candidate.display()),
                    )
                },
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(
                    "operation_db_permissions",
                    format!("cannot inspect {}: {error}", candidate.display()),
                ));
            }
        }
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
    use super::{OperationJournal, OperationState};
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn journal_path(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
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
}
