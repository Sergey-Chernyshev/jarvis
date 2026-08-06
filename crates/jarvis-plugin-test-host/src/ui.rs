use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use jarvis_plugin_protocol::bridge::{
    BridgeClientFrame, BridgeHostFrame, BridgeRequest, BridgeResponse, SubscribeResult, Welcome,
    BRIDGE_PROTOCOL_V1, MAX_BRIDGE_IN_FLIGHT, MAX_BRIDGE_MESSAGE_BYTES, MAX_BRIDGE_SUBSCRIPTIONS,
};
use serde_json::{json, Value};

const DEFAULT_PAGE_ID: &str = "fixture";
const MAX_BRIDGE_IDENTIFIER_BYTES: usize = 128;
const CALLER_IDENTITY_FIELDS: [&str; 8] = [
    "pluginId",
    "packageDigest",
    "pageId",
    "grants",
    "owner",
    "ownerPluginId",
    "principal",
    "principalId",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundPage {
    plugin_id: String,
    package_digest: String,
    page_id: String,
    generation: u64,
    grants: Vec<String>,
}

impl BoundPage {
    pub fn fixture(
        plugin_id: impl Into<String>,
        package_digest: impl Into<String>,
        generation: u64,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            package_digest: package_digest.into(),
            page_id: DEFAULT_PAGE_ID.to_owned(),
            generation,
            grants: Vec::new(),
        }
    }

    pub fn with_grants(mut self, grants: impl IntoIterator<Item = String>) -> Self {
        self.grants = grants.into_iter().collect();
        self
    }
}

#[derive(Default)]
struct UiState {
    recorded_requests: Vec<BridgeRequest>,
    subscriptions: BTreeSet<String>,
}

pub struct UiTestHost {
    bound_page: BoundPage,
    in_flight: AtomicUsize,
    state: Mutex<UiState>,
}

impl UiTestHost {
    pub fn new(bound_page: BoundPage) -> Self {
        Self {
            bound_page,
            in_flight: AtomicUsize::new(0),
            state: Mutex::new(UiState::default()),
        }
    }

    pub fn bound_plugin_id(&self) -> &str {
        &self.bound_page.plugin_id
    }

    pub fn welcome_fixture(&self) -> BridgeHostFrame {
        BridgeHostFrame::Welcome(Welcome {
            v: BRIDGE_PROTOCOL_V1,
            plugin_id: self.bound_page.plugin_id.clone(),
            package_digest: self.bound_page.package_digest.clone(),
            page_id: self.bound_page.page_id.clone(),
            generation: self.bound_page.generation,
            grants: self.bound_page.grants.clone(),
        })
    }

    pub fn recorded_requests(&self) -> Vec<BridgeRequest> {
        self.state
            .lock()
            .map(|state| state.recorded_requests.clone())
            .unwrap_or_default()
    }

    pub fn request_fixture(&self, payload: &str) -> Result<BridgeHostFrame, UiContractError> {
        if payload.len() > MAX_BRIDGE_MESSAGE_BYTES {
            return Err(UiContractError::MessageTooLarge);
        }

        let value: Value =
            serde_json::from_str(payload).map_err(|_| UiContractError::SchemaInvalid)?;
        reject_caller_identity(&value)?;
        let frame: BridgeClientFrame = serde_json::from_value(value).map_err(map_frame_error)?;
        let BridgeClientFrame::Request(request) = frame else {
            return Err(UiContractError::SchemaInvalid);
        };
        if request.generation != self.bound_page.generation {
            return Err(UiContractError::PageGenerationStale);
        }

        let _in_flight = InFlightGuard::acquire(&self.in_flight)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| UiContractError::ProviderUnavailable)?;

        let response = if request.method.ends_with(".watch") {
            let subscription_id = subscription_id_for(&request.id);
            if !state.subscriptions.contains(&subscription_id)
                && state.subscriptions.len() >= MAX_BRIDGE_SUBSCRIPTIONS
            {
                return Err(UiContractError::SubscriptionLimit);
            }
            state.subscriptions.insert(subscription_id.clone());
            BridgeHostFrame::SubscribeResult(SubscribeResult {
                v: BRIDGE_PROTOCOL_V1,
                id: request.id.clone(),
                generation: request.generation,
                subscription_id,
                cursor: 0,
            })
        } else {
            BridgeHostFrame::Response(BridgeResponse {
                v: BRIDGE_PROTOCOL_V1,
                id: request.id.clone(),
                generation: request.generation,
                result: deterministic_result(&request),
            })
        };

        state.recorded_requests.push(request);
        Ok(response)
    }
}

fn subscription_id_for(request_id: &str) -> String {
    // Request IDs are opaque and may already consume the full public 128-byte
    // budget. A stable hash keeps the fake host output bounded without
    // truncating two distinct request IDs to the same subscription ID.
    let mut first = 0xcbf29ce484222325_u64;
    let mut second = 0x84222325cbf29ce4_u64;
    for byte in request_id.as_bytes() {
        first ^= u64::from(*byte);
        first = first.wrapping_mul(0x100000001b3);
        second ^= u64::from(*byte);
        second = second.wrapping_mul(0x100000001b3).rotate_left(13);
    }
    let id = format!("subscription/{first:016x}{second:016x}");
    debug_assert!(id.len() <= MAX_BRIDGE_IDENTIFIER_BYTES);
    id
}

fn deterministic_result(request: &BridgeRequest) -> Value {
    json!({
        "fixture": true,
        "method": request.method,
        "namespace": request.namespace,
    })
}

fn reject_caller_identity(value: &Value) -> Result<(), UiContractError> {
    let object = value.as_object().ok_or(UiContractError::SchemaInvalid)?;
    if CALLER_IDENTITY_FIELDS
        .iter()
        .any(|field| object.contains_key(*field))
    {
        return Err(UiContractError::UnknownField);
    }
    Ok(())
}

fn map_frame_error(error: serde_json::Error) -> UiContractError {
    let message = error.to_string();
    if message.contains("unknown field") {
        UiContractError::UnknownField
    } else if message.contains("bridge protocol incompatible") {
        UiContractError::ProtocolIncompatible
    } else if message.contains("invalid bridge deadline") {
        UiContractError::Deadline
    } else {
        UiContractError::SchemaInvalid
    }
}

struct InFlightGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> InFlightGuard<'a> {
    fn acquire(counter: &'a AtomicUsize) -> Result<Self, UiContractError> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= MAX_BRIDGE_IN_FLIGHT {
                return Err(UiContractError::InFlightLimit);
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self { counter }),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiContractError {
    MessageTooLarge,
    UnknownField,
    SchemaInvalid,
    ProtocolIncompatible,
    Deadline,
    PageGenerationStale,
    InFlightLimit,
    SubscriptionLimit,
    ProviderUnavailable,
}

impl UiContractError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MessageTooLarge => "bridge_message_too_large",
            Self::UnknownField => "bridge_unknown_field",
            Self::SchemaInvalid => "schema_invalid",
            Self::ProtocolIncompatible => "bridge_protocol_incompatible",
            Self::Deadline => "bridge_deadline",
            Self::PageGenerationStale => "page_generation_stale",
            Self::InFlightLimit => "bridge_in_flight_limit",
            Self::SubscriptionLimit => "bridge_subscription_limit",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

impl fmt::Display for UiContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Error for UiContractError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_flight_limit_rejects_one_over_and_drop_releases_capacity() {
        let counter = AtomicUsize::new(0);
        let mut guards = Vec::new();

        for _ in 0..MAX_BRIDGE_IN_FLIGHT {
            guards.push(InFlightGuard::acquire(&counter).expect("permit within limit"));
        }
        assert_eq!(counter.load(Ordering::Acquire), MAX_BRIDGE_IN_FLIGHT);
        assert_eq!(
            InFlightGuard::acquire(&counter)
                .err()
                .expect("one-over must fail")
                .code(),
            "bridge_in_flight_limit"
        );

        guards.pop();
        let replacement = InFlightGuard::acquire(&counter).expect("drop releases one permit");
        drop(replacement);
        drop(guards);
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }
}
