mod access;
mod database;
mod entity_store;
mod event_store;
mod outbox;
mod private_storage;
mod schema_registry;

use std::fmt;
use std::path::Path;

pub(crate) use access::VerifiedBrokerAccess;
pub(crate) use entity_store::{EntityChangePage, EntityStore};
pub(crate) use event_store::{EventChangePage, EventStore};
pub(crate) use outbox::OutboxIngress;
pub(crate) use private_storage::{PrivateStorage, PrivateValue};
pub(crate) use schema_registry::{
    ContractRegistration, RegisteredContract, ResolvedContract, SchemaRegistry,
};

use database::BrokerDatabase;

#[derive(Debug)]
pub(crate) struct BrokerError {
    code: &'static str,
    message: String,
}

impl BrokerError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BrokerError {}

impl From<rusqlite::Error> for BrokerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new("broker_storage", format!("broker storage failure: {error}"))
    }
}

pub(crate) type BrokerResult<T> = Result<T, BrokerError>;

pub(crate) struct Broker {
    database: BrokerDatabase,
}

impl Broker {
    pub(crate) fn open(root: &Path, now_ms: i64) -> BrokerResult<Self> {
        Ok(Self {
            database: BrokerDatabase::open(root, now_ms)?,
        })
    }

    pub(crate) fn schemas(&self) -> SchemaRegistry<'_> {
        SchemaRegistry::new(&self.database)
    }

    pub(crate) fn entities(&self) -> EntityStore<'_> {
        EntityStore::new(&self.database)
    }

    pub(crate) fn events(&self) -> EventStore<'_> {
        EventStore::new(&self.database)
    }

    pub(crate) fn private_storage(&self) -> PrivateStorage<'_> {
        PrivateStorage::new(&self.database)
    }

    pub(crate) fn outbox(&self) -> OutboxIngress<'_> {
        OutboxIngress::new(&self.database)
    }

    pub(crate) fn revision(&self) -> BrokerResult<u64> {
        self.database.revision()
    }

    pub(crate) fn shutdown(&self) -> BrokerResult<()> {
        self.database.shutdown()
    }
}

pub(super) fn canonical_json(value: &serde_json::Value) -> BrokerResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|_| BrokerError::new("invalid_payload", "value is not canonicalizable JSON"))
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use semver::{Version, VersionReq};
    use serde_json::json;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_root(tag: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        // macOS exposes TMPDIR below `/var`, which itself is a symlink to
        // `/private/var`. SQLite's OPEN_NOFOLLOW correctly rejects any symlink
        // component, so tests must start from the canonical temp parent just
        // like the production profile starts from a real `/Users` path.
        std::env::temp_dir()
            .canonicalize()
            .expect("system temp directory must be canonicalizable")
            .join(format!("jarvis-broker-{tag}-{}-{id}", std::process::id()))
    }

    fn contract() -> ContractRegistration {
        ContractRegistration {
            contract_id: "dev.jarvis.test/item@1.0.0".into(),
            version: Version::parse("1.0.0").unwrap(),
            schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "required": ["name"],
                "properties": { "name": { "type": "string", "maxLength": 64 } }
            }),
            publisher_plugin_id: "dev.jarvis.test".into(),
            publisher_key_lineage: "test.release".into(),
            installed_package_digest: format!("sha256:{}", "a".repeat(64)),
            sensitivity: "internal".into(),
            visibility: "granted".into(),
            retention: "persistent".into(),
        }
    }

    fn access() -> VerifiedBrokerAccess {
        VerifiedBrokerAccess::from_activation(
            "dev.jarvis.test",
            "test.release",
            &format!("sha256:{}", "a".repeat(64)),
            7,
        )
        .unwrap()
    }

    fn foreign_access(generation: u64) -> VerifiedBrokerAccess {
        VerifiedBrokerAccess::from_activation(
            "dev.jarvis.foreign",
            "foreign.release",
            &format!("sha256:{}", "b".repeat(64)),
            generation,
        )
        .unwrap()
    }

    fn create_owner_private_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn expect_open_error(root: &Path) -> BrokerError {
        match Broker::open(root, 1) {
            Ok(_) => panic!("broker unexpectedly opened an unsafe path"),
            Err(error) => error,
        }
    }

    #[test]
    fn broker_rejects_preplanted_symlink_wal_and_shm_sidecars() {
        for suffix in ["-wal", "-shm"] {
            let root = temp_root("symlink-sidecar");
            create_owner_private_dir(&root);
            let victim = root.with_extension(format!("victim{}", &suffix[1..]));
            fs::write(&victim, b"must stay unchanged").unwrap();
            symlink(&victim, root.join(format!("broker-v1.sqlite3{suffix}"))).unwrap();

            let error = expect_open_error(&root);
            assert_eq!(error.code(), "broker_path");
            assert_eq!(fs::read(&victim).unwrap(), b"must stay unchanged");

            fs::remove_dir_all(root).unwrap();
            fs::remove_file(victim).unwrap();
        }
    }

    #[test]
    fn broker_rejects_preplanted_hardlink_wal_and_shm_sidecars() {
        for suffix in ["-wal", "-shm"] {
            let root = temp_root("hardlink-sidecar");
            create_owner_private_dir(&root);
            let victim = root.with_extension(format!("victim{}", &suffix[1..]));
            fs::write(&victim, b"must stay unchanged").unwrap();
            fs::hard_link(&victim, root.join(format!("broker-v1.sqlite3{suffix}"))).unwrap();

            let error = expect_open_error(&root);
            assert_eq!(error.code(), "broker_path");
            assert_eq!(fs::read(&victim).unwrap(), b"must stay unchanged");

            fs::remove_dir_all(root).unwrap();
            fs::remove_file(victim).unwrap();
        }
    }

    #[test]
    fn broker_rejects_a_symlinked_root_ancestor_before_creating_the_database() {
        let base = temp_root("symlink-ancestor");
        let real_parent = base.join("real");
        let real_root = real_parent.join("broker");
        create_owner_private_dir(&real_root);
        symlink(&real_parent, base.join("linked")).unwrap();
        let linked_root = base.join("linked").join("broker");

        let error = expect_open_error(&linked_root);
        assert_eq!(error.code(), "broker_path");
        assert!(!real_root.join("broker-v1.sqlite3").exists());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn revoke_waits_for_an_admitted_write_to_commit() {
        let root = temp_root("revoke-write");
        let broker = Arc::new(Broker::open(&root, 1).unwrap());
        let operation_access = access();
        let revoke_access = operation_access.clone();
        let denied_access = operation_access.clone();
        let operation_broker = Arc::clone(&broker);
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let operation = std::thread::spawn(move || {
            operation_broker
                .database
                .with_access_write(&operation_access, |transaction| {
                    transaction.execute(
                        "UPDATE broker_meta SET opened_at_ms = 777 WHERE singleton = 1",
                        [],
                    )?;
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
        });
        admitted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker = std::thread::spawn(move || {
            revoke_access.revoke();
            revoked_tx.send(()).unwrap();
        });
        assert!(matches!(
            revoked_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        release_tx.send(()).unwrap();
        operation.join().unwrap().unwrap();
        revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        revoker.join().unwrap();
        let opened_at_ms: i64 = broker
            .database
            .with_read(|connection| {
                Ok(connection.query_row(
                    "SELECT opened_at_ms FROM broker_meta WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(opened_at_ms, 777);
        assert_eq!(
            broker
                .database
                .with_access_read(&denied_access, |_| Ok(()))
                .unwrap_err()
                .code(),
            "principal_revoked"
        );

        broker.shutdown().unwrap();
        drop(broker);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revoke_waits_for_an_admitted_read_to_complete() {
        let root = temp_root("revoke-read");
        let broker = Arc::new(Broker::open(&root, 1).unwrap());
        let operation_access = access();
        let revoke_access = operation_access.clone();
        let operation_broker = Arc::clone(&broker);
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let operation = std::thread::spawn(move || {
            operation_broker
                .database
                .with_access_read(&operation_access, |connection| {
                    let opened_at_ms = connection.query_row(
                        "SELECT opened_at_ms FROM broker_meta WHERE singleton = 1",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?;
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(opened_at_ms)
                })
        });
        admitted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker = std::thread::spawn(move || {
            revoke_access.revoke();
            revoked_tx.send(()).unwrap();
        });
        assert!(matches!(
            revoked_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        release_tx.send(()).unwrap();
        assert_eq!(operation.join().unwrap().unwrap(), 1);
        revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        revoker.join().unwrap();

        broker.shutdown().unwrap();
        drop(broker);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immutable_contract_and_entity_cas_share_monotonic_revision() {
        let root = temp_root("entity");
        let broker = Broker::open(&root, 10).unwrap();
        let owner = access();
        let registered = broker.schemas().register(&owner, contract(), 11).unwrap();
        assert_eq!(
            broker
                .schemas()
                .resolve("dev.jarvis.test/item", &VersionReq::parse("^1.0").unwrap())
                .unwrap()
                .schema_digest,
            registered.schema_digest
        );

        let contract_ref = registered.contract_ref();
        let created = broker
            .entities()
            .put(
                &owner,
                contract_ref.clone(),
                "one",
                0,
                json!({"name": "A"}),
                12,
            )
            .unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(created.broker_revision, 1);

        let conflict = broker
            .entities()
            .put(
                &owner,
                contract_ref.clone(),
                "one",
                0,
                json!({"name": "B"}),
                13,
            )
            .unwrap_err();
        assert_eq!(conflict.code(), "revision_conflict");
        assert_eq!(broker.revision().unwrap(), 1);

        let updated = broker
            .entities()
            .put(
                &owner,
                contract_ref.clone(),
                "one",
                1,
                json!({"name": "B"}),
                14,
            )
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.broker_revision, 2);
        broker.shutdown().unwrap();

        let reopened = Broker::open(&root, 15).unwrap();
        let snapshot = reopened
            .entities()
            .snapshot(&owner, &contract_ref, 16)
            .unwrap();
        assert_eq!(snapshot.snapshot_revision, 2);
        assert_eq!(snapshot.entities, vec![updated]);
        reopened.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schema_mutation_owner_spoof_and_invalid_payload_fail_closed() {
        let root = temp_root("security");
        let broker = Broker::open(&root, 20).unwrap();
        let owner = access();
        assert_eq!(
            broker
                .schemas()
                .register(&foreign_access(1), contract(), 20)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        let registered = broker.schemas().register(&owner, contract(), 21).unwrap();

        let mut changed = contract();
        changed.schema["properties"]["name"]["maxLength"] = json!(65);
        assert_eq!(
            broker
                .schemas()
                .register(&owner, changed, 22)
                .unwrap_err()
                .code(),
            "contract_immutable"
        );

        let foreign = foreign_access(1);
        assert_eq!(
            broker
                .entities()
                .put(
                    &foreign,
                    registered.contract_ref(),
                    "one",
                    0,
                    json!({"name": "A"}),
                    23
                )
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        assert_eq!(
            broker
                .entities()
                .put(
                    &owner,
                    registered.contract_ref(),
                    "one",
                    0,
                    json!({"name": 42}),
                    24
                )
                .unwrap_err()
                .code(),
            "schema_rejected"
        );
        assert_eq!(broker.revision().unwrap(), 0);
        broker.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn events_are_idempotent_and_private_storage_is_signer_scoped() {
        use jarvis_plugin_protocol::broker::EventMutation;

        let root = temp_root("events-storage");
        let broker = Broker::open(&root, 30).unwrap();
        let owner = access();
        let registered = broker.schemas().register(&owner, contract(), 31).unwrap();
        let event = EventMutation {
            contract: registered.contract_ref(),
            stream_id: "project-one".into(),
            event_id: "event-one".into(),
            subject: "one".into(),
            kind: "updated".into(),
            correlation_id: None,
            data: json!({"name": "A"}),
            at_ms: 32,
        };
        let first = broker.events().append(&owner, event.clone()).unwrap();
        let replay = broker.events().append(&owner, event).unwrap();
        assert_eq!(first, replay);
        assert_eq!(first.seq, 1);
        assert_eq!(
            broker
                .events()
                .changes_after(&owner, &registered.contract_ref(), 0, &[], 16)
                .unwrap()
                .changes
                .len(),
            1
        );

        let value = broker
            .private_storage()
            .set(&access(), "settings", 0, json!({"enabled": true}), 33)
            .unwrap();
        assert_eq!(value.revision, 1);
        assert_eq!(
            broker.private_storage().get(&access(), "settings").unwrap(),
            Some(value)
        );
        let foreign_signer = VerifiedBrokerAccess::from_activation(
            "dev.jarvis.test",
            "other.release",
            &format!("sha256:{}", "a".repeat(64)),
            8,
        )
        .unwrap();
        assert_eq!(
            broker
                .private_storage()
                .get(&foreign_signer, "settings")
                .unwrap(),
            None
        );
        let revoked = access();
        revoked.revoke();
        assert_eq!(
            broker
                .private_storage()
                .get(&revoked, "settings")
                .unwrap_err()
                .code(),
            "principal_revoked"
        );
        broker.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn outbox_batch_is_atomic_and_replay_returns_original_revision() {
        use jarvis_plugin_protocol::broker::{EntityMutation, OutboxBatch, OutboxMutation};

        let root = temp_root("outbox");
        let broker = Broker::open(&root, 40).unwrap();
        let owner = access();
        let registered = broker.schemas().register(&owner, contract(), 41).unwrap();
        let batch = OutboxBatch {
            source_instance_id: "provider-one".into(),
            outbox_id: "outbox-one".into(),
            mutations: vec![
                OutboxMutation::Entity {
                    mutation: EntityMutation::Put {
                        contract: registered.contract_ref(),
                        id: "one".into(),
                        expected_revision: 0,
                        data: json!({"name": "A"}),
                    },
                },
                OutboxMutation::Entity {
                    mutation: EntityMutation::Put {
                        contract: registered.contract_ref(),
                        id: "two".into(),
                        expected_revision: 0,
                        data: json!({"name": "B"}),
                    },
                },
            ],
        };
        let applied = broker.outbox().apply(&owner, batch.clone(), 42).unwrap();
        let replay = broker.outbox().apply(&owner, batch, 43).unwrap();
        assert_eq!(applied, replay);
        assert_eq!(applied.applied_broker_revision, 1);
        let snapshot = broker
            .entities()
            .snapshot(&owner, &registered.contract_ref(), 16)
            .unwrap();
        assert_eq!(snapshot.snapshot_revision, 1);
        assert_eq!(snapshot.entities.len(), 2);
        assert!(snapshot
            .entities
            .iter()
            .all(|entity| entity.broker_revision == 1));
        broker.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_and_cursors_require_an_exact_live_contract_grant() {
        use jarvis_plugin_protocol::broker::EventMutation;

        let root = temp_root("read-grants");
        let broker = Broker::open(&root, 50).unwrap();
        let owner = access();
        let consumer = foreign_access(3);
        let registered = broker.schemas().register(&owner, contract(), 51).unwrap();
        let contract_ref = registered.contract_ref();
        broker
            .entities()
            .put(
                &owner,
                contract_ref.clone(),
                "one",
                0,
                json!({"name": "A"}),
                52,
            )
            .unwrap();
        broker
            .events()
            .append(
                &owner,
                EventMutation {
                    contract: contract_ref.clone(),
                    stream_id: "project-one".into(),
                    event_id: "event-one".into(),
                    subject: "one".into(),
                    kind: "updated".into(),
                    correlation_id: None,
                    data: json!({"name": "A"}),
                    at_ms: 53,
                },
            )
            .unwrap();

        assert_eq!(
            broker
                .entities()
                .snapshot(&consumer, &contract_ref, 16)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        assert_eq!(
            broker
                .entities()
                .changes_after(&consumer, &contract_ref, 0, 16)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        assert_eq!(
            broker
                .events()
                .changes_after(&consumer, &contract_ref, 0, &[], 16)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        assert_eq!(
            broker
                .events()
                .bind_cursor(&consumer, "foreign-cursor", &contract_ref, 1, 54)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );

        consumer.grant_contract_from(&owner, &contract_ref).unwrap();
        broker
            .events()
            .bind_cursor(&owner, "owner-private-cursor", &contract_ref, 1, 55)
            .unwrap();
        assert_eq!(
            broker
                .events()
                .bind_cursor(&consumer, "owner-private-cursor", &contract_ref, 1, 55)
                .unwrap_err()
                .code(),
            "cursor_forbidden"
        );
        assert_eq!(
            broker
                .entities()
                .snapshot(&consumer, &contract_ref, 16)
                .unwrap()
                .entities
                .len(),
            1
        );
        broker
            .events()
            .bind_cursor(&consumer, "foreign-cursor", &contract_ref, 1, 55)
            .unwrap();
        assert_eq!(
            broker
                .events()
                .poll_cursor(&consumer, "foreign-cursor", &[], 16)
                .unwrap()
                .changes
                .len(),
            1
        );

        let next_generation = foreign_access(4);
        next_generation
            .grant_contract_from(&owner, &contract_ref)
            .unwrap();
        assert_eq!(
            broker
                .events()
                .poll_cursor(&next_generation, "foreign-cursor", &[], 16)
                .unwrap_err()
                .code(),
            "cursor_forbidden"
        );

        let next_provider_generation = VerifiedBrokerAccess::from_activation(
            "dev.jarvis.test",
            "test.release",
            &format!("sha256:{}", "a".repeat(64)),
            8,
        )
        .unwrap();
        broker
            .schemas()
            .register(&next_provider_generation, contract(), 56)
            .unwrap();
        assert_eq!(
            broker
                .events()
                .poll_cursor(&consumer, "foreign-cursor", &[], 16)
                .unwrap_err()
                .code(),
            "contract_forbidden"
        );
        consumer.revoke();
        assert_eq!(
            broker
                .events()
                .poll_cursor(&consumer, "foreign-cursor", &[], 16)
                .unwrap_err()
                .code(),
            "principal_revoked"
        );
        broker.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subject_filter_is_applied_before_event_page_limit() {
        use jarvis_plugin_protocol::broker::EventMutation;

        let root = temp_root("event-filter");
        let broker = Broker::open(&root, 60).unwrap();
        let owner = access();
        let registered = broker.schemas().register(&owner, contract(), 61).unwrap();
        let contract_ref = registered.contract_ref();
        for (index, subject) in ["skip", "skip", "wanted"].into_iter().enumerate() {
            broker
                .events()
                .append(
                    &owner,
                    EventMutation {
                        contract: contract_ref.clone(),
                        stream_id: "project-one".into(),
                        event_id: format!("event-{index}"),
                        subject: subject.into(),
                        kind: "updated".into(),
                        correlation_id: None,
                        data: json!({"name": subject}),
                        at_ms: 62 + index as i64,
                    },
                )
                .unwrap();
        }

        let page = broker
            .events()
            .changes_after(&owner, &contract_ref, 0, &["wanted".to_string()], 1)
            .unwrap();
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].event.subject, "wanted");

        broker
            .events()
            .bind_cursor(&owner, "owner-cursor", &contract_ref, 1, 70)
            .unwrap();
        let page = broker
            .events()
            .poll_cursor(&owner, "owner-cursor", &["wanted".to_string()], 1)
            .unwrap();
        assert_eq!(page.changes.len(), 1);
        assert_eq!(page.changes[0].event.subject, "wanted");
        broker.shutdown().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
