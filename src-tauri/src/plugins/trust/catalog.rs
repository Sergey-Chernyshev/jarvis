#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use jarvis_plugin_protocol::catalog::{
    CatalogRelease, PublisherKey, RootKey, RootRotationProposal, SignedCatalog, CATALOG_SCHEMA_JSON,
};
use jarvis_plugin_protocol::json::{parse_bounded_json_with_limits, JsonLimits};
use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::package::{MacOsVersion, PackageTarget};
use semver::Version;
use serde::Deserialize;
use url::Url;

use super::signature::{catalog_digest, catalog_signature_message, verify_catalog_signature};
use super::TrustError;
use crate::plugins::manifest_v2::validate_bundled_schema;

const MAX_ROOT_CONFIG_BYTES: usize = 64 * 1024;
const MAX_ROOT_CONFIG_DEPTH: usize = 8;
const MAX_ROOT_CONFIG_NODES: usize = 512;
const MAX_ROOT_CONFIG_STRING_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootTrustConfig {
    schema_version: u32,
    threshold: u32,
    keys: Vec<RootKey>,
}

impl RootTrustConfig {
    pub fn parse(bytes: &[u8]) -> Result<Self, TrustError> {
        let value = parse_bounded_json_with_limits(
            bytes,
            JsonLimits {
                max_bytes: MAX_ROOT_CONFIG_BYTES,
                max_depth: MAX_ROOT_CONFIG_DEPTH,
                max_nodes: MAX_ROOT_CONFIG_NODES,
                max_string_bytes: MAX_ROOT_CONFIG_STRING_BYTES,
            },
        )
        .map_err(|_| TrustError::new("catalog_root_config"))?;
        let config: Self =
            serde_json::from_value(value).map_err(|_| TrustError::new("catalog_root_config"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn is_provisioned(&self) -> bool {
        !self.keys.is_empty()
    }

    fn from_rotation(rotation: &RootRotationProposal) -> Result<Self, TrustError> {
        let config = Self {
            schema_version: 1,
            threshold: rotation.threshold,
            keys: rotation.keys.clone(),
        };
        config.validate()?;
        Ok(config)
    }

    fn empty() -> Self {
        Self {
            schema_version: 1,
            threshold: 1,
            keys: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), TrustError> {
        if self.schema_version != 1 {
            return Err(TrustError::new("catalog_root_config"));
        }
        if self.keys.is_empty() {
            return if self.threshold == 1 {
                Ok(())
            } else {
                Err(TrustError::new("catalog_root_config"))
            };
        }

        RootRotationProposal {
            threshold: self.threshold,
            keys: self.keys.clone(),
        }
        .validate()
        .map_err(|_| TrustError::new("catalog_root_config"))?;
        for key in &self.keys {
            validate_key_window(&key.valid_from, &key.valid_until, "catalog_root_config")?;
        }
        Ok(())
    }

    fn key(&self, key_id: &str) -> Option<&RootKey> {
        self.keys.iter().find(|key| key.key_id == key_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCompatibility {
    jarvis_version: Version,
    plugin_api: u32,
    target: PackageTarget,
    macos_version: MacOsVersion,
}

impl CatalogCompatibility {
    pub fn parse(
        jarvis_version: &str,
        plugin_api: u32,
        target: PackageTarget,
        macos_version: &str,
    ) -> Result<Self, TrustError> {
        if plugin_api == 0 {
            return Err(TrustError::new("catalog_compatibility"));
        }
        Ok(Self {
            jarvis_version: Version::parse(jarvis_version)
                .map_err(|_| TrustError::new("catalog_compatibility"))?,
            plugin_api,
            target,
            macos_version: MacOsVersion::parse(macos_version)
                .map_err(|_| TrustError::new("catalog_compatibility"))?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogState {
    sequence: u64,
    digest: Option<Digest>,
    accepted_roots: RootTrustConfig,
}

impl CatalogState {
    pub fn new(accepted_roots: RootTrustConfig) -> Self {
        Self {
            sequence: 0,
            digest: None,
            accepted_roots,
        }
    }

    pub fn empty() -> Self {
        Self::new(RootTrustConfig::empty())
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn accepted_root_ids(&self) -> Vec<&str> {
        self.accepted_roots
            .keys
            .iter()
            .map(|key| key.key_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCatalog {
    sequence: u64,
    digest: Digest,
    compatibility: CatalogCompatibility,
    releases: Vec<VerifiedCatalogRelease>,
}

impl VerifiedCatalog {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    pub fn release(
        &self,
        plugin_id: &str,
        version: &str,
        target: PackageTarget,
    ) -> Result<&VerifiedCatalogRelease, TrustError> {
        let release = self.release_candidate(plugin_id, version, target)?;
        if let Some(error) = release.availability_error() {
            return Err(error);
        }
        if !release
            .release
            .jarvis_range
            .matches(&self.compatibility.jarvis_version)
            || release.release.plugin_api != self.compatibility.plugin_api
            || release.release.target != self.compatibility.target
            || compare_macos(
                &release.release.minimum_macos,
                &self.compatibility.macos_version,
            )
            .is_gt()
        {
            return Err(TrustError::new("package_incompatible"));
        }
        Ok(release)
    }

    pub(super) fn release_candidate(
        &self,
        plugin_id: &str,
        version: &str,
        target: PackageTarget,
    ) -> Result<&VerifiedCatalogRelease, TrustError> {
        let version = Version::parse(version).map_err(|_| TrustError::new("package_not_found"))?;
        self.releases
            .iter()
            .find(|candidate| {
                candidate.release.plugin_id.as_str() == plugin_id
                    && candidate.release.version == version
                    && candidate.release.target == target
            })
            .ok_or_else(|| TrustError::new("package_not_found"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCatalogRelease {
    release: CatalogRelease,
    publisher_key: PublisherKey,
    catalog_issued_at: DateTime<Utc>,
    catalog_expires_at: DateTime<Utc>,
    package_revoked: bool,
    publisher_key_revoked: bool,
    publisher_key_valid_at_catalog: bool,
}

impl VerifiedCatalogRelease {
    pub fn archive_digest(&self) -> &Digest {
        &self.release.archive_digest
    }

    pub(crate) fn release_record(&self) -> &CatalogRelease {
        &self.release
    }

    pub(crate) fn publisher_key(&self) -> &PublisherKey {
        &self.publisher_key
    }

    pub(crate) fn catalog_issued_at(&self) -> DateTime<Utc> {
        self.catalog_issued_at
    }

    pub(crate) fn catalog_expires_at(&self) -> DateTime<Utc> {
        self.catalog_expires_at
    }

    pub(crate) fn availability_error(&self) -> Option<TrustError> {
        if self.publisher_key_revoked {
            Some(TrustError::new("publisher_key_revoked"))
        } else if self.package_revoked {
            Some(TrustError::new("package_revoked"))
        } else if !self.publisher_key_valid_at_catalog {
            Some(TrustError::new("publisher_key_not_valid"))
        } else {
            None
        }
    }
}

pub fn verify_catalog_bytes(
    bytes: &[u8],
    now: DateTime<Utc>,
    compatibility: &CatalogCompatibility,
    state: &mut CatalogState,
) -> Result<VerifiedCatalog, TrustError> {
    let catalog = SignedCatalog::parse(bytes).map_err(|error| TrustError::new(error.code()))?;
    let value = serde_json::from_slice(bytes).map_err(|_| TrustError::new("catalog_schema"))?;
    validate_bundled_schema(CATALOG_SCHEMA_JSON, &value)
        .map_err(|_| TrustError::new("catalog_schema"))?;

    let issued_at = parse_timestamp(&catalog.issued_at, "catalog_time")?;
    let expires_at = parse_timestamp(&catalog.expires_at, "catalog_time")?;
    if issued_at >= expires_at {
        return Err(TrustError::new("catalog_time"));
    }
    if now < issued_at {
        return Err(TrustError::new("catalog_not_yet_valid"));
    }
    if now >= expires_at {
        return Err(TrustError::new("catalog_expired"));
    }
    if !state.accepted_roots.is_provisioned() {
        return Err(TrustError::new("catalog_trust_not_provisioned"));
    }

    let digest = catalog_digest(&catalog)?;
    validate_sequence(&catalog, &digest, state)?;

    if catalog.sequence != state.sequence {
        let root_horizon = verify_root_signatures(&catalog, now, &state.accepted_roots)?;
        if expires_at > root_horizon {
            return Err(TrustError::new("catalog_root_expiry"));
        }
    }

    let releases = validate_releases(&catalog, issued_at, expires_at, now)?;
    let verified = VerifiedCatalog {
        sequence: catalog.sequence,
        digest: digest.clone(),
        compatibility: compatibility.clone(),
        releases,
    };

    if catalog.sequence == state.sequence {
        return Ok(verified);
    }

    let accepted_roots = match &catalog.payload.root_rotation {
        Some(rotation) => RootTrustConfig::from_rotation(rotation)?,
        None => state.accepted_roots.clone(),
    };
    *state = CatalogState {
        sequence: catalog.sequence,
        digest: Some(digest),
        accepted_roots,
    };
    Ok(verified)
}

fn validate_sequence(
    catalog: &SignedCatalog,
    digest: &Digest,
    state: &CatalogState,
) -> Result<(), TrustError> {
    if catalog.sequence < state.sequence {
        return Err(TrustError::new("catalog_replayed"));
    }
    if catalog.sequence == state.sequence {
        return if state.digest.as_ref() == Some(digest) {
            Ok(())
        } else {
            Err(TrustError::new("catalog_conflict"))
        };
    }
    if state.sequence == 0 {
        if catalog.previous_digest.is_some() {
            return Err(TrustError::new("catalog_previous_digest"));
        }
    } else if catalog.previous_digest.as_ref() != state.digest.as_ref() {
        return Err(TrustError::new("catalog_previous_digest"));
    }
    Ok(())
}

fn verify_root_signatures(
    catalog: &SignedCatalog,
    now: DateTime<Utc>,
    current: &RootTrustConfig,
) -> Result<DateTime<Utc>, TrustError> {
    let proposed = catalog
        .payload
        .root_rotation
        .as_ref()
        .map(RootTrustConfig::from_rotation)
        .transpose()?;
    if let Some(proposed) = &proposed {
        for current_key in &current.keys {
            for proposed_key in &proposed.keys {
                if current_key.public_key == proposed_key.public_key && current_key != proposed_key
                {
                    return Err(TrustError::new("catalog_rotation_key_conflict"));
                }
            }
        }
    }
    let message = catalog_signature_message(catalog)?;
    let mut current_valid = BTreeMap::new();
    let mut proposed_valid = BTreeMap::new();

    for signature in &catalog.signatures {
        let current_key = current.key(&signature.key_id);
        let proposed_key = proposed
            .as_ref()
            .and_then(|roots| roots.key(&signature.key_id));
        let key = match (current_key, proposed_key) {
            (Some(current_key), Some(proposed_key)) if current_key != proposed_key => {
                return Err(TrustError::new("catalog_rotation_key_conflict"));
            }
            (Some(key), _) | (_, Some(key)) => key,
            (None, None) => return Err(TrustError::new("catalog_unknown_root")),
        };
        let (valid_from, valid_until) =
            validate_key_window(&key.valid_from, &key.valid_until, "catalog_key_time")?;
        if now < valid_from || now >= valid_until {
            return Err(TrustError::new("catalog_root_not_valid"));
        }
        verify_catalog_signature(&key.public_key, &message, &signature.value)?;
        if current_key.is_some() {
            current_valid.insert(key.public_key.as_str(), valid_until);
        }
        if proposed_key.is_some() {
            proposed_valid.insert(key.public_key.as_str(), valid_until);
        }
    }

    let current_horizon = quorum_horizon(&current_valid, current.threshold, "catalog_threshold")?;
    proposed
        .as_ref()
        .map(|proposed| {
            quorum_horizon(
                &proposed_valid,
                proposed.threshold,
                "catalog_rotation_threshold",
            )
            .map(|proposed_horizon| current_horizon.min(proposed_horizon))
        })
        .unwrap_or(Ok(current_horizon))
}

fn quorum_horizon(
    valid: &BTreeMap<&str, DateTime<Utc>>,
    threshold: u32,
    error_code: &'static str,
) -> Result<DateTime<Utc>, TrustError> {
    let threshold = usize::try_from(threshold).map_err(|_| TrustError::new(error_code))?;
    if threshold == 0 || valid.len() < threshold {
        return Err(TrustError::new(error_code));
    }
    let mut horizons = valid.values().copied().collect::<Vec<_>>();
    horizons.sort_unstable_by(|left, right| right.cmp(left));
    horizons
        .get(threshold - 1)
        .copied()
        .ok_or_else(|| TrustError::new(error_code))
}

fn validate_releases(
    catalog: &SignedCatalog,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Vec<VerifiedCatalogRelease>, TrustError> {
    let lineages = catalog
        .payload
        .publisher_lineages
        .iter()
        .map(|lineage| (lineage.id.as_str(), lineage))
        .collect::<BTreeMap<_, _>>();
    let revoked_digests = catalog
        .payload
        .revoked_package_digests
        .iter()
        .map(Digest::as_str)
        .collect::<BTreeSet<_>>();
    let revoked_keys = catalog
        .payload
        .revoked_publisher_keys
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut verified = Vec::with_capacity(catalog.payload.releases.len());

    for release in &catalog.payload.releases {
        let lineage = lineages
            .get(release.publisher_lineage.as_str())
            .ok_or_else(|| TrustError::new("publisher_lineage_invalid"))?;
        if lineage.publisher != release.publisher {
            return Err(TrustError::new("publisher_lineage_invalid"));
        }
        if !lineage
            .plugin_ids
            .iter()
            .any(|plugin_id| plugin_id == &release.plugin_id)
        {
            return Err(TrustError::new("publisher_key_not_bound"));
        }
        let key = lineage
            .keys
            .iter()
            .find(|key| key.key_id == release.publisher_key_id)
            .ok_or_else(|| TrustError::new("publisher_key_not_bound"))?;
        if release.package_signature.key_id != release.publisher_key_id {
            return Err(TrustError::new("publisher_key_not_bound"));
        }
        let publisher_key_revoked = revoked_keys.contains(release.publisher_key_id.as_str());
        let publisher_key_valid_at_catalog = key_is_valid(&key.valid_from, &key.valid_until, now)?;
        validate_release_url(&release.url)?;
        verified.push(VerifiedCatalogRelease {
            release: release.clone(),
            publisher_key: key.clone(),
            catalog_issued_at: issued_at,
            catalog_expires_at: expires_at,
            package_revoked: release.revoked
                || revoked_digests.contains(release.archive_digest.as_str()),
            publisher_key_revoked,
            publisher_key_valid_at_catalog,
        });
    }
    Ok(verified)
}

fn validate_release_url(value: &str) -> Result<(), TrustError> {
    let url = Url::parse(value).map_err(|_| TrustError::new("catalog_release_url"))?;
    if url.scheme() != "https"
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(TrustError::new("catalog_release_url"));
    }
    Ok(())
}

fn validate_key_window(
    valid_from: &str,
    valid_until: &str,
    code: &'static str,
) -> Result<(DateTime<Utc>, DateTime<Utc>), TrustError> {
    let from = parse_timestamp(valid_from, code)?;
    let until = parse_timestamp(valid_until, code)?;
    if from >= until {
        return Err(TrustError::new(code));
    }
    Ok((from, until))
}

fn key_is_valid(
    valid_from: &str,
    valid_until: &str,
    now: DateTime<Utc>,
) -> Result<bool, TrustError> {
    let (from, until) = validate_key_window(valid_from, valid_until, "catalog_key_time")?;
    Ok(from <= now && now < until)
}

fn parse_timestamp(value: &str, code: &'static str) -> Result<DateTime<Utc>, TrustError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| TrustError::new(code))
}

fn compare_macos(left: &MacOsVersion, right: &MacOsVersion) -> std::cmp::Ordering {
    for (left, right) in left.as_str().split('.').zip(right.as_str().split('.')) {
        let ordering = left.len().cmp(&right.len()).then_with(|| left.cmp(right));
        if ordering.is_ne() {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use chrono::{DateTime, Utc};
    use ed25519_dalek::{Signer, SigningKey};
    use jarvis_plugin_protocol::package::PackageTarget;
    use serde_json::{json, Value};

    use super::{verify_catalog_bytes, CatalogCompatibility, CatalogState, RootTrustConfig};
    use crate::plugins::trust::signature::catalog_signature_message;

    const ROOTS: &[u8] = include_bytes!("../../../tests/fixtures/plugin-trust/root-public.json");
    const CATALOG_1: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-1.json");
    const CATALOG_2: &[u8] =
        include_bytes!("../../../tests/fixtures/plugin-trust/catalog-seq-2-rotated.json");
    const PRODUCTION_ROOTS: &[u8] = include_bytes!("../../../resources/plugin-trust-roots.json");
    const SEED_HEX: &str =
        include_str!("../../../tests/fixtures/plugin-trust/package-test-signing-seed.hex");
    const PUBLIC_KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const ROOT_2_PUBLIC_KEY: &str =
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn compatibility() -> CatalogCompatibility {
        CatalogCompatibility::parse("0.4.0", 2, PackageTarget::DarwinArm64, "14.0.0").unwrap()
    }

    fn fixture_state() -> CatalogState {
        CatalogState::new(RootTrustConfig::parse(ROOTS).unwrap())
    }

    fn catalog_value(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    fn signing_key() -> SigningKey {
        let raw = SEED_HEX.trim().as_bytes();
        let mut seed = [0u8; 32];
        for (index, chunk) in raw.chunks_exact(2).enumerate() {
            seed[index] = (nibble(chunk[0]) << 4) | nibble(chunk[1]);
        }
        SigningKey::from_bytes(&seed)
    }

    fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("fixture seed is lowercase hex"),
        }
    }

    fn signed_catalog(mut value: Value, key_ids: &[&str]) -> Vec<u8> {
        value["signatures"] = json!([{
            "algorithm": "ed25519",
            "keyId": "placeholder",
            "value": "gDDYgr16HoixPzQjmuL8+CTds3bPmnZlxOHqex3+FifEyJqpD8PHzZT5HUWX4tQrUrijxOGqKbQu/ZaPOSAjCQ=="
        }]);
        let parsed = jarvis_plugin_protocol::catalog::SignedCatalog::parse(
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let signature = signing_key().sign(&catalog_signature_message(&parsed).unwrap());
        let signature = STANDARD.encode(signature.to_bytes());
        value["signatures"] = Value::Array(
            key_ids
                .iter()
                .map(|key_id| {
                    json!({
                        "algorithm": "ed25519",
                        "keyId": key_id,
                        "value": signature
                    })
                })
                .collect(),
        );
        serde_json::to_vec(&value).unwrap()
    }

    fn fixture_signatures(bytes: &[u8], key_ids: &[&str]) -> Vec<u8> {
        let mut value = catalog_value(bytes);
        value["signatures"]
            .as_array_mut()
            .unwrap()
            .retain(|signature| key_ids.contains(&signature["keyId"].as_str().unwrap()));
        serde_json::to_vec(&value).unwrap()
    }

    fn verify(
        bytes: &[u8],
        now: &str,
        state: &mut CatalogState,
    ) -> Result<super::VerifiedCatalog, crate::plugins::trust::TrustError> {
        verify_catalog_bytes(bytes, at(now), &compatibility(), state)
    }

    #[test]
    fn accepts_fresh_monotonic_catalog_and_binds_release_digest() {
        let mut state = fixture_state();
        let verified = verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let release = verified
            .release("dev.example.echo", "1.0.0", PackageTarget::DarwinArm64)
            .unwrap();
        assert_eq!(
            release.archive_digest().as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(verified.sequence(), 1);
        assert_eq!(
            verified.digest().as_str(),
            "sha256:382bdf240eb09eb2b7ba8fa8283ecb3aecb3f5a13b514abdfd36e65f1a7af472"
        );
        assert_eq!(state.sequence(), 1);
    }

    #[test]
    fn same_sequence_and_digest_are_idempotent() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let accepted = state.clone();
        verify(CATALOG_1, "2026-08-01T00:45:00Z", &mut state).unwrap();
        assert_eq!(state, accepted);
    }

    #[test]
    fn rejects_expired_replayed_conflicting_and_wrong_previous_catalogs_atomically() {
        let mut expired = catalog_value(CATALOG_1);
        expired["expiresAt"] = json!("2026-08-01T00:10:00Z");
        let expired = signed_catalog(expired, &["jarvis.root:1"]);
        let mut state = fixture_state();
        let initial = state.clone();
        assert_eq!(
            verify(&expired, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_expired"
        );
        assert_eq!(state, initial);

        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        verify(CATALOG_2, "2026-08-01T01:30:00Z", &mut state).unwrap();
        let sequence_two = state.clone();
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_replayed"
        );
        assert_eq!(state, sequence_two);

        let mut conflict_state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut conflict_state).unwrap();
        let accepted = conflict_state.clone();
        let mut conflict = catalog_value(CATALOG_1);
        conflict["payload"]["releases"][0]["url"] =
            json!("https://plugins.jarvis.example/conflict.jarvis-plugin");
        let conflict = signed_catalog(conflict, &["jarvis.root:1"]);
        assert_eq!(
            verify(&conflict, "2026-08-01T00:30:00Z", &mut conflict_state)
                .unwrap_err()
                .code(),
            "catalog_conflict"
        );
        assert_eq!(conflict_state, accepted);

        let mut wrong_previous = catalog_value(CATALOG_2);
        wrong_previous["previousDigest"] =
            json!("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let wrong_previous = serde_json::to_vec(&wrong_previous).unwrap();
        assert_eq!(
            verify(&wrong_previous, "2026-08-01T01:30:00Z", &mut conflict_state,)
                .unwrap_err()
                .code(),
            "catalog_previous_digest"
        );
        assert_eq!(conflict_state, accepted);
    }

    #[test]
    fn rejects_unknown_and_insufficient_root_thresholds() {
        let mut unknown = catalog_value(CATALOG_1);
        let unknown = signed_catalog(unknown.take(), &["unknown.root:1"]);
        let mut state = fixture_state();
        assert_eq!(
            verify(&unknown, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_unknown_root"
        );

        let roots = RootTrustConfig::parse(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "threshold": 2,
                "keys": [
                    {
                        "keyId": "jarvis.root:1",
                        "algorithm": "ed25519",
                        "publicKey": PUBLIC_KEY,
                        "validFrom": "2026-01-01T00:00:00Z",
                        "validUntil": "2027-08-01T00:00:00Z"
                    },
                    {
                        "keyId": "jarvis.root:2",
                        "algorithm": "ed25519",
                        "publicKey": ROOT_2_PUBLIC_KEY,
                        "validFrom": "2026-01-01T00:00:00Z",
                        "validUntil": "2027-08-01T00:00:00Z"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut threshold_state = CatalogState::new(roots);
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut threshold_state,)
                .unwrap_err()
                .code(),
            "catalog_threshold"
        );
    }

    #[test]
    fn catalog_lifetime_cannot_outlive_the_root_quorum() {
        let roots = RootTrustConfig::parse(
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "threshold": 1,
                "keys": [{
                    "keyId": "jarvis.root:1",
                    "algorithm": "ed25519",
                    "publicKey": PUBLIC_KEY,
                    "validFrom": "2026-01-01T00:00:00Z",
                    "validUntil": "2026-08-01T01:00:00Z"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut state = CatalogState::new(roots);
        let initial = state.clone();
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_root_expiry"
        );
        assert_eq!(state, initial);
    }

    #[test]
    fn invalid_signature_is_rejected_before_unsigned_release_semantics() {
        let mut unsigned = catalog_value(CATALOG_1);
        unsigned["payload"]["releases"][0]["publisherLineage"] = json!("missing.lineage");
        let unsigned = serde_json::to_vec(&unsigned).unwrap();
        let mut state = fixture_state();
        let initial = state.clone();
        assert_eq!(
            verify(&unsigned, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_signature_invalid"
        );
        assert_eq!(state, initial);
    }

    #[test]
    fn rotation_requires_old_and_new_thresholds_before_replacing_roots() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let accepted = state.clone();

        let old_only = fixture_signatures(CATALOG_2, &["jarvis.root:1"]);
        assert_eq!(
            verify(&old_only, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_rotation_threshold"
        );
        assert_eq!(state, accepted);

        let new_only = fixture_signatures(CATALOG_2, &["jarvis.root:2"]);
        assert_eq!(
            verify(&new_only, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_threshold"
        );
        assert_eq!(state, accepted);

        verify(CATALOG_2, "2026-08-01T01:30:00Z", &mut state).unwrap();
        assert_eq!(state.sequence(), 2);
        assert_eq!(state.accepted_root_ids(), vec!["jarvis.root:2"]);
    }

    #[test]
    fn rotation_rejects_same_verifying_key_under_different_ids() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();
        let accepted = state.clone();
        let mut duplicated = catalog_value(CATALOG_2);
        duplicated["payload"]["rootRotation"]["keys"][0]["publicKey"] = json!(PUBLIC_KEY);
        let duplicated = serde_json::to_vec(&duplicated).unwrap();
        assert_eq!(
            verify(&duplicated, "2026-08-01T01:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_rotation_key_conflict"
        );
        assert_eq!(state, accepted);
    }

    #[test]
    fn publisher_key_must_be_bound_to_plugin_and_revocation_blocks_release() {
        let mut unbound = catalog_value(CATALOG_1);
        unbound["payload"]["publisherLineages"][0]["pluginIds"] = json!(["dev.example.other"]);
        let unbound = signed_catalog(unbound, &["jarvis.root:1"]);
        let mut state = fixture_state();
        let initial = state.clone();
        assert_eq!(
            verify(&unbound, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "publisher_key_not_bound"
        );
        assert_eq!(state, initial);

        let mut revoked = catalog_value(CATALOG_1);
        revoked["payload"]["releases"][0]["revoked"] = json!(true);
        let revoked = signed_catalog(revoked, &["jarvis.root:1"]);
        let verified = verify(&revoked, "2026-08-01T00:30:00Z", &mut state).unwrap();
        assert_eq!(
            verified
                .release("dev.example.echo", "1.0.0", PackageTarget::DarwinArm64,)
                .unwrap_err()
                .code(),
            "package_revoked"
        );
    }

    #[test]
    fn publisher_revocation_advances_state_and_blocks_retained_release() {
        let mut state = fixture_state();
        verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state).unwrap();

        let mut revoked = catalog_value(CATALOG_1);
        revoked["sequence"] = json!(2);
        revoked["issuedAt"] = json!("2026-08-01T01:00:00Z");
        revoked["expiresAt"] = json!("2026-08-03T00:00:00Z");
        revoked["previousDigest"] =
            json!("sha256:382bdf240eb09eb2b7ba8fa8283ecb3aecb3f5a13b514abdfd36e65f1a7af472");
        revoked["payload"]["revokedPublisherKeys"] = json!(["example.release:1"]);
        let revoked = signed_catalog(revoked, &["jarvis.root:1"]);
        let verified = verify(&revoked, "2026-08-01T01:30:00Z", &mut state).unwrap();
        assert_eq!(state.sequence(), 2);
        assert_eq!(
            verified
                .release("dev.example.echo", "1.0.0", PackageTarget::DarwinArm64,)
                .unwrap_err()
                .code(),
            "publisher_key_revoked"
        );
    }

    #[test]
    fn oversized_numeric_macos_component_is_incompatible_without_panicking() {
        let mut catalog = catalog_value(CATALOG_1);
        catalog["payload"]["releases"][0]["minimumMacos"] = json!("18446744073709551616.0.0");
        let catalog = signed_catalog(catalog, &["jarvis.root:1"]);
        let mut state = fixture_state();
        let verified = verify(&catalog, "2026-08-01T00:30:00Z", &mut state).unwrap();
        assert_eq!(
            verified
                .release("dev.example.echo", "1.0.0", PackageTarget::DarwinArm64,)
                .unwrap_err()
                .code(),
            "package_incompatible"
        );
    }

    #[test]
    fn empty_production_roots_fail_closed_without_test_material() {
        let roots = RootTrustConfig::parse(PRODUCTION_ROOTS).unwrap();
        assert!(!roots.is_provisioned());
        let mut state = CatalogState::empty();
        assert_eq!(
            verify(CATALOG_1, "2026-08-01T00:30:00Z", &mut state)
                .unwrap_err()
                .code(),
            "catalog_trust_not_provisioned"
        );
        assert!(!std::str::from_utf8(PRODUCTION_ROOTS)
            .unwrap()
            .contains(SEED_HEX.trim()));
    }
}
