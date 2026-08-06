use jarvis_plugin_protocol::broker::{
    ContractRef, EntityEnvelope, EntityMutation, EventMutation, OutboxAck, OutboxBatch,
    OutboxMutation, MAX_BROKER_BATCH_ITEMS, MAX_ENTITY_BYTES, MAX_EVENT_BYTES,
};
use rusqlite::{OptionalExtension, Transaction};
use serde_json::Value;

use super::access::VerifiedBrokerAccess;
use super::database::{allocate_revision, BrokerDatabase};
use super::entity_store::authorize;
use super::schema_registry::{validate_instance, ResolvedContract, SchemaRegistry};
use super::{canonical_json, sha256, BrokerError, BrokerResult};

const MAX_ID_BYTES: usize = 256;

enum PreparedMutation {
    EntityPut {
        contract: ContractRef,
        id: String,
        expected_revision: u64,
        data: Value,
        data_json: Vec<u8>,
    },
    EntityDelete {
        contract: ContractRef,
        id: String,
        expected_revision: u64,
    },
    Event {
        event: EventMutation,
        data_json: Vec<u8>,
        event_digest: String,
    },
}

pub(crate) struct OutboxIngress<'a> {
    database: &'a BrokerDatabase,
}

impl<'a> OutboxIngress<'a> {
    pub(super) fn new(database: &'a BrokerDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn apply(
        &self,
        access: &VerifiedBrokerAccess,
        batch: OutboxBatch,
        now_ms: i64,
    ) -> BrokerResult<OutboxAck> {
        validate_id(&batch.source_instance_id)?;
        validate_id(&batch.outbox_id)?;
        if batch.mutations.is_empty() || batch.mutations.len() > MAX_BROKER_BATCH_ITEMS {
            return Err(BrokerError::new(
                "invalid_batch",
                "outbox batch item count is invalid",
            ));
        }
        let batch_value = serde_json::to_value(&batch)
            .map_err(|_| BrokerError::new("invalid_batch", "outbox encoding failed"))?;
        let payload_digest = sha256(&canonical_json(&batch_value)?);
        let mut prepared = Vec::with_capacity(batch.mutations.len());
        for mutation in &batch.mutations {
            prepared.push(self.prepare(access, mutation)?);
        }

        self.database.with_access_write(access, |transaction| {
            if let Some(existing) = transaction
                .query_row(
                    "SELECT payload_digest, applied_broker_revision \
                       FROM broker_outbox_receipts \
                      WHERE owner_plugin_id = ?1 AND owner_package_digest = ?2 \
                        AND source_instance_id = ?3 AND outbox_id = ?4",
                    rusqlite::params![
                        access.plugin_id(),
                        access.package_digest(),
                        batch.source_instance_id,
                        batch.outbox_id,
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
            {
                if existing.0 != payload_digest {
                    return Err(BrokerError::new(
                        "outbox_idempotency_conflict",
                        "outbox ID was reused with different bytes",
                    ));
                }
                return Ok(OutboxAck {
                    source_instance_id: batch.source_instance_id.clone(),
                    outbox_id: batch.outbox_id.clone(),
                    payload_digest: payload_digest.clone(),
                    applied_broker_revision: u64::try_from(existing.1).map_err(|_| {
                        BrokerError::new("broker_storage", "invalid applied broker revision")
                    })?,
                    accepted_operation_refs: Vec::new(),
                });
            }

            let broker_revision = allocate_revision(transaction)?;
            for (ordinal, mutation) in prepared.iter().enumerate() {
                let ordinal = i64::try_from(ordinal)
                    .map_err(|_| BrokerError::new("invalid_batch", "too many mutations"))?;
                match mutation {
                    PreparedMutation::EntityPut {
                        contract,
                        id,
                        expected_revision,
                        data,
                        data_json,
                    } => apply_entity_put(
                        transaction,
                        access,
                        contract,
                        id,
                        *expected_revision,
                        data,
                        data_json,
                        broker_revision,
                        ordinal,
                        now_ms,
                    )?,
                    PreparedMutation::EntityDelete {
                        contract,
                        id,
                        expected_revision,
                    } => apply_entity_delete(
                        transaction,
                        contract,
                        id,
                        *expected_revision,
                        broker_revision,
                        ordinal,
                        now_ms,
                    )?,
                    PreparedMutation::Event {
                        event,
                        data_json,
                        event_digest,
                    } => apply_event(
                        transaction,
                        access,
                        event,
                        data_json,
                        event_digest,
                        broker_revision,
                    )?,
                }
            }
            transaction.execute(
                "INSERT INTO broker_outbox_receipts( \
                   owner_plugin_id, owner_package_digest, source_instance_id, outbox_id, \
                   payload_digest, applied_broker_revision, applied_at_ms \
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    access.plugin_id(),
                    access.package_digest(),
                    batch.source_instance_id,
                    batch.outbox_id,
                    payload_digest,
                    broker_revision,
                    now_ms,
                ],
            )?;
            Ok(OutboxAck {
                source_instance_id: batch.source_instance_id.clone(),
                outbox_id: batch.outbox_id.clone(),
                payload_digest: payload_digest.clone(),
                applied_broker_revision: broker_revision,
                accepted_operation_refs: Vec::new(),
            })
        })
    }

    fn prepare(
        &self,
        access: &VerifiedBrokerAccess,
        mutation: &OutboxMutation,
    ) -> BrokerResult<PreparedMutation> {
        match mutation {
            OutboxMutation::Entity { mutation } => match mutation {
                EntityMutation::Put {
                    contract,
                    id,
                    expected_revision,
                    data,
                } => {
                    validate_id(id)?;
                    let registered = self.resolve_owned(access, contract)?;
                    validate_instance(&registered.schema, data)?;
                    let data_json = canonical_json(data)?;
                    if data_json.len() > MAX_ENTITY_BYTES {
                        return Err(BrokerError::new(
                            "payload_too_large",
                            "entity exceeds byte quota",
                        ));
                    }
                    Ok(PreparedMutation::EntityPut {
                        contract: contract.clone(),
                        id: id.clone(),
                        expected_revision: *expected_revision,
                        data: data.clone(),
                        data_json,
                    })
                }
                EntityMutation::Delete {
                    contract,
                    id,
                    expected_revision,
                } => {
                    validate_id(id)?;
                    self.resolve_owned(access, contract)?;
                    Ok(PreparedMutation::EntityDelete {
                        contract: contract.clone(),
                        id: id.clone(),
                        expected_revision: *expected_revision,
                    })
                }
            },
            OutboxMutation::Event { event } => {
                for id in [
                    event.stream_id.as_str(),
                    event.event_id.as_str(),
                    event.subject.as_str(),
                    event.kind.as_str(),
                ] {
                    validate_id(id)?;
                }
                let registered = self.resolve_owned(access, &event.contract)?;
                validate_instance(&registered.schema, &event.data)?;
                let data_json = canonical_json(&event.data)?;
                if data_json.len() > MAX_EVENT_BYTES {
                    return Err(BrokerError::new(
                        "payload_too_large",
                        "event exceeds byte quota",
                    ));
                }
                let event_digest = sha256(&canonical_json(
                    &serde_json::to_value(event)
                        .map_err(|_| BrokerError::new("invalid_batch", "event encoding failed"))?,
                )?);
                Ok(PreparedMutation::Event {
                    event: event.clone(),
                    data_json,
                    event_digest,
                })
            }
        }
    }

    fn resolve_owned(
        &self,
        access: &VerifiedBrokerAccess,
        contract: &ContractRef,
    ) -> BrokerResult<ResolvedContract> {
        let registered = SchemaRegistry::new(self.database).exact(contract)?;
        authorize(access, &registered)?;
        Ok(registered)
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_entity_put(
    transaction: &Transaction<'_>,
    access: &VerifiedBrokerAccess,
    contract: &ContractRef,
    id: &str,
    expected_revision: u64,
    data: &Value,
    data_json: &[u8],
    broker_revision: u64,
    ordinal: i64,
    now_ms: i64,
) -> BrokerResult<()> {
    let current: i64 = transaction
        .query_row(
            "SELECT revision FROM broker_entities \
              WHERE contract_id = ?1 AND contract_version = ?2 AND entity_id = ?3",
            rusqlite::params![contract.id, contract.version.to_string(), id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if u64::try_from(current).ok() != Some(expected_revision) {
        return Err(BrokerError::new(
            "revision_conflict",
            "entity revision changed",
        ));
    }
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| BrokerError::new("revision_overflow", "entity revision exhausted"))?;
    let envelope = EntityEnvelope {
        contract: contract.clone(),
        id: id.into(),
        revision,
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
           contract_id, contract_version, entity_id, owner_plugin_id, owner_package_digest, \
           revision, broker_revision, state, data_json, updated_at_ms, stale \
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
            id,
            access.plugin_id(),
            access.package_digest(),
            revision,
            broker_revision,
            data_json,
            now_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO broker_entity_changes( \
           broker_revision, change_ordinal, contract_id, contract_version, entity_id, \
           entity_revision, change_kind, envelope_json \
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'put', ?7)",
        rusqlite::params![
            broker_revision,
            ordinal,
            contract.id,
            contract.version.to_string(),
            id,
            revision,
            envelope_json,
        ],
    )?;
    Ok(())
}

fn apply_entity_delete(
    transaction: &Transaction<'_>,
    contract: &ContractRef,
    id: &str,
    expected_revision: u64,
    broker_revision: u64,
    ordinal: i64,
    now_ms: i64,
) -> BrokerResult<()> {
    let row = transaction
        .query_row(
            "SELECT revision, data_json FROM broker_entities \
              WHERE contract_id = ?1 AND contract_version = ?2 AND entity_id = ?3",
            rusqlite::params![contract.id, contract.version.to_string(), id],
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
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| BrokerError::new("revision_overflow", "entity revision exhausted"))?;
    let envelope = EntityEnvelope {
        contract: contract.clone(),
        id: id.into(),
        revision,
        broker_revision,
        state: "deleted".into(),
        data: serde_json::from_slice(&row.1)
            .map_err(|_| BrokerError::new("broker_storage", "invalid stored entity"))?,
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
        rusqlite::params![contract.id, contract.version.to_string(), id],
    )?;
    transaction.execute(
        "INSERT INTO broker_entity_changes( \
           broker_revision, change_ordinal, contract_id, contract_version, entity_id, \
           entity_revision, change_kind, envelope_json \
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'delete', ?7)",
        rusqlite::params![
            broker_revision,
            ordinal,
            contract.id,
            contract.version.to_string(),
            id,
            revision,
            envelope_json,
        ],
    )?;
    Ok(())
}

fn apply_event(
    transaction: &Transaction<'_>,
    access: &VerifiedBrokerAccess,
    event: &EventMutation,
    data_json: &[u8],
    event_digest: &str,
    broker_revision: u64,
) -> BrokerResult<()> {
    if let Some(existing_digest) = transaction
        .query_row(
            "SELECT payload_digest FROM broker_events \
              WHERE contract_id = ?1 AND contract_version = ?2 \
                AND stream_id = ?3 AND event_id = ?4",
            rusqlite::params![
                event.contract.id,
                event.contract.version.to_string(),
                event.stream_id,
                event.event_id,
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        if existing_digest == event_digest {
            return Ok(());
        }
        return Err(BrokerError::new(
            "event_idempotency_conflict",
            "event ID was reused with different bytes",
        ));
    }
    transaction.execute(
        "INSERT OR IGNORE INTO broker_streams( \
           contract_id, contract_version, stream_id, next_seq, earliest_seq, latest_seq \
         ) VALUES(?1, ?2, ?3, 1, 1, 0)",
        rusqlite::params![
            event.contract.id,
            event.contract.version.to_string(),
            event.stream_id,
        ],
    )?;
    let seq: i64 = transaction.query_row(
        "SELECT next_seq FROM broker_streams \
          WHERE contract_id = ?1 AND contract_version = ?2 AND stream_id = ?3",
        rusqlite::params![
            event.contract.id,
            event.contract.version.to_string(),
            event.stream_id,
        ],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO broker_events( \
           contract_id, contract_version, stream_id, seq, event_id, subject, kind, \
           correlation_id, data_json, payload_digest, at_ms, broker_revision, \
           owner_plugin_id, owner_package_digest \
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            event.contract.id,
            event.contract.version.to_string(),
            event.stream_id,
            seq,
            event.event_id,
            event.subject,
            event.kind,
            event.correlation_id,
            data_json,
            event_digest,
            event.at_ms,
            broker_revision,
            access.plugin_id(),
            access.package_digest(),
        ],
    )?;
    transaction.execute(
        "UPDATE broker_streams SET next_seq = ?4, latest_seq = ?5 \
          WHERE contract_id = ?1 AND contract_version = ?2 AND stream_id = ?3",
        rusqlite::params![
            event.contract.id,
            event.contract.version.to_string(),
            event.stream_id,
            seq.checked_add(1)
                .ok_or_else(|| BrokerError::new("event_seq_overflow", "event seq exhausted"))?,
            seq,
        ],
    )?;
    Ok(())
}

fn validate_id(value: &str) -> BrokerResult<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(BrokerError::new(
            "invalid_identifier",
            "invalid outbox identifier",
        ));
    }
    Ok(())
}
