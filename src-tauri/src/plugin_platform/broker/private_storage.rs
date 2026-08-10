use rusqlite::OptionalExtension;
use serde_json::Value;

use super::access::VerifiedBrokerAccess;
use super::database::BrokerDatabase;
use super::{canonical_json, BrokerError, BrokerResult};

const MAX_KEY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_NAMESPACE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LIST_LIMIT: u32 = 128;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrivateValue {
    pub key: String,
    pub value: Value,
    pub revision: u64,
    pub updated_at_ms: i64,
}

pub(crate) struct PrivateStorage<'a> {
    database: &'a BrokerDatabase,
}

impl<'a> PrivateStorage<'a> {
    pub(super) fn new(database: &'a BrokerDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn get(
        &self,
        access: &VerifiedBrokerAccess,
        key: &str,
    ) -> BrokerResult<Option<PrivateValue>> {
        access.ensure_live()?;
        validate_key(key)?;
        self.database.with_access_read(access, |connection| {
            connection
                .query_row(
                    "SELECT value_json, revision, updated_at_ms FROM plugin_private_storage \
                      WHERE plugin_id = ?1 AND signer_lineage = ?2 AND key = ?3",
                    rusqlite::params![access.plugin_id(), access.signer_lineage(), key],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?
                .map(|row| decode_value(key, row))
                .transpose()
        })
    }

    pub(crate) fn set(
        &self,
        access: &VerifiedBrokerAccess,
        key: &str,
        expected_revision: u64,
        value: Value,
        now_ms: i64,
    ) -> BrokerResult<PrivateValue> {
        access.ensure_live()?;
        validate_key(key)?;
        let bytes = canonical_json(&value)?;
        if bytes.len() > MAX_VALUE_BYTES {
            return Err(BrokerError::new(
                "storage_quota",
                "private value exceeds byte quota",
            ));
        }
        self.database.with_access_write(access, |transaction| {
            let existing = transaction
                .query_row(
                    "SELECT length(value_json), revision FROM plugin_private_storage \
                      WHERE plugin_id = ?1 AND signer_lineage = ?2 AND key = ?3",
                    rusqlite::params![access.plugin_id(), access.signer_lineage(), key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
            if u64::try_from(existing.1).ok() != Some(expected_revision) {
                return Err(BrokerError::new(
                    "revision_conflict",
                    "private value revision changed",
                ));
            }
            let usage = transaction
                .query_row(
                    "SELECT total_bytes, revision FROM plugin_private_storage_usage \
                      WHERE plugin_id = ?1 AND signer_lineage = ?2",
                    rusqlite::params![access.plugin_id(), access.signer_lineage()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
            let next_total = u64::try_from(usage.0)
                .ok()
                .and_then(|total| total.checked_sub(u64::try_from(existing.0).ok()?))
                .and_then(|total| total.checked_add(bytes.len() as u64))
                .ok_or_else(|| BrokerError::new("storage_quota", "invalid storage accounting"))?;
            if next_total > MAX_NAMESPACE_BYTES {
                return Err(BrokerError::new(
                    "storage_quota",
                    "private namespace exceeds byte quota",
                ));
            }
            let revision = expected_revision
                .checked_add(1)
                .ok_or_else(|| BrokerError::new("revision_overflow", "value revision exhausted"))?;
            let usage_revision = u64::try_from(usage.1)
                .map_err(|_| BrokerError::new("broker_storage", "invalid usage revision"))?
                .checked_add(1)
                .ok_or_else(|| BrokerError::new("revision_overflow", "usage revision exhausted"))?;
            transaction.execute(
                "INSERT INTO plugin_private_storage( \
                   plugin_id, signer_lineage, key, value_json, revision, updated_at_ms \
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(plugin_id, signer_lineage, key) DO UPDATE SET \
                   value_json = excluded.value_json, revision = excluded.revision, \
                   updated_at_ms = excluded.updated_at_ms",
                rusqlite::params![
                    access.plugin_id(),
                    access.signer_lineage(),
                    key,
                    bytes,
                    revision,
                    now_ms,
                ],
            )?;
            transaction.execute(
                "INSERT INTO plugin_private_storage_usage( \
                   plugin_id, signer_lineage, total_bytes, revision \
                 ) VALUES(?1, ?2, ?3, ?4) \
                 ON CONFLICT(plugin_id, signer_lineage) DO UPDATE SET \
                   total_bytes = excluded.total_bytes, revision = excluded.revision",
                rusqlite::params![
                    access.plugin_id(),
                    access.signer_lineage(),
                    next_total,
                    usage_revision,
                ],
            )?;
            Ok(PrivateValue {
                key: key.into(),
                value: value.clone(),
                revision,
                updated_at_ms: now_ms,
            })
        })
    }

    pub(crate) fn delete(
        &self,
        access: &VerifiedBrokerAccess,
        key: &str,
        expected_revision: u64,
    ) -> BrokerResult<bool> {
        access.ensure_live()?;
        validate_key(key)?;
        self.database.with_access_write(access, |transaction| {
            let existing = transaction
                .query_row(
                    "SELECT length(value_json), revision FROM plugin_private_storage \
                      WHERE plugin_id = ?1 AND signer_lineage = ?2 AND key = ?3",
                    rusqlite::params![access.plugin_id(), access.signer_lineage(), key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let Some(existing) = existing else {
                return Ok(false);
            };
            if u64::try_from(existing.1).ok() != Some(expected_revision) {
                return Err(BrokerError::new(
                    "revision_conflict",
                    "private value revision changed",
                ));
            }
            let usage: i64 = transaction.query_row(
                "SELECT total_bytes FROM plugin_private_storage_usage \
                  WHERE plugin_id = ?1 AND signer_lineage = ?2",
                rusqlite::params![access.plugin_id(), access.signer_lineage()],
                |row| row.get(0),
            )?;
            let next_total = usage.checked_sub(existing.0).ok_or_else(|| {
                BrokerError::new("broker_storage", "invalid private storage accounting")
            })?;
            transaction.execute(
                "DELETE FROM plugin_private_storage \
                  WHERE plugin_id = ?1 AND signer_lineage = ?2 AND key = ?3",
                rusqlite::params![access.plugin_id(), access.signer_lineage(), key],
            )?;
            transaction.execute(
                "UPDATE plugin_private_storage_usage \
                    SET total_bytes = ?3, revision = revision + 1 \
                  WHERE plugin_id = ?1 AND signer_lineage = ?2",
                rusqlite::params![access.plugin_id(), access.signer_lineage(), next_total],
            )?;
            Ok(true)
        })
    }

    pub(crate) fn list(
        &self,
        access: &VerifiedBrokerAccess,
        after_key: Option<&str>,
        limit: u32,
    ) -> BrokerResult<Vec<String>> {
        access.ensure_live()?;
        if let Some(after) = after_key {
            validate_key(after)?;
        }
        if limit == 0 || limit > MAX_LIST_LIMIT {
            return Err(BrokerError::new(
                "invalid_limit",
                "invalid storage list limit",
            ));
        }
        self.database.with_access_read(access, |connection| {
            let mut statement = connection.prepare(
                "SELECT key FROM plugin_private_storage \
                  WHERE plugin_id = ?1 AND signer_lineage = ?2 AND key > ?3 \
                  ORDER BY key LIMIT ?4",
            )?;
            let rows = statement.query_map(
                rusqlite::params![
                    access.plugin_id(),
                    access.signer_lineage(),
                    after_key.unwrap_or(""),
                    limit,
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
    }
}

fn decode_value(key: &str, row: (Vec<u8>, i64, i64)) -> BrokerResult<PrivateValue> {
    Ok(PrivateValue {
        key: key.into(),
        value: serde_json::from_slice(&row.0)
            .map_err(|_| BrokerError::new("broker_storage", "invalid private value"))?,
        revision: u64::try_from(row.1)
            .map_err(|_| BrokerError::new("broker_storage", "invalid private revision"))?,
        updated_at_ms: row.2,
    })
}

fn validate_key(key: &str) -> BrokerResult<()> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || key.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(BrokerError::new(
            "invalid_storage_key",
            "invalid private storage key",
        ));
    }
    Ok(())
}
