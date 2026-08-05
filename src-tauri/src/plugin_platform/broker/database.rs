use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use super::access::VerifiedBrokerAccess;
use super::{BrokerError, BrokerResult};

const DATABASE_NAME: &str = "broker-v1.sqlite3";
const SQLITE_SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS broker_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  schema_version INTEGER NOT NULL,
  broker_revision INTEGER NOT NULL,
  clean_shutdown INTEGER NOT NULL CHECK (clean_shutdown IN (0, 1)),
  opened_at_ms INTEGER NOT NULL
);
INSERT OR IGNORE INTO broker_meta(singleton, schema_version, broker_revision, clean_shutdown, opened_at_ms)
VALUES(1, 1, 0, 1, 0);

CREATE TABLE IF NOT EXISTS broker_contracts (
  contract_id TEXT NOT NULL,
  version TEXT NOT NULL,
  schema_digest TEXT NOT NULL,
  publisher_plugin_id TEXT NOT NULL,
  publisher_key_lineage TEXT NOT NULL,
  publisher_activation_generation INTEGER NOT NULL,
  sensitivity TEXT NOT NULL,
  visibility TEXT NOT NULL,
  retention TEXT NOT NULL,
  schema_json BLOB NOT NULL,
  installed_package_digest TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(contract_id, version),
  UNIQUE(contract_id, version, schema_digest)
);

CREATE TABLE IF NOT EXISTS broker_entities (
  contract_id TEXT NOT NULL,
  contract_version TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  owner_plugin_id TEXT NOT NULL,
  owner_package_digest TEXT NOT NULL,
  revision INTEGER NOT NULL,
  broker_revision INTEGER NOT NULL,
  state TEXT NOT NULL,
  data_json BLOB NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  stale INTEGER NOT NULL CHECK (stale IN (0, 1)),
  PRIMARY KEY(contract_id, contract_version, entity_id),
  FOREIGN KEY(contract_id, contract_version)
    REFERENCES broker_contracts(contract_id, version)
);

CREATE TABLE IF NOT EXISTS broker_entity_changes (
  broker_revision INTEGER NOT NULL,
  change_ordinal INTEGER NOT NULL,
  contract_id TEXT NOT NULL,
  contract_version TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  entity_revision INTEGER NOT NULL,
  change_kind TEXT NOT NULL,
  envelope_json BLOB NOT NULL,
  PRIMARY KEY(broker_revision, change_ordinal)
);

CREATE TABLE IF NOT EXISTS broker_quarantine (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  owner_plugin_id TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  record_kind TEXT NOT NULL,
  record_key TEXT NOT NULL,
  reason_code TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  payload_blob BLOB NOT NULL,
  quarantined_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS broker_streams (
  contract_id TEXT NOT NULL,
  contract_version TEXT NOT NULL,
  stream_id TEXT NOT NULL,
  next_seq INTEGER NOT NULL,
  earliest_seq INTEGER NOT NULL,
  latest_seq INTEGER NOT NULL,
  PRIMARY KEY(contract_id, contract_version, stream_id),
  FOREIGN KEY(contract_id, contract_version)
    REFERENCES broker_contracts(contract_id, version)
);

CREATE TABLE IF NOT EXISTS broker_events (
  contract_id TEXT NOT NULL,
  contract_version TEXT NOT NULL,
  stream_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  event_id TEXT NOT NULL,
  subject TEXT NOT NULL,
  kind TEXT NOT NULL,
  correlation_id TEXT,
  data_json BLOB NOT NULL,
  payload_digest TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  broker_revision INTEGER NOT NULL,
  owner_plugin_id TEXT NOT NULL,
  owner_package_digest TEXT NOT NULL,
  PRIMARY KEY(contract_id, contract_version, stream_id, seq),
  UNIQUE(contract_id, contract_version, stream_id, event_id),
  FOREIGN KEY(contract_id, contract_version, stream_id)
    REFERENCES broker_streams(contract_id, contract_version, stream_id)
);

CREATE TABLE IF NOT EXISTS broker_cursors (
  cursor_id TEXT PRIMARY KEY,
  consumer_plugin_id TEXT NOT NULL,
  consumer_signer_lineage TEXT NOT NULL,
  consumer_package_digest TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  contract_version TEXT NOT NULL,
  next_broker_revision INTEGER NOT NULL,
  delivered_through INTEGER NOT NULL,
  last_ack_ms INTEGER NOT NULL,
  grant_revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_private_storage (
  plugin_id TEXT NOT NULL,
  signer_lineage TEXT NOT NULL,
  key TEXT NOT NULL,
  value_json BLOB NOT NULL,
  revision INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(plugin_id, signer_lineage, key)
);

CREATE TABLE IF NOT EXISTS plugin_private_storage_usage (
  plugin_id TEXT NOT NULL,
  signer_lineage TEXT NOT NULL,
  total_bytes INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  PRIMARY KEY(plugin_id, signer_lineage)
);

CREATE TABLE IF NOT EXISTS broker_outbox_receipts (
  owner_plugin_id TEXT NOT NULL,
  owner_package_digest TEXT NOT NULL,
  source_instance_id TEXT NOT NULL,
  outbox_id TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  applied_broker_revision INTEGER NOT NULL,
  applied_at_ms INTEGER NOT NULL,
  PRIMARY KEY(owner_plugin_id, owner_package_digest, source_instance_id, outbox_id)
);
"#;

pub(super) struct BrokerDatabase {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl BrokerDatabase {
    pub(super) fn open(root: &Path, now_ms: i64) -> BrokerResult<Self> {
        if !root.is_absolute() {
            return Err(BrokerError::new(
                "broker_path",
                "broker root must be absolute",
            ));
        }
        reject_symlinked_ancestors(root)?;
        if root.exists() {
            reject_unsafe_directory(root)?;
        } else {
            fs::create_dir_all(root)
                .map_err(|error| BrokerError::new("broker_path", error.to_string()))?;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))
                .map_err(|error| BrokerError::new("broker_path", error.to_string()))?;
        }
        reject_symlinked_ancestors(root)?;
        reject_unsafe_directory(root)?;

        let path = root.join(DATABASE_NAME);
        if !path.exists() {
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .map_err(|error| BrokerError::new("broker_path", error.to_string()))?;
        }
        reject_unsafe_file(&path)?;
        let _prepared_sidecars = prepare_sqlite_sidecars(&path)?;

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&path, flags)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; \
             PRAGMA journal_mode = WAL; \
             PRAGMA synchronous = FULL; \
             PRAGMA temp_store = MEMORY;",
        )?;
        connection.execute_batch(MIGRATION_V1)?;
        ensure_exact_access_columns(&connection)?;
        connection.execute(
            "UPDATE broker_meta SET schema_version = 2 WHERE singleton = 1",
            [],
        )?;

        let clean: i64 = connection.query_row(
            "SELECT clean_shutdown FROM broker_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if clean == 0 {
            let quick: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            if quick != "ok" {
                return Err(BrokerError::new(
                    "broker_integrity",
                    "unclean broker database failed quick_check",
                ));
            }
        }
        connection.execute(
            "UPDATE broker_meta SET clean_shutdown = 0, opened_at_ms = ?1 WHERE singleton = 1",
            [now_ms],
        )?;
        reject_unsafe_sqlite_sidecars(&path)?;

        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }

    pub(super) fn with_read<T>(
        &self,
        action: impl FnOnce(&Connection) -> BrokerResult<T>,
    ) -> BrokerResult<T> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::new("broker_lock", "broker lock poisoned"))?;
        action(&connection)
    }

    pub(super) fn with_access_read<T>(
        &self,
        access: &VerifiedBrokerAccess,
        action: impl FnOnce(&Connection) -> BrokerResult<T>,
    ) -> BrokerResult<T> {
        let _admission = access.admit()?;
        self.with_read(action)
    }

    pub(super) fn with_access_write<T>(
        &self,
        access: &VerifiedBrokerAccess,
        action: impl FnOnce(&Transaction<'_>) -> BrokerResult<T>,
    ) -> BrokerResult<T> {
        let _admission = access.admit()?;
        self.with_write(action)
    }

    fn with_write<T>(
        &self,
        action: impl FnOnce(&Transaction<'_>) -> BrokerResult<T>,
    ) -> BrokerResult<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::new("broker_lock", "broker lock poisoned"))?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = action(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub(super) fn revision(&self) -> BrokerResult<u64> {
        self.with_read(|connection| {
            let revision: i64 = connection.query_row(
                "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            u64::try_from(revision)
                .map_err(|_| BrokerError::new("broker_storage", "negative broker revision"))
        })
    }

    pub(super) fn shutdown(&self) -> BrokerResult<()> {
        self.with_read(|connection| {
            connection.execute(
                "UPDATE broker_meta SET clean_shutdown = 1 WHERE singleton = 1",
                [],
            )?;
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })?;
        reject_unsafe_file(&self.path)
    }
}

fn ensure_exact_access_columns(connection: &Connection) -> BrokerResult<()> {
    ensure_column(
        connection,
        "broker_contracts",
        "publisher_activation_generation",
        "ALTER TABLE broker_contracts ADD COLUMN \
         publisher_activation_generation INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        connection,
        "broker_cursors",
        "consumer_signer_lineage",
        "ALTER TABLE broker_cursors ADD COLUMN consumer_signer_lineage TEXT NOT NULL DEFAULT ''",
    )
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    migration: &str,
) -> BrokerResult<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    connection.execute_batch(migration)?;
    Ok(())
}

pub(super) fn allocate_revision(transaction: &Transaction<'_>) -> BrokerResult<u64> {
    let current: i64 = transaction.query_row(
        "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| BrokerError::new("broker_revision_overflow", "revision exhausted"))?;
    transaction.execute(
        "UPDATE broker_meta SET broker_revision = ?1 WHERE singleton = 1",
        [next],
    )?;
    u64::try_from(next).map_err(|_| BrokerError::new("broker_storage", "negative broker revision"))
}

fn reject_unsafe_directory(path: &Path) -> BrokerResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| BrokerError::new("broker_path", error.to_string()))?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(BrokerError::new(
            "broker_path",
            "broker directory is not owner-private",
        ));
    }
    Ok(())
}

fn reject_unsafe_file(path: &Path) -> BrokerResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| BrokerError::new("broker_path", error.to_string()))?;
    reject_unsafe_file_metadata(&metadata)
}

fn reject_unsafe_file_metadata(metadata: &fs::Metadata) -> BrokerResult<()> {
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(BrokerError::new(
            "broker_path",
            "broker database is not an owner-private single-link file",
        ));
    }
    Ok(())
}

fn reject_symlinked_ancestors(path: &Path) -> BrokerResult<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(BrokerError::new(
                "broker_path",
                "broker path must not contain parent traversal",
            ));
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BrokerError::new(
                    "broker_path",
                    "broker path must not contain symlink ancestors",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(BrokerError::new("broker_path", error.to_string())),
        }
    }
    Ok(())
}

fn prepare_sqlite_sidecars(database_path: &Path) -> BrokerResult<Vec<File>> {
    SQLITE_SIDECAR_SUFFIXES
        .into_iter()
        .map(|suffix| prepare_sqlite_sidecar(&sidecar_path(database_path, suffix)))
        .collect()
}

fn prepare_sqlite_sidecar(path: &Path) -> BrokerResult<File> {
    let open_existing = || {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    };
    let file = match fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            open_existing().map_err(|error| BrokerError::new("broker_path", error.to_string()))?
        }
        Err(error) => return Err(BrokerError::new("broker_path", error.to_string())),
    };
    reject_unsafe_file_metadata(
        &file
            .metadata()
            .map_err(|error| BrokerError::new("broker_path", error.to_string()))?,
    )?;
    reject_unsafe_file(path)?;
    Ok(file)
}

fn reject_unsafe_sqlite_sidecars(database_path: &Path) -> BrokerResult<()> {
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        let path = sidecar_path(database_path, suffix);
        match fs::symlink_metadata(&path) {
            Ok(_) => reject_unsafe_file(&path)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(BrokerError::new("broker_path", error.to_string())),
        }
    }
    Ok(())
}

fn sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}
