use std::fmt;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::manifest::{Digest, MountMode, PermissionId, PermissionScope, PluginId};
use crate::package::PackageTarget;

pub const INSTALL_RECEIPT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptContractError(&'static str);

impl ReceiptContractError {
    pub fn code(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ReceiptContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ReceiptContractError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantedPermission {
    pub id: PermissionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<PermissionScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<MountMode>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallReceipt {
    pub schema_version: u32,
    pub plugin_id: PluginId,
    pub version: Version,
    pub package_digest: Digest,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub target: PackageTarget,
    pub source: InstallSource,
    pub enabled: bool,
    pub granted_permissions: Vec<GrantedPermission>,
    pub native_trust_digest: Option<Digest>,
    pub installed_at_ms: i64,
    pub generation: u64,
    pub state_schema_version: u32,
    pub rollback_compatible_through: u32,
    pub previous: Option<ReceiptSummary>,
}

impl InstallReceipt {
    pub fn summary(&self) -> ReceiptSummary {
        ReceiptSummary {
            plugin_id: self.plugin_id.clone(),
            version: self.version.clone(),
            package_digest: self.package_digest.clone(),
            publisher_key_id: self.publisher_key_id.clone(),
            publisher_lineage: self.publisher_lineage.clone(),
            target: self.target,
            source: self.source,
            enabled: self.enabled,
            granted_permissions: self.granted_permissions.clone(),
            native_trust_digest: self.native_trust_digest.clone(),
            installed_at_ms: self.installed_at_ms,
            generation: self.generation,
            state_schema_version: self.state_schema_version,
            rollback_compatible_through: self.rollback_compatible_through,
        }
    }

    pub fn validate(&self) -> Result<(), ReceiptContractError> {
        if self.schema_version != INSTALL_RECEIPT_SCHEMA_VERSION {
            return Err(ReceiptContractError("receipt_schema"));
        }
        validate_receipt_fields(
            &self.plugin_id,
            &self.publisher_key_id,
            &self.publisher_lineage,
            self.source,
            self.native_trust_digest.as_ref(),
            self.generation,
            self.state_schema_version,
            self.rollback_compatible_through,
        )?;
        if let Some(previous) = &self.previous {
            previous.validate()?;
            if previous.plugin_id != self.plugin_id || previous.generation >= self.generation {
                return Err(ReceiptContractError("receipt_previous"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptSummary {
    pub plugin_id: PluginId,
    pub version: Version,
    pub package_digest: Digest,
    pub publisher_key_id: String,
    pub publisher_lineage: String,
    pub target: PackageTarget,
    pub source: InstallSource,
    pub enabled: bool,
    pub granted_permissions: Vec<GrantedPermission>,
    pub native_trust_digest: Option<Digest>,
    pub installed_at_ms: i64,
    pub generation: u64,
    pub state_schema_version: u32,
    pub rollback_compatible_through: u32,
}

impl ReceiptSummary {
    pub fn validate(&self) -> Result<(), ReceiptContractError> {
        validate_receipt_fields(
            &self.plugin_id,
            &self.publisher_key_id,
            &self.publisher_lineage,
            self.source,
            self.native_trust_digest.as_ref(),
            self.generation,
            self.state_schema_version,
            self.rollback_compatible_through,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    Catalog,
    LocalPackage,
    DeveloperSnapshot,
    LegacyBundledV1,
}

fn validate_receipt_fields(
    plugin_id: &PluginId,
    publisher_key_id: &str,
    publisher_lineage: &str,
    source: InstallSource,
    native_trust_digest: Option<&Digest>,
    generation: u64,
    state_schema_version: u32,
    rollback_compatible_through: u32,
) -> Result<(), ReceiptContractError> {
    if publisher_key_id.is_empty()
        || publisher_key_id.len() > 256
        || publisher_lineage.is_empty()
        || publisher_lineage.len() > 256
        || generation == 0
        || state_schema_version == 0
        || rollback_compatible_through == 0
        || rollback_compatible_through > state_schema_version
    {
        return Err(ReceiptContractError("receipt_schema"));
    }
    if source == InstallSource::LegacyBundledV1 {
        if plugin_id.as_str() != "agent-vm" {
            return Err(ReceiptContractError("receipt_legacy_plugin_id"));
        }
        if native_trust_digest.is_some() {
            return Err(ReceiptContractError("receipt_legacy_native_trust"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InstallReceipt, InstallSource, ReceiptSummary, INSTALL_RECEIPT_SCHEMA_VERSION};
    use crate::manifest::{Digest, PluginId};
    use crate::package::PackageTarget;
    use semver::Version;

    fn digest(fill: char) -> Digest {
        Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn receipt(plugin_id: &str, source: InstallSource) -> InstallReceipt {
        InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            plugin_id: PluginId::new(plugin_id).unwrap(),
            version: Version::parse("1.2.3").unwrap(),
            package_digest: digest('a'),
            publisher_key_id: "key-1".into(),
            publisher_lineage: "lineage-1".into(),
            target: PackageTarget::DarwinArm64,
            source,
            enabled: true,
            granted_permissions: Vec::new(),
            native_trust_digest: None,
            installed_at_ms: 42,
            generation: 7,
            state_schema_version: 3,
            rollback_compatible_through: 2,
            previous: None,
        }
    }

    #[test]
    fn summary_preserves_the_exact_previous_generation() {
        let receipt = receipt("dev.example.echo", InstallSource::Catalog);

        assert_eq!(
            receipt.summary(),
            ReceiptSummary {
                plugin_id: PluginId::new("dev.example.echo").unwrap(),
                version: Version::parse("1.2.3").unwrap(),
                package_digest: digest('a'),
                publisher_key_id: "key-1".into(),
                publisher_lineage: "lineage-1".into(),
                target: PackageTarget::DarwinArm64,
                source: InstallSource::Catalog,
                enabled: true,
                granted_permissions: Vec::new(),
                native_trust_digest: None,
                installed_at_ms: 42,
                generation: 7,
                state_schema_version: 3,
                rollback_compatible_through: 2,
            }
        );
    }

    #[test]
    fn legacy_bundled_source_is_restricted_to_canonical_agent_vm() {
        assert_eq!(
            receipt("dev.example.echo", InstallSource::LegacyBundledV1)
                .validate()
                .unwrap_err()
                .code(),
            "receipt_legacy_plugin_id"
        );
        assert!(receipt("agent-vm", InstallSource::LegacyBundledV1)
            .validate()
            .is_ok());
    }

    #[test]
    fn previous_summary_must_satisfy_schema_and_generation_invariants() {
        let mut expected = receipt("dev.example.echo", InstallSource::Catalog);
        let mut previous = expected.summary();
        previous.generation = 0;
        expected.previous = Some(previous);

        assert_eq!(expected.validate().unwrap_err().code(), "receipt_schema");

        let mut expected = receipt("dev.example.echo", InstallSource::Catalog);
        let mut previous = expected.summary();
        previous.generation = 6;
        previous.state_schema_version = 0;
        expected.previous = Some(previous);

        assert_eq!(expected.validate().unwrap_err().code(), "receipt_schema");

        let mut expected = receipt("dev.example.echo", InstallSource::Catalog);
        let mut previous = expected.summary();
        previous.generation = 6;
        previous.rollback_compatible_through = previous.state_schema_version + 1;
        expected.previous = Some(previous);

        assert_eq!(expected.validate().unwrap_err().code(), "receipt_schema");
    }

    #[test]
    fn previous_summary_must_satisfy_legacy_source_restrictions() {
        let mut expected = receipt("dev.example.echo", InstallSource::Catalog);
        let mut previous = expected.summary();
        previous.generation = 6;
        previous.source = InstallSource::LegacyBundledV1;
        expected.previous = Some(previous);

        assert_eq!(
            expected.validate().unwrap_err().code(),
            "receipt_legacy_plugin_id"
        );

        let mut expected = receipt("agent-vm", InstallSource::Catalog);
        let mut previous = expected.summary();
        previous.generation = 6;
        previous.source = InstallSource::LegacyBundledV1;
        previous.native_trust_digest = Some(digest('b'));
        expected.previous = Some(previous);

        assert_eq!(
            expected.validate().unwrap_err().code(),
            "receipt_legacy_native_trust"
        );
    }

    #[test]
    fn receipt_json_rejects_unknown_fields() {
        let mut value =
            serde_json::to_value(receipt("dev.example.echo", InstallSource::Catalog)).unwrap();
        value["unexpected"] = true.into();

        assert!(serde_json::from_value::<InstallReceipt>(value).is_err());
    }
}
