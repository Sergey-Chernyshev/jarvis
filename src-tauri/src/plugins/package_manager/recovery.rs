use super::manager::{ManagerError, ManagerResult};
use super::receipt::{ReceiptVisibility, VersionVisibility};
use super::DurableObservation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedInstallPhase {
    Prepared,
    Extracted,
    Migrated,
    HealthPassed,
    VersionCommitted,
    ReceiptWritten,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallRecoveryDecision {
    ResumeFreshVerification,
    ResumeReceiptCommit,
    Succeeded,
    Failed { code: &'static str },
}

pub fn decide_install_recovery(
    phase: SavedInstallPhase,
    fresh_trust: ManagerResult<()>,
    version: VersionVisibility,
    receipt: ReceiptVisibility,
) -> ManagerResult<InstallRecoveryDecision> {
    fresh_trust?;

    match (&version, &receipt) {
        (
            VersionVisibility::Exact {
                plugin_id: version_plugin,
                package_digest: version_digest,
                ..
            },
            ReceiptVisibility::Exact {
                plugin_id: receipt_plugin,
                package_digest: receipt_digest,
                ..
            },
        ) if version_plugin == receipt_plugin && version_digest == receipt_digest => {
            return Ok(InstallRecoveryDecision::Succeeded);
        }
        (VersionVisibility::Conflict { .. }, _) => {
            return Ok(InstallRecoveryDecision::Failed {
                code: "version_digest_conflict",
            });
        }
        (_, ReceiptVisibility::Different { .. }) => {
            return Ok(InstallRecoveryDecision::Failed {
                code: "install_interrupted",
            });
        }
        _ => {}
    }

    match (phase, version, receipt) {
        (
            SavedInstallPhase::HealthPassed
            | SavedInstallPhase::VersionCommitted
            | SavedInstallPhase::ReceiptWritten,
            VersionVisibility::Exact { .. },
            ReceiptVisibility::Absent,
        ) => Ok(InstallRecoveryDecision::ResumeReceiptCommit),
        (SavedInstallPhase::Prepared, VersionVisibility::Absent, ReceiptVisibility::Absent) => {
            Ok(InstallRecoveryDecision::ResumeFreshVerification)
        }
        _ => Ok(InstallRecoveryDecision::Failed {
            code: "install_interrupted",
        }),
    }
}

pub fn collapse_durability<T>(observation: DurableObservation<T>) -> T {
    match observation {
        DurableObservation::Confirmed(value) | DurableObservation::DurabilityUnknown(value) => {
            value
        }
    }
}

pub fn require_recoverable_decision(
    decision: InstallRecoveryDecision,
) -> ManagerResult<InstallRecoveryDecision> {
    match decision {
        InstallRecoveryDecision::Failed { code } => Err(ManagerError::new(
            code,
            "durable package state cannot safely resume",
        )),
        decision => Ok(decision),
    }
}
