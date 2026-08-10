use std::path::{Component, Path, PathBuf};

use jarvis_plugin_protocol::manifest::StateDeclaration;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manager::{ManagerError, ManagerResult};

const MAX_MIGRATION_DOCUMENTS: usize = 128;
const MAX_OPERATIONS_PER_DOCUMENT: usize = 1024;
const MAX_PATH_BYTES: usize = 512;
const MAX_POINTER_BYTES: usize = 4096;
const MAX_SQL_BYTES: usize = 64 * 1024;
const MAX_PARAMETER_STRING_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationDocument {
    pub schema_version: u32,
    pub from: u32,
    pub to: u32,
    pub reversible: bool,
    pub operations: Vec<MigrationOperation>,
}

impl MigrationDocument {
    pub fn new(from: u32, to: u32, reversible: bool, operations: Vec<MigrationOperation>) -> Self {
        Self {
            schema_version: 1,
            from,
            to,
            reversible,
            operations,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MigrationOperation {
    JsonSet {
        path: String,
        pointer: String,
        value: Value,
    },
    JsonRename {
        path: String,
        from_pointer: String,
        to_pointer: String,
    },
    JsonDelete {
        path: String,
        pointer: String,
    },
    Sqlite {
        database: String,
        statement: String,
        parameters: Vec<Value>,
    },
}

#[derive(Clone, Debug)]
pub struct MigrationRequest {
    pub package_root: PathBuf,
    pub state_root: PathBuf,
    pub current_schema_version: u32,
    pub target: StateDeclaration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub schema_version: u32,
    pub rollback_available: bool,
}

pub trait MigrationRunner: Send + Sync {
    fn migrate(&self, request: &MigrationRequest) -> ManagerResult<MigrationOutcome>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RefuseSchemaChanges;

impl MigrationRunner for RefuseSchemaChanges {
    fn migrate(&self, request: &MigrationRequest) -> ManagerResult<MigrationOutcome> {
        if request.current_schema_version != request.target.schema_version {
            return Err(ManagerError::new(
                "migration_runner_unavailable",
                "state schema change requires the declarative migration runner",
            ));
        }
        Ok(MigrationOutcome {
            schema_version: request.target.schema_version,
            rollback_available: request.target.rollback_compatible_through
                <= request.current_schema_version,
        })
    }
}

pub fn validate_migration_set(
    current: u32,
    target: u32,
    documents: &[MigrationDocument],
) -> ManagerResult<MigrationOutcome> {
    if current == 0 || target == 0 || current > target {
        return Err(ManagerError::new(
            "migration_graph",
            "invalid state schema version range",
        ));
    }
    if documents.len() > MAX_MIGRATION_DOCUMENTS {
        return Err(ManagerError::new(
            "migration_limit",
            "too many migration documents",
        ));
    }
    let mut cursor = current;
    let mut rollback_available = true;
    for document in documents {
        if document.schema_version != 1
            || document.from != cursor
            || document.to <= document.from
            || document.to > target
        {
            return Err(ManagerError::new(
                "migration_graph_gap",
                format!(
                    "expected migration from schema {cursor}, got {} -> {}",
                    document.from, document.to
                ),
            ));
        }
        if document.operations.len() > MAX_OPERATIONS_PER_DOCUMENT {
            return Err(ManagerError::new(
                "migration_limit",
                "migration document contains too many operations",
            ));
        }
        for operation in &document.operations {
            validate_operation(operation)?;
        }
        rollback_available &= document.reversible;
        cursor = document.to;
    }
    if cursor != target {
        return Err(ManagerError::new(
            "migration_graph_gap",
            format!("migration graph stops at {cursor}, target is {target}"),
        ));
    }
    Ok(MigrationOutcome {
        schema_version: target,
        rollback_available,
    })
}

pub fn validate_operation(operation: &MigrationOperation) -> ManagerResult<()> {
    match operation {
        MigrationOperation::JsonSet {
            path,
            pointer,
            value,
        } => {
            validate_owned_path(path)?;
            validate_json_pointer(pointer)?;
            validate_json_value(value, 0)
        }
        MigrationOperation::JsonRename {
            path,
            from_pointer,
            to_pointer,
        } => {
            validate_owned_path(path)?;
            validate_json_pointer(from_pointer)?;
            validate_json_pointer(to_pointer)?;
            if from_pointer == to_pointer {
                return Err(ManagerError::new(
                    "migration_json_pointer",
                    "rename source and destination must differ",
                ));
            }
            Ok(())
        }
        MigrationOperation::JsonDelete { path, pointer } => {
            validate_owned_path(path)?;
            validate_json_pointer(pointer)
        }
        MigrationOperation::Sqlite {
            database,
            statement,
            parameters,
        } => {
            validate_owned_path(database)?;
            if !matches!(
                Path::new(database)
                    .extension()
                    .and_then(|value| value.to_str()),
                Some("sqlite") | Some("sqlite3") | Some("db")
            ) {
                return Err(ManagerError::new(
                    "migration_database",
                    "SQLite migration target must use a database extension",
                ));
            }
            validate_sql(statement, parameters)
        }
    }
}

fn validate_owned_path(value: &str) -> ManagerResult<()> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(ManagerError::new(
            "migration_path",
            "migration path is invalid",
        ));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(ManagerError::new(
            "migration_path",
            "absolute migration paths are forbidden",
        ));
    }
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return Err(ManagerError::new(
            "migration_path",
            "migration path must be plugin-owned",
        ));
    };
    if root != "settings" && root != "state" {
        return Err(ManagerError::new(
            "migration_path",
            "migration path must start with settings/ or state/",
        ));
    }
    let mut child_count = 0_usize;
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            return Err(ManagerError::new(
                "migration_path",
                "migration path contains traversal",
            ));
        }
        child_count += 1;
    }
    if child_count == 0 {
        return Err(ManagerError::new(
            "migration_path",
            "migration path must name a file below its plugin-owned root",
        ));
    }
    Ok(())
}

fn validate_json_pointer(pointer: &str) -> ManagerResult<()> {
    if pointer.len() > MAX_POINTER_BYTES
        || (!pointer.is_empty() && !pointer.starts_with('/'))
        || pointer.contains('\0')
    {
        return Err(ManagerError::new(
            "migration_json_pointer",
            "invalid bounded JSON pointer",
        ));
    }
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~'
            && (index + 1 == bytes.len() || !matches!(bytes[index + 1], b'0' | b'1'))
        {
            return Err(ManagerError::new(
                "migration_json_pointer",
                "JSON pointer contains an invalid escape",
            ));
        }
        index += 1;
    }
    Ok(())
}

fn validate_json_value(value: &Value, depth: usize) -> ManagerResult<()> {
    if depth > 32 {
        return Err(ManagerError::new(
            "migration_json_value",
            "migration JSON value is too deeply nested",
        ));
    }
    match value {
        Value::String(value) if value.len() > MAX_PARAMETER_STRING_BYTES => Err(ManagerError::new(
            "migration_json_value",
            "migration JSON string is too large",
        )),
        Value::Array(values) => {
            if values.len() > MAX_OPERATIONS_PER_DOCUMENT {
                return Err(ManagerError::new(
                    "migration_json_value",
                    "migration JSON array is too large",
                ));
            }
            for value in values {
                validate_json_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            if values.len() > MAX_OPERATIONS_PER_DOCUMENT {
                return Err(ManagerError::new(
                    "migration_json_value",
                    "migration JSON object is too large",
                ));
            }
            for (key, value) in values {
                if key.len() > MAX_PATH_BYTES {
                    return Err(ManagerError::new(
                        "migration_json_value",
                        "migration JSON key is too large",
                    ));
                }
                validate_json_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

fn validate_sql(statement: &str, parameters: &[Value]) -> ManagerResult<()> {
    if statement.is_empty()
        || statement.len() > MAX_SQL_BYTES
        || statement.contains('\0')
        || statement.contains("--")
        || statement.contains("/*")
        || statement.contains("*/")
        || statement.contains('/')
    {
        return Err(ManagerError::new(
            "migration_sql",
            "SQL migration statement contains forbidden syntax",
        ));
    }
    let trimmed = statement.trim();
    let without_trailing = trimmed.strip_suffix(';').unwrap_or(trimmed).trim_end();
    if without_trailing.contains(';') {
        return Err(ManagerError::new(
            "migration_sql",
            "SQL migration must contain exactly one statement",
        ));
    }
    let normalized = without_trailing
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let forbidden = [
        "attach",
        "detach",
        "pragma",
        "load_extension",
        "trigger",
        "virtual table",
        "vacuum",
        "reindex",
        "analyze",
        "insert",
        "delete",
        "drop",
        "replace",
        "select",
    ];
    if forbidden
        .iter()
        .any(|token| contains_word(&normalized, token))
    {
        return Err(ManagerError::new(
            "migration_sql_forbidden",
            "SQL migration uses a forbidden capability",
        ));
    }

    let allowed = normalized.starts_with("create table ")
        || normalized.starts_with("create index ")
        || (normalized.starts_with("alter table ") && normalized.contains(" add column "))
        || normalized.starts_with("update ");
    if !allowed {
        return Err(ManagerError::new(
            "migration_sql_forbidden",
            "SQL migration is outside the host-interpreted subset",
        ));
    }
    if normalized.starts_with("alter table ")
        && (normalized.contains(" rename ")
            || normalized.contains(" drop ")
            || normalized.contains(" alter column "))
    {
        return Err(ManagerError::new(
            "migration_sql_forbidden",
            "ALTER TABLE is limited to ADD COLUMN",
        ));
    }
    let open_parentheses = normalized.bytes().filter(|byte| *byte == b'(').count();
    let close_parentheses = normalized.bytes().filter(|byte| *byte == b')').count();
    if normalized.starts_with("create table ") || normalized.starts_with("create index ") {
        if open_parentheses != 1 || close_parentheses != 1 || !parameters.is_empty() {
            return Err(ManagerError::new(
                "migration_sql_forbidden",
                "DDL is limited to one structural column list without functions",
            ));
        }
    } else if normalized.starts_with("alter table ")
        && (open_parentheses != 0 || close_parentheses != 0 || !parameters.is_empty())
    {
        return Err(ManagerError::new(
            "migration_sql_forbidden",
            "ALTER TABLE ADD COLUMN cannot call functions or accept parameters",
        ));
    }
    if normalized.starts_with("update ")
        && (!normalized.contains(" set ")
            || !normalized.contains('?')
            || normalized.contains('(')
            || parameters.is_empty())
    {
        return Err(ManagerError::new(
            "migration_sql_parameters",
            "UPDATE must be a simple parameterized statement",
        ));
    }
    for parameter in parameters {
        match parameter {
            Value::String(value) if value.len() <= MAX_PARAMETER_STRING_BYTES => {}
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
            _ => {
                return Err(ManagerError::new(
                    "migration_sql_parameters",
                    "SQLite parameters must be bounded scalar JSON values",
                ));
            }
        }
    }
    Ok(())
}

fn contains_word(value: &str, needle: &str) -> bool {
    value.match_indices(needle).any(|(index, _)| {
        let before = value[..index].chars().next_back();
        let after = value[index + needle.len()..].chars().next();
        !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
    })
}

fn is_identifier_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}
