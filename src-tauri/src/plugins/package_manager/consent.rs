use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::receipt::GrantedPermission;
use serde::{Deserialize, Serialize};

use super::manager::{InstallPlan, ManagerError};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Approval {
    pub operation_id: String,
    pub package_digest: Digest,
    pub granted_permissions: Vec<GrantedPermission>,
    pub native_trust_digest: Option<Digest>,
    pub approve_irreversible_migration: bool,
}

impl Approval {
    pub fn all(operation_id: impl Into<String>, package_digest: Digest) -> Self {
        Self {
            operation_id: operation_id.into(),
            native_trust_digest: Some(package_digest.clone()),
            package_digest,
            granted_permissions: Vec::new(),
            approve_irreversible_migration: true,
        }
    }

    pub fn native(operation_id: impl Into<String>, package_digest: Digest) -> Self {
        Self {
            operation_id: operation_id.into(),
            native_trust_digest: Some(package_digest.clone()),
            package_digest,
            granted_permissions: Vec::new(),
            approve_irreversible_migration: false,
        }
    }

    pub fn with_permissions(mut self, permissions: Vec<GrantedPermission>) -> Self {
        self.granted_permissions = permissions;
        self
    }

    pub fn approve_irreversible(mut self) -> Self {
        self.approve_irreversible_migration = true;
        self
    }
}

pub fn validate_approval(plan: &InstallPlan, approval: &Approval) -> Result<(), ManagerError> {
    if approval.operation_id != plan.operation_id {
        return Err(ManagerError::new(
            "consent_operation_mismatch",
            "approval belongs to another package operation",
        ));
    }
    match (&plan.native_trust_digest, &approval.native_trust_digest) {
        (Some(expected), Some(observed)) if expected == observed => {}
        (Some(_), _) => {
            return Err(ManagerError::new(
                "native_digest_consent_mismatch",
                "native trust approval does not match the prepared package digest",
            ));
        }
        (None, Some(_)) => {
            return Err(ManagerError::new(
                "unexpected_native_consent",
                "a UI-only package cannot request a native trust grant",
            ));
        }
        (None, None) => {}
    }
    if approval.package_digest != plan.package_digest {
        return Err(ManagerError::new(
            "package_digest_consent_mismatch",
            "approval package digest differs from the prepared package",
        ));
    }
    if approval.granted_permissions != plan.requested_permissions {
        return Err(ManagerError::new(
            "permission_consent_mismatch",
            "approved permissions differ from the prepared permission set",
        ));
    }
    if plan.irreversible_migration && !approval.approve_irreversible_migration {
        return Err(ManagerError::new(
            "irreversible_migration_consent_required",
            "irreversible state migration requires a separate approval",
        ));
    }
    Ok(())
}
