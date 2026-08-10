use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use jarvis_plugin_protocol::broker::ContractRef;
use jarvis_plugin_protocol::manifest::{Digest, PluginId};

use super::{BrokerError, BrokerResult};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactContractGrant {
    consumer_activation_generation: u64,
    contract: ContractRef,
    provider_plugin_id: String,
    provider_signer_lineage: String,
    provider_package_digest: String,
    provider_activation_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedBrokerAccess {
    plugin_id: PluginId,
    signer_lineage: String,
    package_digest: Digest,
    activation_generation: u64,
    live: Arc<AtomicBool>,
    operation_gate: Arc<RwLock<()>>,
    grants: Arc<RwLock<Vec<ExactContractGrant>>>,
}

pub(super) struct BrokerAccessAdmission<'a> {
    _operation: RwLockReadGuard<'a, ()>,
}

impl VerifiedBrokerAccess {
    pub(crate) fn from_activation(
        plugin_id: &str,
        signer_lineage: &str,
        package_digest: &str,
        activation_generation: u64,
    ) -> BrokerResult<Self> {
        let plugin_id = PluginId::new(plugin_id)
            .map_err(|_| BrokerError::new("invalid_principal", "invalid plugin identity"))?;
        let package_digest = Digest::new(package_digest)
            .map_err(|_| BrokerError::new("invalid_principal", "invalid package digest"))?;
        if signer_lineage.is_empty() || signer_lineage.len() > 256 || activation_generation == 0 {
            return Err(BrokerError::new(
                "invalid_principal",
                "invalid activation binding",
            ));
        }
        Ok(Self {
            plugin_id,
            signer_lineage: signer_lineage.into(),
            package_digest,
            activation_generation,
            live: Arc::new(AtomicBool::new(true)),
            operation_gate: Arc::new(RwLock::new(())),
            grants: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub(super) fn admit(&self) -> BrokerResult<BrokerAccessAdmission<'_>> {
        let operation = self
            .operation_gate
            .read()
            .map_err(|_| BrokerError::new("broker_lock", "broker access gate poisoned"))?;
        self.ensure_live()?;
        Ok(BrokerAccessAdmission {
            _operation: operation,
        })
    }

    pub(super) fn ensure_live(&self) -> BrokerResult<()> {
        if !self.live.load(Ordering::Acquire) {
            return Err(BrokerError::new(
                "principal_revoked",
                "plugin activation has been revoked",
            ));
        }
        Ok(())
    }

    pub(super) fn grant_contract_from(
        &self,
        provider: &VerifiedBrokerAccess,
        contract: &ContractRef,
    ) -> BrokerResult<()> {
        let _consumer_admission = self.admit()?;
        let _provider_admission = if Arc::ptr_eq(&self.operation_gate, &provider.operation_gate) {
            provider.ensure_live()?;
            None
        } else {
            Some(provider.admit()?)
        };
        let grant = ExactContractGrant {
            consumer_activation_generation: self.activation_generation,
            contract: contract.clone(),
            provider_plugin_id: provider.plugin_id().into(),
            provider_signer_lineage: provider.signer_lineage().into(),
            provider_package_digest: provider.package_digest().into(),
            provider_activation_generation: provider.activation_generation(),
        };
        let mut grants = self
            .grants
            .write()
            .map_err(|_| BrokerError::new("broker_lock", "broker grant lock poisoned"))?;
        if !grants.contains(&grant) {
            grants.push(grant);
        }
        Ok(())
    }

    pub(super) fn permits_contract(
        &self,
        contract: &ContractRef,
        provider_plugin_id: &str,
        provider_signer_lineage: &str,
        provider_package_digest: &str,
        provider_activation_generation: u64,
    ) -> BrokerResult<bool> {
        self.ensure_live()?;
        let grants = self
            .grants
            .read()
            .map_err(|_| BrokerError::new("broker_lock", "broker grant lock poisoned"))?;
        Ok(grants.iter().any(|grant| {
            grant.consumer_activation_generation == self.activation_generation
                && grant.contract == *contract
                && grant.provider_plugin_id == provider_plugin_id
                && grant.provider_signer_lineage == provider_signer_lineage
                && grant.provider_package_digest == provider_package_digest
                && grant.provider_activation_generation == provider_activation_generation
        }))
    }

    pub(super) fn revoke(&self) {
        let _operation = self
            .operation_gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.live.store(false, Ordering::Release);
    }

    pub(super) fn plugin_id(&self) -> &str {
        self.plugin_id.as_str()
    }

    pub(super) fn signer_lineage(&self) -> &str {
        &self.signer_lineage
    }

    pub(super) fn package_digest(&self) -> &str {
        self.package_digest.as_str()
    }

    pub(super) fn activation_generation(&self) -> u64 {
        self.activation_generation
    }
}
