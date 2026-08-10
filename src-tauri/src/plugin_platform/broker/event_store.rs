use jarvis_plugin_protocol::broker::{
    ContractRef, EventChange, EventEnvelope, EventMutation, MAX_EVENT_BYTES,
};
use rusqlite::{OptionalExtension, Transaction};

use super::access::VerifiedBrokerAccess;
use super::database::{allocate_revision, BrokerDatabase};
use super::entity_store::{authorize, authorize_read, authorize_read_binding};
use super::schema_registry::{validate_instance, SchemaRegistry};
use super::{canonical_json, sha256, BrokerError, BrokerResult};

const MAX_PAGE: u32 = 128;
const MAX_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EventChangePage {
    pub snapshot_revision: u64,
    pub changes: Vec<EventChange>,
}

pub(crate) struct EventStore<'a> {
    database: &'a BrokerDatabase,
}

impl<'a> EventStore<'a> {
    pub(super) fn new(database: &'a BrokerDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn append(
        &self,
        access: &VerifiedBrokerAccess,
        event: EventMutation,
    ) -> BrokerResult<EventEnvelope> {
        for token in [
            event.stream_id.as_str(),
            event.event_id.as_str(),
            event.subject.as_str(),
            event.kind.as_str(),
        ] {
            validate_token(token)?;
        }
        if let Some(correlation) = event.correlation_id.as_deref() {
            validate_token(correlation)?;
        }
        let data_json = canonical_json(&event.data)?;
        if data_json.len() > MAX_EVENT_BYTES {
            return Err(BrokerError::new(
                "payload_too_large",
                "event exceeds byte quota",
            ));
        }
        let registered = SchemaRegistry::new(self.database).exact(&event.contract)?;
        authorize(access, &registered)?;
        validate_instance(&registered.schema, &event.data)?;
        let payload_digest = event_digest(&event, &data_json)?;

        self.database.with_access_write(access, |transaction| {
            let existing = transaction
                .query_row(
                    "SELECT seq, subject, kind, correlation_id, data_json, payload_digest, at_ms \
                       FROM broker_events \
                      WHERE contract_id = ?1 AND contract_version = ?2 \
                        AND stream_id = ?3 AND event_id = ?4",
                    rusqlite::params![
                        event.contract.id,
                        event.contract.version.to_string(),
                        event.stream_id,
                        event.event_id,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Vec<u8>>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()?;
            if let Some(existing) = existing {
                if existing.5 != payload_digest {
                    return Err(BrokerError::new(
                        "event_idempotency_conflict",
                        "event ID was reused with different bytes",
                    ));
                }
                return Ok(EventEnvelope {
                    contract: event.contract.clone(),
                    stream_id: event.stream_id.clone(),
                    event_id: event.event_id.clone(),
                    seq: u64::try_from(existing.0)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid event seq"))?,
                    subject: existing.1,
                    kind: existing.2,
                    correlation_id: existing.3,
                    data: serde_json::from_slice(&existing.4)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid stored event"))?,
                    at_ms: existing.6,
                });
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
            let next_seq: i64 = transaction.query_row(
                "SELECT next_seq FROM broker_streams \
                  WHERE contract_id = ?1 AND contract_version = ?2 AND stream_id = ?3",
                rusqlite::params![
                    event.contract.id,
                    event.contract.version.to_string(),
                    event.stream_id,
                ],
                |row| row.get(0),
            )?;
            let broker_revision = allocate_revision(transaction)?;
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
                    next_seq,
                    event.event_id,
                    event.subject,
                    event.kind,
                    event.correlation_id,
                    data_json,
                    payload_digest,
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
                    next_seq.checked_add(1).ok_or_else(|| BrokerError::new(
                        "event_seq_overflow",
                        "event seq exhausted"
                    ))?,
                    next_seq,
                ],
            )?;
            Ok(EventEnvelope {
                contract: event.contract.clone(),
                stream_id: event.stream_id.clone(),
                event_id: event.event_id.clone(),
                seq: u64::try_from(next_seq)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid event seq"))?,
                subject: event.subject.clone(),
                kind: event.kind.clone(),
                correlation_id: event.correlation_id.clone(),
                data: event.data.clone(),
                at_ms: event.at_ms,
            })
        })
    }

    pub(crate) fn changes_after(
        &self,
        access: &VerifiedBrokerAccess,
        contract: &ContractRef,
        cursor: u64,
        subjects: &[String],
        limit: u32,
    ) -> BrokerResult<EventChangePage> {
        let limit = validate_limit(limit)?;
        if subjects.len() > MAX_PAGE as usize {
            return Err(BrokerError::new(
                "invalid_selector",
                "too many event subjects",
            ));
        }
        for subject in subjects {
            validate_token(subject)?;
        }
        let registered = SchemaRegistry::new(self.database).exact(contract)?;
        authorize_read(access, &registered)?;
        self.database.with_access_read(access, |connection| {
            let snapshot_revision: i64 = connection.query_row(
                "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            let mut query = String::from(
                "SELECT broker_revision, stream_id, event_id, seq, subject, kind, \
                        correlation_id, data_json, at_ms \
                   FROM broker_events \
                  WHERE contract_id = ? AND contract_version = ? AND broker_revision > ?",
            );
            append_subject_filter(&mut query, subjects.len());
            query.push_str(" ORDER BY broker_revision LIMIT ?");
            let mut parameters = vec![
                rusqlite::types::Value::Text(contract.id.clone()),
                rusqlite::types::Value::Text(contract.version.to_string()),
                rusqlite::types::Value::Integer(i64::try_from(cursor).map_err(|_| {
                    BrokerError::new("invalid_cursor", "event cursor exceeds SQLite range")
                })?),
            ];
            parameters.extend(subjects.iter().cloned().map(rusqlite::types::Value::Text));
            parameters.push(rusqlite::types::Value::Integer(limit));
            let mut statement = connection.prepare(&query)?;
            let rows =
                statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Vec<u8>>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                })?;
            let mut changes = Vec::new();
            for row in rows {
                let row = row?;
                changes.push(EventChange {
                    cursor: u64::try_from(row.0)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid event cursor"))?,
                    event: EventEnvelope {
                        contract: contract.clone(),
                        stream_id: row.1,
                        event_id: row.2,
                        seq: u64::try_from(row.3)
                            .map_err(|_| BrokerError::new("broker_storage", "invalid event seq"))?,
                        subject: row.4,
                        kind: row.5,
                        correlation_id: row.6,
                        data: serde_json::from_slice(&row.7).map_err(|_| {
                            BrokerError::new("broker_storage", "invalid stored event")
                        })?,
                        at_ms: row.8,
                    },
                });
            }
            Ok(EventChangePage {
                snapshot_revision: u64::try_from(snapshot_revision)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid snapshot revision"))?,
                changes,
            })
        })
    }

    pub(crate) fn bind_cursor(
        &self,
        access: &VerifiedBrokerAccess,
        cursor_id: &str,
        contract: &ContractRef,
        next_broker_revision: u64,
        now_ms: i64,
    ) -> BrokerResult<()> {
        validate_token(cursor_id)?;
        let registered = SchemaRegistry::new(self.database).exact(contract)?;
        authorize_read(access, &registered)?;
        let delivered_through = next_broker_revision.saturating_sub(1);
        self.database.with_access_write(access, |transaction| {
            let existing = transaction
                .query_row(
                    "SELECT consumer_plugin_id, consumer_signer_lineage, \
                            consumer_package_digest, contract_id, contract_version, grant_revision \
                       FROM broker_cursors WHERE cursor_id = ?1",
                    [cursor_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?;
            if let Some(existing) = existing {
                if existing
                    != (
                        access.plugin_id().to_string(),
                        access.signer_lineage().to_string(),
                        access.package_digest().to_string(),
                        contract.id.clone(),
                        contract.version.to_string(),
                        i64::try_from(access.activation_generation()).map_err(|_| {
                            BrokerError::new("invalid_principal", "activation generation overflow")
                        })?,
                    )
                {
                    return Err(BrokerError::new(
                        "cursor_forbidden",
                        "cursor is bound to another exact activation",
                    ));
                }
                return Ok(());
            }
            transaction.execute(
                "INSERT INTO broker_cursors( \
                   cursor_id, consumer_plugin_id, consumer_signer_lineage, \
                   consumer_package_digest, contract_id, contract_version, \
                   next_broker_revision, delivered_through, last_ack_ms, grant_revision \
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    cursor_id,
                    access.plugin_id(),
                    access.signer_lineage(),
                    access.package_digest(),
                    contract.id,
                    contract.version.to_string(),
                    next_broker_revision,
                    delivered_through,
                    now_ms,
                    access.activation_generation(),
                ],
            )?;
            Ok(())
        })
    }

    pub(crate) fn poll_cursor(
        &self,
        access: &VerifiedBrokerAccess,
        cursor_id: &str,
        subjects: &[String],
        limit: u32,
    ) -> BrokerResult<EventChangePage> {
        validate_token(cursor_id)?;
        access.ensure_live()?;
        let limit = validate_limit(limit)?;
        if subjects.len() > MAX_PAGE as usize {
            return Err(BrokerError::new(
                "invalid_selector",
                "too many event subjects",
            ));
        }
        for subject in subjects {
            validate_token(subject)?;
        }
        self.database.with_access_write(access, |transaction| {
            let binding = transaction
                .query_row(
                    "SELECT consumer_plugin_id, consumer_signer_lineage, \
                            consumer_package_digest, contract_id, contract_version, \
                            next_broker_revision, grant_revision \
                       FROM broker_cursors WHERE cursor_id = ?1",
                    [cursor_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| BrokerError::new("cursor_not_found", "cursor does not exist"))?;
            if binding.0 != access.plugin_id()
                || binding.1 != access.signer_lineage()
                || binding.2 != access.package_digest()
                || u64::try_from(binding.6).ok() != Some(access.activation_generation())
            {
                return Err(BrokerError::new(
                    "cursor_forbidden",
                    "cursor belongs to another exact activation",
                ));
            }
            let version = authorize_cursor_contract(transaction, access, &binding.3, &binding.4)?;
            let next_revision = u64::try_from(binding.5)
                .map_err(|_| BrokerError::new("broker_storage", "invalid stored cursor"))?;
            let snapshot_revision: i64 = transaction.query_row(
                "SELECT broker_revision FROM broker_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            let mut query = String::from(
                "SELECT broker_revision, stream_id, event_id, seq, subject, kind, \
                        correlation_id, data_json, at_ms, schema_digest \
                   FROM broker_events \
                   JOIN broker_contracts \
                     ON broker_contracts.contract_id = broker_events.contract_id \
                    AND broker_contracts.version = broker_events.contract_version \
                  WHERE broker_events.contract_id = ? \
                    AND broker_events.contract_version = ? \
                    AND broker_revision >= ?",
            );
            append_subject_filter(&mut query, subjects.len());
            query.push_str(" ORDER BY broker_revision LIMIT ?");
            let mut parameters = vec![
                rusqlite::types::Value::Text(binding.3.clone()),
                rusqlite::types::Value::Text(binding.4.clone()),
                rusqlite::types::Value::Integer(i64::try_from(next_revision).map_err(|_| {
                    BrokerError::new("invalid_cursor", "event cursor exceeds SQLite range")
                })?),
            ];
            parameters.extend(subjects.iter().cloned().map(rusqlite::types::Value::Text));
            parameters.push(rusqlite::types::Value::Integer(limit));
            let mut statement = transaction.prepare(&query)?;
            let rows =
                statement.query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Vec<u8>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                })?;
            let mut changes = Vec::new();
            for row in rows {
                let row = row?;
                changes.push(EventChange {
                    cursor: u64::try_from(row.0)
                        .map_err(|_| BrokerError::new("broker_storage", "invalid event cursor"))?,
                    event: EventEnvelope {
                        contract: ContractRef {
                            id: binding.3.clone(),
                            version: version.clone(),
                            schema_digest: row.9,
                        },
                        stream_id: row.1,
                        event_id: row.2,
                        seq: u64::try_from(row.3)
                            .map_err(|_| BrokerError::new("broker_storage", "invalid event seq"))?,
                        subject: row.4,
                        kind: row.5,
                        correlation_id: row.6,
                        data: serde_json::from_slice(&row.7).map_err(|_| {
                            BrokerError::new("broker_storage", "invalid stored event")
                        })?,
                        at_ms: row.8,
                    },
                });
            }
            drop(statement);
            if let Some(delivered) = changes.last().map(|change| change.cursor) {
                transaction.execute(
                    "UPDATE broker_cursors SET delivered_through = ?2 WHERE cursor_id = ?1",
                    rusqlite::params![cursor_id, delivered],
                )?;
            }
            Ok(EventChangePage {
                snapshot_revision: u64::try_from(snapshot_revision)
                    .map_err(|_| BrokerError::new("broker_storage", "invalid snapshot revision"))?,
                changes,
            })
        })
    }

    pub(crate) fn acknowledge_cursor(
        &self,
        access: &VerifiedBrokerAccess,
        cursor_id: &str,
        delivered_through: u64,
        now_ms: i64,
    ) -> BrokerResult<()> {
        validate_token(cursor_id)?;
        access.ensure_live()?;
        self.database.with_access_write(access, |transaction| {
            let row = transaction
                .query_row(
                    "SELECT consumer_plugin_id, consumer_signer_lineage, \
                            consumer_package_digest, contract_id, contract_version, \
                            next_broker_revision, delivered_through, grant_revision \
                       FROM broker_cursors WHERE cursor_id = ?1",
                    [cursor_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| BrokerError::new("cursor_not_found", "cursor does not exist"))?;
            if row.0 != access.plugin_id()
                || row.1 != access.signer_lineage()
                || row.2 != access.package_digest()
                || u64::try_from(row.7).ok() != Some(access.activation_generation())
            {
                return Err(BrokerError::new(
                    "cursor_forbidden",
                    "cursor belongs to another activation",
                ));
            }
            authorize_cursor_contract(transaction, access, &row.3, &row.4)?;
            let next = delivered_through
                .checked_add(1)
                .ok_or_else(|| BrokerError::new("cursor_overflow", "cursor exhausted"))?;
            let current = u64::try_from(row.5)
                .map_err(|_| BrokerError::new("broker_storage", "invalid stored cursor"))?;
            if next < current {
                return Ok(());
            }
            let delivered = u64::try_from(row.6)
                .map_err(|_| BrokerError::new("broker_storage", "invalid delivered cursor"))?;
            if delivered_through > delivered {
                return Err(BrokerError::new(
                    "cursor_ahead",
                    "cursor cannot acknowledge undelivered revision",
                ));
            }
            transaction.execute(
                "UPDATE broker_cursors SET next_broker_revision = ?2, last_ack_ms = ?3 \
                  WHERE cursor_id = ?1",
                rusqlite::params![cursor_id, next, now_ms],
            )?;
            Ok(())
        })
    }
}

fn authorize_cursor_contract(
    transaction: &Transaction<'_>,
    access: &VerifiedBrokerAccess,
    contract_id: &str,
    contract_version: &str,
) -> BrokerResult<semver::Version> {
    let binding = transaction
        .query_row(
            "SELECT schema_digest, publisher_plugin_id, publisher_key_lineage, \
                    installed_package_digest, publisher_activation_generation \
               FROM broker_contracts WHERE contract_id = ?1 AND version = ?2",
            rusqlite::params![contract_id, contract_version],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| BrokerError::new("contract_not_found", "cursor contract does not exist"))?;
    let version = semver::Version::parse(contract_version)
        .map_err(|_| BrokerError::new("broker_storage", "invalid cursor contract"))?;
    let generation = u64::try_from(binding.4)
        .map_err(|_| BrokerError::new("broker_storage", "invalid publisher generation"))?;
    authorize_read_binding(
        access,
        &ContractRef {
            id: contract_id.into(),
            version: version.clone(),
            schema_digest: binding.0,
        },
        &binding.1,
        &binding.2,
        &binding.3,
        generation,
    )?;
    Ok(version)
}

fn append_subject_filter(query: &mut String, subject_count: usize) {
    if subject_count == 0 {
        return;
    }
    query.push_str(" AND subject IN (");
    for index in 0..subject_count {
        if index > 0 {
            query.push(',');
        }
        query.push('?');
    }
    query.push(')');
}

fn event_digest(event: &EventMutation, data_json: &[u8]) -> BrokerResult<String> {
    let value = serde_json::json!({
        "contract": event.contract,
        "streamId": event.stream_id,
        "eventId": event.event_id,
        "subject": event.subject,
        "kind": event.kind,
        "correlationId": event.correlation_id,
        "data": serde_json::from_slice::<serde_json::Value>(data_json)
            .map_err(|_| BrokerError::new("invalid_payload", "event data is invalid"))?,
        "atMs": event.at_ms,
    });
    Ok(sha256(&canonical_json(&value)?))
}

fn validate_token(value: &str) -> BrokerResult<()> {
    if value.is_empty()
        || value.len() > MAX_TOKEN_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(BrokerError::new(
            "invalid_identifier",
            "invalid broker identifier",
        ));
    }
    Ok(())
}

fn validate_limit(limit: u32) -> BrokerResult<i64> {
    if limit == 0 || limit > MAX_PAGE {
        return Err(BrokerError::new("invalid_limit", "invalid broker limit"));
    }
    Ok(i64::from(limit))
}
