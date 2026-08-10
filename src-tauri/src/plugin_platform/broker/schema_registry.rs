use std::sync::Arc;

use jarvis_plugin_protocol::broker::ContractRef;
use jarvis_plugin_protocol::manifest::{ContractId, Digest, PluginId};
use jsonschema::{Draft, JSONSchema, SchemaResolver, SchemaResolverError};
use semver::{Version, VersionReq};
use serde_json::Value;
use url::Url;

use super::access::VerifiedBrokerAccess;
use super::database::BrokerDatabase;
use super::{canonical_json, sha256, BrokerError, BrokerResult};

#[derive(Clone, Debug)]
pub(crate) struct ContractRegistration {
    pub contract_id: String,
    pub version: Version,
    pub schema: Value,
    pub publisher_plugin_id: String,
    pub publisher_key_lineage: String,
    pub installed_package_digest: String,
    pub sensitivity: String,
    pub visibility: String,
    pub retention: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegisteredContract {
    pub contract_id: String,
    pub version: Version,
    pub schema_digest: String,
}

impl RegisteredContract {
    pub(crate) fn contract_ref(&self) -> ContractRef {
        ContractRef {
            id: self.contract_id.clone(),
            version: self.version.clone(),
            schema_digest: self.schema_digest.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedContract {
    pub contract_id: String,
    pub version: Version,
    pub schema_digest: String,
    pub publisher_plugin_id: String,
    pub publisher_key_lineage: String,
    pub installed_package_digest: String,
    pub publisher_activation_generation: u64,
    pub schema: Value,
}

pub(crate) struct SchemaRegistry<'a> {
    database: &'a BrokerDatabase,
}

impl<'a> SchemaRegistry<'a> {
    pub(super) fn new(database: &'a BrokerDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn register(
        &self,
        access: &VerifiedBrokerAccess,
        registration: ContractRegistration,
        now_ms: i64,
    ) -> BrokerResult<RegisteredContract> {
        access.ensure_live()?;
        validate_registration(&registration)?;
        authorize_registration(access, &registration)?;
        validate_schema(&registration.schema)?;
        let schema_json = canonical_json(&registration.schema)?;
        let schema_digest = sha256(&schema_json);
        let version = registration.version.to_string();
        let contract_name = registration
            .contract_id
            .rsplit_once('@')
            .map(|(name, _)| name.to_owned())
            .expect("validated manifest contract id contains a version");

        self.database.with_access_write(access, |transaction| {
            let existing = transaction.query_row(
                "SELECT schema_digest, publisher_plugin_id, publisher_key_lineage, \
                        installed_package_digest, publisher_activation_generation, schema_json \
                   FROM broker_contracts WHERE contract_id = ?1 AND version = ?2",
                rusqlite::params![contract_name, version],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                },
            );
            match existing {
                Ok(existing)
                    if (
                        existing.0.as_str(),
                        existing.1.as_str(),
                        existing.2.as_str(),
                        existing.3.as_str(),
                        existing.5.as_slice(),
                    ) == (
                        schema_digest.as_str(),
                        registration.publisher_plugin_id.as_str(),
                        registration.publisher_key_lineage.as_str(),
                        registration.installed_package_digest.as_str(),
                        schema_json.as_slice(),
                    ) =>
                {
                    if u64::try_from(existing.4).ok() != Some(access.activation_generation()) {
                        transaction.execute(
                            "UPDATE broker_contracts \
                                SET publisher_activation_generation = ?3 \
                              WHERE contract_id = ?1 AND version = ?2",
                            rusqlite::params![
                                contract_name,
                                version,
                                access.activation_generation(),
                            ],
                        )?;
                    }
                }
                Ok(_) => {
                    return Err(BrokerError::new(
                        "contract_immutable",
                        "contract version is already bound to different verified bytes",
                    ));
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    transaction.execute(
                        "INSERT INTO broker_contracts( \
                           contract_id, version, schema_digest, publisher_plugin_id, \
                           publisher_key_lineage, publisher_activation_generation, sensitivity, \
                           visibility, retention, schema_json, installed_package_digest, \
                           created_at_ms \
                         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        rusqlite::params![
                            contract_name,
                            version,
                            schema_digest,
                            registration.publisher_plugin_id,
                            registration.publisher_key_lineage,
                            access.activation_generation(),
                            registration.sensitivity,
                            registration.visibility,
                            registration.retention,
                            schema_json,
                            registration.installed_package_digest,
                            now_ms,
                        ],
                    )?;
                }
                Err(error) => return Err(error.into()),
            }
            Ok(RegisteredContract {
                contract_id: contract_name.clone(),
                version: registration.version.clone(),
                schema_digest: schema_digest.clone(),
            })
        })
    }

    pub(crate) fn resolve(
        &self,
        contract_id: &str,
        requirement: &VersionReq,
    ) -> BrokerResult<ResolvedContract> {
        self.database.with_read(|connection| {
            let mut statement = connection.prepare(
                "SELECT version, schema_digest, publisher_plugin_id, publisher_key_lineage, \
                        installed_package_digest, publisher_activation_generation, schema_json \
                   FROM broker_contracts WHERE contract_id = ?1 ORDER BY version ASC",
            )?;
            let rows = statement.query_map([contract_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                ))
            })?;
            let mut matches = Vec::new();
            for row in rows {
                let row = row?;
                let version = Version::parse(&row.0).map_err(|_| {
                    BrokerError::new("broker_storage", "stored contract version is invalid")
                })?;
                if requirement.matches(&version) {
                    let schema = serde_json::from_slice(&row.6).map_err(|_| {
                        BrokerError::new("broker_storage", "stored contract schema is invalid")
                    })?;
                    matches.push(ResolvedContract {
                        contract_id: contract_id.into(),
                        version,
                        schema_digest: row.1,
                        publisher_plugin_id: row.2,
                        publisher_key_lineage: row.3,
                        installed_package_digest: row.4,
                        publisher_activation_generation: u64::try_from(row.5).map_err(|_| {
                            BrokerError::new(
                                "broker_storage",
                                "stored publisher generation is invalid",
                            )
                        })?,
                        schema,
                    });
                }
            }
            matches
                .into_iter()
                .max_by(|left, right| left.version.cmp(&right.version))
                .ok_or_else(|| {
                    BrokerError::new("contract_not_found", "no compatible contract version")
                })
        })
    }

    pub(super) fn exact(&self, contract: &ContractRef) -> BrokerResult<ResolvedContract> {
        let resolved = self.resolve(
            &contract.id,
            &VersionReq::parse(&format!("={}", contract.version))
                .map_err(|_| BrokerError::new("invalid_contract", "invalid exact version"))?,
        )?;
        if resolved.schema_digest != contract.schema_digest {
            return Err(BrokerError::new(
                "contract_digest_mismatch",
                "contract schema digest differs from registered version",
            ));
        }
        Ok(resolved)
    }
}

fn authorize_registration(
    access: &VerifiedBrokerAccess,
    registration: &ContractRegistration,
) -> BrokerResult<()> {
    let expected_namespace = format!("{}/", access.plugin_id());
    if registration.publisher_plugin_id != access.plugin_id()
        || registration.publisher_key_lineage != access.signer_lineage()
        || registration.installed_package_digest != access.package_digest()
        || !registration.contract_id.starts_with(&expected_namespace)
    {
        return Err(BrokerError::new(
            "contract_forbidden",
            "verified activation does not own the contract registration",
        ));
    }
    Ok(())
}

fn validate_registration(registration: &ContractRegistration) -> BrokerResult<()> {
    ContractId::new(registration.contract_id.clone())
        .map_err(|_| BrokerError::new("invalid_contract", "invalid contract id"))?;
    PluginId::new(registration.publisher_plugin_id.clone())
        .map_err(|_| BrokerError::new("invalid_contract", "invalid publisher plugin id"))?;
    Digest::new(registration.installed_package_digest.clone())
        .map_err(|_| BrokerError::new("invalid_contract", "invalid package digest"))?;
    let version = registration.version.to_string();
    let suffix = registration
        .contract_id
        .rsplit_once('@')
        .map(|(_, version)| version);
    if suffix != Some(version.as_str())
        || registration.publisher_key_lineage.is_empty()
        || registration.publisher_key_lineage.len() > 256
    {
        return Err(BrokerError::new(
            "invalid_contract",
            "contract binding is not canonical",
        ));
    }
    for value in [
        &registration.sensitivity,
        &registration.visibility,
        &registration.retention,
    ] {
        if value.is_empty() || value.len() > 64 {
            return Err(BrokerError::new(
                "invalid_contract",
                "contract policy value is invalid",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_instance(schema: &Value, instance: &Value) -> BrokerResult<()> {
    let compiled = compile_schema(schema)?;
    if !compiled.is_valid(instance) {
        return Err(BrokerError::new(
            "schema_rejected",
            "payload does not match the immutable contract",
        ));
    }
    Ok(())
}

fn validate_schema(schema: &Value) -> BrokerResult<()> {
    compile_schema(schema).map(|_| ())
}

fn compile_schema(schema: &Value) -> BrokerResult<JSONSchema> {
    reject_external_references(schema)?;
    let mut options = JSONSchema::options();
    options
        .with_draft(Draft::Draft202012)
        .with_resolver(DenyExternalSchemaResolver);
    options
        .compile(schema)
        .map_err(|_| BrokerError::new("schema_rejected", "contract schema is invalid"))
}

fn reject_external_references(schema: &Value) -> BrokerResult<()> {
    let mut stack = vec![schema];
    let mut nodes = 0_usize;
    while let Some(value) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > 20_000 {
            return Err(BrokerError::new(
                "schema_rejected",
                "contract schema exceeds node quota",
            ));
        }
        match value {
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => {
                if let Some(reference) = values.get("$ref") {
                    if reference.as_str().map(|value| value.starts_with("#/")) != Some(true) {
                        return Err(BrokerError::new(
                            "schema_rejected",
                            "external schema references are disabled",
                        ));
                    }
                }
                stack.extend(values.values());
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct DenyExternalSchemaResolver;

impl SchemaResolver for DenyExternalSchemaResolver {
    fn resolve(
        &self,
        _root_schema: &Value,
        _url: &Url,
        _original_reference: &str,
    ) -> Result<Arc<Value>, SchemaResolverError> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "external JSON Schema resolution is disabled",
        )
        .into())
    }
}
