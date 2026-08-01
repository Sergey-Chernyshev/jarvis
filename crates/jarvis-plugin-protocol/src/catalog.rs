use std::collections::BTreeSet;
use std::fmt;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::json::{parse_bounded_json_with_limits, JsonLimits};
use crate::manifest::{Digest, PluginId, PublisherId, VersionRange};
use crate::package::{MacOsVersion, PackageSignatureV1, PackageTarget, SignatureAlgorithm};

pub const CATALOG_SCHEMA_VERSION: u32 = 1;
pub const CATALOG_SCHEMA_JSON: &[u8] = include_bytes!("../schema/plugin-catalog-v1.schema.json");

const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_CATALOG_DEPTH: usize = 32;
const MAX_CATALOG_NODES: usize = 100_000;
const MAX_CATALOG_STRING_BYTES: usize = 4096;
const MAX_SIGNATURES: usize = 64;
const MAX_ROOT_KEYS: usize = 64;
const MAX_LINEAGES: usize = 4096;
const MAX_LINEAGE_KEYS: usize = 64;
const MAX_PLUGIN_BINDINGS: usize = 4096;
const MAX_RELEASES: usize = 20_000;
const MAX_REVOCATIONS: usize = 20_000;
const MAX_KEY_ID_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 2048;
const MAX_TIMESTAMP_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogContractError(&'static str);

impl CatalogContractError {
    pub const fn code(&self) -> &'static str {
        self.0
    }

    const fn json() -> Self {
        Self("catalog_json")
    }

    const fn schema() -> Self {
        Self("catalog_schema")
    }

    const fn cardinality() -> Self {
        Self("catalog_cardinality")
    }

    const fn duplicate() -> Self {
        Self("catalog_duplicate")
    }

    const fn string() -> Self {
        Self("catalog_string")
    }

    const fn key() -> Self {
        Self("catalog_key")
    }
}

impl fmt::Display for CatalogContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for CatalogContractError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedCatalog {
    pub schema_version: u32,
    pub sequence: u64,
    pub issued_at: String,
    pub expires_at: String,
    pub previous_digest: Option<Digest>,
    pub payload: CatalogPayload,
    pub signatures: Vec<CatalogSignatureV1>,
}

impl SignedCatalog {
    pub fn parse(bytes: &[u8]) -> Result<Self, CatalogContractError> {
        let value = parse_bounded_json_with_limits(
            bytes,
            JsonLimits {
                max_bytes: MAX_CATALOG_BYTES,
                max_depth: MAX_CATALOG_DEPTH,
                max_nodes: MAX_CATALOG_NODES,
                max_string_bytes: MAX_CATALOG_STRING_BYTES,
            },
        )
        .map_err(|_| CatalogContractError::json())?;
        let catalog: Self =
            serde_json::from_value(value).map_err(|_| CatalogContractError::schema())?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), CatalogContractError> {
        if self.schema_version != CATALOG_SCHEMA_VERSION || self.sequence == 0 {
            return Err(CatalogContractError::schema());
        }
        validate_timestamp_shape(&self.issued_at)?;
        validate_timestamp_shape(&self.expires_at)?;
        bounded_nonempty(&self.signatures, MAX_SIGNATURES)?;
        let mut signature_ids = BTreeSet::new();
        for signature in &self.signatures {
            signature.validate()?;
            if !signature_ids.insert(signature.key_id.as_str()) {
                return Err(CatalogContractError::duplicate());
            }
        }
        self.payload.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogSignatureV1 {
    pub algorithm: SignatureAlgorithm,
    pub key_id: String,
    pub value: String,
}

impl CatalogSignatureV1 {
    pub fn new(
        algorithm: SignatureAlgorithm,
        key_id: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, CatalogContractError> {
        let signature = Self {
            algorithm,
            key_id: key_id.into(),
            value: value.into(),
        };
        signature.validate()?;
        Ok(signature)
    }

    pub fn validate(&self) -> Result<(), CatalogContractError> {
        PackageSignatureV1::new(self.algorithm, &self.key_id, &self.value)
            .map(|_| ())
            .map_err(|_| CatalogContractError::key())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogPayload {
    pub publisher_lineages: Vec<PublisherKeyLineage>,
    pub releases: Vec<CatalogRelease>,
    pub root_rotation: Option<RootRotationProposal>,
    pub revoked_package_digests: Vec<Digest>,
    pub revoked_publisher_keys: Vec<String>,
}

impl CatalogPayload {
    fn validate(&self) -> Result<(), CatalogContractError> {
        bounded(&self.publisher_lineages, MAX_LINEAGES)?;
        bounded(&self.releases, MAX_RELEASES)?;
        bounded(&self.revoked_package_digests, MAX_REVOCATIONS)?;
        bounded(&self.revoked_publisher_keys, MAX_REVOCATIONS)?;

        let mut lineage_ids = BTreeSet::new();
        let mut publisher_key_ids = BTreeSet::new();
        for lineage in &self.publisher_lineages {
            lineage.validate()?;
            if !lineage_ids.insert(lineage.id.as_str()) {
                return Err(CatalogContractError::duplicate());
            }
            for key in &lineage.keys {
                if !publisher_key_ids.insert(key.key_id.as_str()) {
                    return Err(CatalogContractError::duplicate());
                }
            }
        }

        let mut releases = BTreeSet::new();
        for release in &self.releases {
            release.validate()?;
            let identity = format!(
                "{}\0{}\0{}",
                release.plugin_id.as_str(),
                release.version,
                release.target.as_str()
            );
            if !releases.insert(identity) {
                return Err(CatalogContractError::duplicate());
            }
        }

        if let Some(rotation) = &self.root_rotation {
            rotation.validate()?;
        }

        let revoked_digests = self
            .revoked_package_digests
            .iter()
            .map(Digest::as_str)
            .collect::<BTreeSet<_>>();
        if revoked_digests.len() != self.revoked_package_digests.len() {
            return Err(CatalogContractError::duplicate());
        }
        let mut revoked_keys = BTreeSet::new();
        for key_id in &self.revoked_publisher_keys {
            validate_key_id(key_id)?;
            if !revoked_keys.insert(key_id.as_str()) {
                return Err(CatalogContractError::duplicate());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublisherKeyLineage {
    pub id: String,
    pub publisher: PublisherId,
    pub plugin_ids: Vec<PluginId>,
    pub keys: Vec<PublisherKey>,
}

impl PublisherKeyLineage {
    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_key_id(&self.id)?;
        bounded_nonempty(&self.plugin_ids, MAX_PLUGIN_BINDINGS)?;
        bounded_nonempty(&self.keys, MAX_LINEAGE_KEYS)?;
        let plugin_ids = self
            .plugin_ids
            .iter()
            .map(PluginId::as_str)
            .collect::<BTreeSet<_>>();
        if plugin_ids.len() != self.plugin_ids.len() {
            return Err(CatalogContractError::duplicate());
        }
        let mut key_ids = BTreeSet::new();
        for key in &self.keys {
            key.validate()?;
            if !key_ids.insert(key.key_id.as_str()) {
                return Err(CatalogContractError::duplicate());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublisherKey {
    pub key_id: String,
    pub algorithm: SignatureAlgorithm,
    pub public_key: String,
    pub valid_from: String,
    pub valid_until: String,
}

impl PublisherKey {
    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_key_id(&self.key_id)?;
        validate_public_key(&self.public_key)?;
        validate_timestamp_shape(&self.valid_from)?;
        validate_timestamp_shape(&self.valid_until)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootRotationProposal {
    pub threshold: u32,
    pub keys: Vec<RootKey>,
}

impl RootRotationProposal {
    pub fn validate(&self) -> Result<(), CatalogContractError> {
        bounded_nonempty(&self.keys, MAX_ROOT_KEYS)?;
        if self.threshold == 0
            || usize::try_from(self.threshold)
                .map(|threshold| threshold > self.keys.len())
                .unwrap_or(true)
        {
            return Err(CatalogContractError::cardinality());
        }
        let mut key_ids = BTreeSet::new();
        for key in &self.keys {
            key.validate()?;
            if !key_ids.insert(key.key_id.as_str()) {
                return Err(CatalogContractError::duplicate());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootKey {
    pub key_id: String,
    pub algorithm: SignatureAlgorithm,
    pub public_key: String,
    pub valid_from: String,
    pub valid_until: String,
}

impl RootKey {
    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_key_id(&self.key_id)?;
        validate_public_key(&self.public_key)?;
        validate_timestamp_shape(&self.valid_from)?;
        validate_timestamp_shape(&self.valid_until)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRelease {
    pub plugin_id: PluginId,
    pub publisher: PublisherId,
    pub version: Version,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub jarvis_range: VersionRange,
    pub plugin_api: u32,
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
    pub url: String,
    pub archive_digest: Digest,
    pub package_signature: PackageSignatureV1,
    pub revoked: bool,
}

impl CatalogRelease {
    fn validate(&self) -> Result<(), CatalogContractError> {
        validate_key_id(&self.publisher_key_id)?;
        validate_key_id(&self.publisher_lineage)?;
        if self.plugin_api == 0
            || self.url.is_empty()
            || self.url.len() > MAX_URL_BYTES
            || self.url.chars().any(char::is_control)
        {
            return Err(CatalogContractError::string());
        }
        self.package_signature
            .validate()
            .map_err(|_| CatalogContractError::key())
    }
}

fn bounded<T>(values: &[T], maximum: usize) -> Result<(), CatalogContractError> {
    if values.len() > maximum {
        return Err(CatalogContractError::cardinality());
    }
    Ok(())
}

fn bounded_nonempty<T>(values: &[T], maximum: usize) -> Result<(), CatalogContractError> {
    if values.is_empty() || values.len() > maximum {
        return Err(CatalogContractError::cardinality());
    }
    Ok(())
}

fn validate_key_id(value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty()
        || value.len() > MAX_KEY_ID_BYTES
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(CatalogContractError::string());
    }
    Ok(())
}

fn validate_public_key(value: &str) -> Result<(), CatalogContractError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(CatalogContractError::key());
    }
    Ok(())
}

fn validate_timestamp_shape(value: &str) -> Result<(), CatalogContractError> {
    if value.is_empty()
        || value.len() > MAX_TIMESTAMP_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CatalogContractError::string());
    }
    Ok(())
}
