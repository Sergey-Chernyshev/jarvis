use jarvis_plugin_protocol::broker::{
    ContractRef, EntityChange, EntityEnvelope, EntityQuerySnapshot, MAX_ENTITY_BYTES,
};
use rusqlite::OptionalExtension;
use serde_json::Value;

use super::access::VerifiedBrokerAccess;
use super::database::{allocate_revision, BrokerDatabase};
use super::schema_registry::{validate_instance, SchemaRegistry};
use super::{canonical_json, BrokerError, BrokerResult};

const MAX_ENTITY_ID_BYTES: usize = 256;
const MAX_PAGE: u32 = 128;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EntityChangePage {
    pub snapshot_revision: u64,
    pub changes: Vec<EntityChange>,
}

pub(crate) struct EntityStore<'a> {
    database: &'a BrokerDatabase,
}

impl<'a> EntityStore<'a> {
    pub(super) fn new(database: &'a BrokerDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn put(
        &self,
        access: &VerifiedBrokerAccess,
        contract: ContractRef,
        entity_id: &str,
        expected_revision: u64,
        data: Value,
        now_ms: i64,
    ) -> BrokerResult<EntityEnvelope> {
        validate_entity_id(entity_id)?;
        let canonical = canonical_json(&data)?;
        if canonical.len() > MAX_ENTITY_BYTES {
            return Err(BrokerError::new(
                "payload_too_large",
                "entity exceeds byte quota",
            ));
        }
        let registered = SchemaRegistry::new(self.database).exact(&contract)?;
        authorize(access, &registered)?;
        validate_instance(&registered.schema, &data)?;

        self.database.with_access_write(access, |transaction| {
            let current: Option<i64> = transaction
                .query_row(
                    "SELECT revision FROM broker_entities \
                      WHERE contract_id = ?1 AND contract_version = ?2 AND entity_id = ?3",
                    rusqlite::params![contract.id, contract.version.to_string(), entity_id],
                    |row| row.get(0),
                )
                .optional()?;
            let current = current.unwrap_or(0);
            if u64::try_from(current).ok() != Some(expected_revision) {
                return Err(BrokerError::new(
                    "revision_conflict",
                    "entity revision changed",
                ));
            }
            let entity_revision = expected_revision.checked_add(1).ok_or_else(|| {
                BrokerError::new("revision_overflow", "entity revision exhausted")
            })?;
            let broker_revision = allocate_revision(transaction)?;
            let envelope = EntityEnvelope {
                contract: contract.clone(),
                id: entity_id.into(),
                revision: entity_revision,
                broker_revision,
                state: "active".into(),
                data: data.clone(),
                updated_at_ms: now_ms,
                stale: false,
            };
            let envelope_json = canonical_json(
                &serde_json::to_value(&envelope)
                    .map_err(|_| BrokerError::new("broker_storage", "entity encoding failed"))?,
            )?;
            transaction.execute(
                "INSERT INTO broker_entities( \
                   contract_id, contract_version, entity_id, owner_plugin_id, \
                   owner_package_digest, revision, broker_revision, state, data_json, \
                   updated_at_ms, stale \
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, 0) \
                 ON CONFLICT(contract_id, contract_version, entity_id) DO UPDATE SET \
                   owner_plugin_id = excluded.owner_plugin_id, \
                   owner_package_digest = excluded.owner_package_digest, \
                   revision = excluded.revision, \
                   broker_revision = excluded.broker_revision, \
                   state = excluded.state, \
                   data_json = excluded.data_json, \
                   updated_at_ms = excluded.updated_at_ms, \
                   stale = 0",
                rusqlite::params![
                    contract.id,
                    contract.version.to_string(),
                    entity_id,
                    access.plugin_id(),
                    access.package_digest(),
                    entity_revision,
                    broker_revision,
                    canonical,
                    now_ms,
                ],
            )?;
            transaction.execute(
                "INSERT INTO broker_entity_changes( \
                   broker_revision, change_ordinal, contract_id, contract_version, entity_id, \
                   entity_revision, change_kind, envelope_json \
                 ) VALUES(?1, 0, ?2, ?3, ?4, ?5, 'put', ?6)",
                rusqlite::params![
                    broker_revision,
                    contract.id,
                    contract.version.to_string(),
                    entity_id,
                    entity_revision,
                    envelope_json,
                ],
            )?;
            Ok(envelope)
        })
    }

    pub(crate) fn delete(
        &self,
        access: &VerifiedBrokerAccess,
        contract: ContractRef,
        entity_id: &str,
        expected_revision: u64,
        now_ms: i64,
    ) -> BrokerResult<EntityEnvelope> {
        validate_entity_id(entity_id)?;
        let registered = SchemaRegistry::new(self.database).exact(&contract)?;
        authorize(access, &registered)?;

        self.database.with_access_write(access, |transaction| {
            let row = transaction
                .query_row(
                    "SELECT revision, data_json FROM broker_entities \
                      WHERE contract_id = ?1 AND contract_version = ?2 AND entity_id = ?3",
                    rusqlite::params![contract.id, contract.version.to_string(), entity_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
                .ok_or_else(|| BrokerError::new("entity_not_found", "entity does not exist"))?;
            if u64::try_from(row.0).ok() != Some(expected_revision) {
                return Err(BrokerError::new(
                    "revision_conflict",
                    "entity revision changed",
                ));
            }
            let entity_revision = expected_revision.checked_add(1).ok_or_else(|| {
                BrokerError::new("revision_overflow", "entity revision exhausted")
            })?;
            let broker_revision = allocate_revision(transaction)?;
            let data = serde_json::from_slice(&row.1)
                .map_err(|_| BrokerError::new("broker_storage", "stored entity is invalid"))?;
            let envelope = EntityEnvelope {
                contract: contract.clone(),
                id: entity_id.into(),
                revision: entity_revision,
                broker_revision,
                state: "deleted".into(),
                data,
                updated_at_ms: now_ms,
                stale: true,
            };
            let envelope_json = canonical_json(
                &serde_json::to_value(&envelope)
                    .map_err(|_| BrokerError::new("broker_storage", "entity encoding failed"))?,
            )?;
            transaction.execute(
                "DELETE FROM broker_entities \
                  WHERE contract_id = ?1 AND contract_version = ?2 AND entity_id = ?3",
                rusqlite::params![contract.id, contract.version.to_string(), entity_id],
            )?;
            transaction.execute(
                "INSERT INTO broker_entity_changes( \
                   broker_revision, change_ordinal, contract_id, contract_version, entity_id, \
                   entity_revision, change_kind, envelope_json \
                 ) VALUES(?1, 0, ?2, ?3, ?4, ?5, 'delete', ?6)",
                rusqlite::params![
                    broker_revision,
                    contract.id,
                    contract.version.to_string(),
                    entity_id,
                    entity_revision,
                    envelope_json,
                ],
            )?;
            Ok(envelope)
        })
    }

    pub(crate) fn snapshot(
        &self,
        access: &VerifiedBrokerAccess,
        contract: &ContractRef,
        limit: u32,
    ) -> BrokerResult<EntityQuerySnapshot> {
        let limit = validate_limit(limit)?;
        let registered = SchemaRegistry::new(self.database).exact(contract)?;
        authorize_read(access, &registered)?;
        self.database.with_access_read(access, |connection| {
            let snapshot_revision: i64 = connection.query_row(
                "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            let mut statement = connection.prepare(
                "SELECT contract_version, entity_id, revision, broker_revision, state, \
                        data_json, updated_at_ms, stale \
                   FROM broker_entities \
                  WHERE contract_id = ?1 AND contract_version = ?2 \
                   ORDER BY contract_version, entity_id LIMIT ?3",
            )?;
            let rows = statement.query_map(
                rusqlite::params![contract.id, contract.version.to_string(), limit],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )?;
            let mut entities = Vec::new();
            for row in rows {
                let row = row?;
                let version = semver::Version::parse(&row.0)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid stored version"))?;
                entities.push(EntityEnvelope {
                    contract: ContractRef {
                        id: contract.id.clone(),
                        version,
                        schema_digest: contract.schema_digest.clone(),
                    },
                    id: row.1,
                    revision: u64::try_from(row.2)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid revision"))?,
                    broker_revision: u64::try_from(row.3).map_err(|_| {
                        BrokerError::new("broker_storage", "invalid broker revision")
                    })?,
                    state: row.4,
                    data: serde_json::from_slice(&row.5)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid stored entity"))?,
                    updated_at_ms: row.6,
                    stale: row.7 != 0,
                });
            }
            Ok(EntityQuerySnapshot {
                snapshot_revision: u64::try_from(snapshot_revision)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid snapshot revision"))?,
                entities,
            })
        })
    }

    pub(crate) fn changes_after(
        &self,
        access: &VerifiedBrokerAccess,
        contract: &ContractRef,
        cursor: u64,
        limit: u32,
    ) -> BrokerResult<EntityChangePage> {
        let limit = validate_limit(limit)?;
        let registered = SchemaRegistry::new(self.database).exact(contract)?;
        authorize_read(access, &registered)?;
        self.database.with_access_read(access, |connection| {
            let snapshot_revision: i64 = connection.query_row(
                "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            let mut statement = connection.prepare(
                "SELECT broker_revision, envelope_json FROM broker_entity_changes \
                  WHERE contract_id = ?1 AND contract_version = ?2 \
                    AND broker_revision > ?3 \
                  ORDER BY broker_revision LIMIT ?4",
            )?;
            let rows = statement.query_map(
                rusqlite::params![contract.id, contract.version.to_string(), cursor, limit],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )?;
            let mut changes = Vec::new();
            for row in rows {
                let row = row?;
                changes.push(EntityChange {
                    cursor: u64::try_from(row.0)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid cursor"))?,
                    entity: serde_json::from_slice(&row.1)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid stored change"))?,
                });
            }
            Ok(EntityChangePage {
                snapshot_revision: u64::try_from(snapshot_revision)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid snapshot revision"))?,
                changes,
            })
        })
    }
}

pub(super) fn authorize(
    access: &VerifiedBrokerAccess,
    registered: &super::schema_registry::ResolvedContract,
) -> BrokerResult<()> {
    access.ensure_live()?;
    if registered.publisher_plugin_id != access.plugin_id()
        || registered.publisher_key_lineage != access.signer_lineage()
        || registered.installed_package_digest != access.package_digest()
        || registered.publisher_activation_generation != access.activation_generation()
    {
        return Err(BrokerError::new(
            "contract_forbidden",
            "activation does not own the exact contract",
        ));
    }
    Ok(())
}

pub(super) fn authorize_read(
    access: &VerifiedBrokerAccess,
    registered: &super::schema_registry::ResolvedContract,
) -> BrokerResult<()> {
    let contract = ContractRef {
        id: registered.contract_id.clone(),
        version: registered.version.clone(),
        schema_digest: registered.schema_digest.clone(),
    };
    authorize_read_binding(
        access,
        &contract,
        &registered.publisher_plugin_id,
        &registered.publisher_key_lineage,
        &registered.installed_package_digest,
        registered.publisher_activation_generation,
    )
}

pub(super) fn authorize_read_binding(
    access: &VerifiedBrokerAccess,
    contract: &ContractRef,
    provider_plugin_id: &str,
    provider_signer_lineage: &str,
    provider_package_digest: &str,
    provider_activation_generation: u64,
) -> BrokerResult<()> {
    access.ensure_live()?;
    if provider_plugin_id == access.plugin_id()
        && provider_signer_lineage == access.signer_lineage()
        && provider_package_digest == access.package_digest()
        && provider_activation_generation == access.activation_generation()
    {
        return Ok(());
    }
    if access.permits_contract(
        contract,
        provider_plugin_id,
        provider_signer_lineage,
        provider_package_digest,
        provider_activation_generation,
    )? {
        return Ok(());
    }
    Err(BrokerError::new(
        "contract_forbidden",
        "activation has no exact read grant for the contract",
    ))
}

fn validate_entity_id(entity_id: &str) -> BrokerResult<()> {
    if entity_id.is_empty()
        || entity_id.len() > MAX_ENTITY_ID_BYTES
        || entity_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(BrokerError::new("invalid_entity_id", "invalid entity id"));
    }
    Ok(())
}

fn validate_limit(limit: u32) -> BrokerResult<i64> {
    if limit == 0 || limit > MAX_PAGE {
        return Err(BrokerError::new("invalid_limit", "invalid broker limit"));
    }
    Ok(i64::from(limit))
}
