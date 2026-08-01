use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicErrorCode {
    BridgeProtocolIncompatible,
    BridgeMessageTooLarge,
    BridgeRateLimited,
    BridgeInFlightLimit,
    BridgeSubscriptionLimit,
    BridgeDeadline,
    BridgeCancelled,
    PageBindingMissing,
    PageGenerationStale,
    PackageDigestStale,
    GrantRevoked,
    GrantScopeDenied,
    ContractNotFound,
    ContractIncompatible,
    SchemaInvalid,
    RevisionConflict,
    CursorGap,
    ResourceHandleInvalid,
    ResourceHandleExpired,
    ResourceHandleExhausted,
    OperationPending,
    ProviderUnavailable,
    PluginUiIsolationUnavailable,
}

impl PublicErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BridgeProtocolIncompatible => "bridge_protocol_incompatible",
            Self::BridgeMessageTooLarge => "bridge_message_too_large",
            Self::BridgeRateLimited => "bridge_rate_limited",
            Self::BridgeInFlightLimit => "bridge_in_flight_limit",
            Self::BridgeSubscriptionLimit => "bridge_subscription_limit",
            Self::BridgeDeadline => "bridge_deadline",
            Self::BridgeCancelled => "bridge_cancelled",
            Self::PageBindingMissing => "page_binding_missing",
            Self::PageGenerationStale => "page_generation_stale",
            Self::PackageDigestStale => "package_digest_stale",
            Self::GrantRevoked => "grant_revoked",
            Self::GrantScopeDenied => "grant_scope_denied",
            Self::ContractNotFound => "contract_not_found",
            Self::ContractIncompatible => "contract_incompatible",
            Self::SchemaInvalid => "schema_invalid",
            Self::RevisionConflict => "revision_conflict",
            Self::CursorGap => "cursor_gap",
            Self::ResourceHandleInvalid => "resource_handle_invalid",
            Self::ResourceHandleExpired => "resource_handle_expired",
            Self::ResourceHandleExhausted => "resource_handle_exhausted",
            Self::OperationPending => "operation_pending",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::PluginUiIsolationUnavailable => "plugin_ui_isolation_unavailable",
        }
    }
}

impl fmt::Display for PublicErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
