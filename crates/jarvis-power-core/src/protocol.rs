use std::error::Error;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MIN_TTL_MS: u64 = 5_000;
pub const DEFAULT_TTL_MS: u64 = 45_000;
pub const MAX_TTL_MS: u64 = 120_000;
pub const RENEW_EVERY_MS: u64 = 15_000;
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
pub const SERVICE_LABEL: &str = "app.jarvis.monitor.power-helper";

const MAX_IDENTIFIER_BYTES: usize = 128;
const UUID_TEXT_BYTES: usize = 36;
const LEASE_ID_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if is_canonical_uuid_v7(&value) {
            Ok(Self(value))
        } else {
            Err(ProtocolError::InvalidRequest)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// A validated power-helper request.
///
/// Untrusted wire bytes must enter through [`decode_request`], which enforces
/// [`MAX_FRAME_BYTES`] before JSON parsing.
///
/// ```compile_fail
/// use jarvis_power_core::protocol::RequestEnvelope;
///
/// let _: RequestEnvelope = serde_json::from_slice(br#"{}"#).unwrap();
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub request: Request,
}

impl RequestEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_protocol_version(self.protocol_version)?;
        self.request.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    AcquireLease {
        profile: String,
        owner_generation: String,
        ttl_ms: u64,
    },
    RenewLease {
        lease_id: String,
        owner_generation: String,
        ttl_ms: u64,
    },
    ReleaseLease {
        lease_id: String,
        owner_generation: String,
    },
    Status,
}

impl Request {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::AcquireLease {
                profile,
                owner_generation,
                ttl_ms,
            } => {
                validate_identifier(profile)?;
                validate_identifier(owner_generation)?;
                validate_ttl(*ttl_ms)
            }
            Self::RenewLease {
                lease_id,
                owner_generation,
                ttl_ms,
            } => {
                validate_lease_id(lease_id)?;
                validate_identifier(owner_generation)?;
                validate_ttl(*ttl_ms)
            }
            Self::ReleaseLease {
                lease_id,
                owner_generation,
            } => {
                validate_lease_id(lease_id)?;
                validate_identifier(owner_generation)
            }
            Self::Status => Ok(()),
        }
    }
}

/// A validated power-helper response.
///
/// Untrusted wire bytes must enter through [`decode_response`], which enforces
/// [`MAX_FRAME_BYTES`] before JSON parsing.
///
/// ```compile_fail
/// use jarvis_power_core::protocol::ResponseEnvelope;
///
/// let _: ResponseEnvelope = serde_json::from_slice(br#"{}"#).unwrap();
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub response: Response,
}

impl ResponseEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_protocol_version(self.protocol_version)?;
        self.response.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Acquired {
        lease_id: String,
        granted_ttl_ms: u64,
    },
    Renewed {
        lease_id: String,
        granted_ttl_ms: u64,
    },
    Released {
        lease_id: String,
    },
    Status {
        active_leases: u32,
        mutation_active: bool,
        recovery_required: bool,
    },
    Error {
        code: ErrorCode,
    },
}

impl Response {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Acquired {
                lease_id,
                granted_ttl_ms,
            }
            | Self::Renewed {
                lease_id,
                granted_ttl_ms,
            } => {
                validate_lease_id(lease_id).map_err(|_| ProtocolError::InvalidResponse)?;
                validate_response_ttl(*granted_ttl_ms)
            }
            Self::Released { lease_id } => {
                validate_lease_id(lease_id).map_err(|_| ProtocolError::InvalidResponse)
            }
            Self::Status { .. } | Self::Error { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    InvalidRequest,
    IncompatibleVersion,
    Unauthorized,
    HelperUnavailable,
    StateUnavailable,
    LeaseNotFound,
    LeaseExpired,
    OwnerMismatch,
    Conflict,
    RecoveryRequired,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    FrameTooLarge,
    MalformedFrame,
    IncompatibleVersion,
    InvalidRequest,
    InvalidResponse,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FrameTooLarge => "power-helper frame exceeds the size limit",
            Self::MalformedFrame => "power-helper frame is malformed",
            Self::IncompatibleVersion => "power-helper protocol version is incompatible",
            Self::InvalidRequest => "power-helper request is invalid",
            Self::InvalidResponse => "power-helper response is invalid",
        })
    }
}

impl Error for ProtocolError {}

pub fn decode_request(bytes: impl AsRef<[u8]>) -> Result<RequestEnvelope, ProtocolError> {
    let bytes = bytes.as_ref();
    validate_frame_size(bytes)?;
    let wire: WireRequestEnvelope =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError::MalformedFrame)?;
    let envelope = RequestEnvelope::from(wire);
    envelope.validate()?;
    Ok(envelope)
}

pub fn encode_request(envelope: &RequestEnvelope) -> Result<Vec<u8>, ProtocolError> {
    envelope.validate()?;
    let bytes = serde_json::to_vec(envelope).map_err(|_| ProtocolError::InvalidRequest)?;
    validate_frame_size(&bytes)?;
    Ok(bytes)
}

pub fn decode_response(bytes: impl AsRef<[u8]>) -> Result<ResponseEnvelope, ProtocolError> {
    let bytes = bytes.as_ref();
    validate_frame_size(bytes)?;
    let wire: WireResponseEnvelope =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError::MalformedFrame)?;
    let envelope = ResponseEnvelope::from(wire);
    envelope.validate()?;
    Ok(envelope)
}

pub fn encode_response(envelope: &ResponseEnvelope) -> Result<Vec<u8>, ProtocolError> {
    envelope.validate()?;
    let bytes = serde_json::to_vec(envelope).map_err(|_| ProtocolError::InvalidResponse)?;
    validate_frame_size(&bytes)?;
    Ok(bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum WireRequestEnvelope {
    AcquireLease {
        protocol_version: u32,
        request_id: RequestId,
        profile: String,
        owner_generation: String,
        ttl_ms: u64,
    },
    RenewLease {
        protocol_version: u32,
        request_id: RequestId,
        lease_id: String,
        owner_generation: String,
        ttl_ms: u64,
    },
    ReleaseLease {
        protocol_version: u32,
        request_id: RequestId,
        lease_id: String,
        owner_generation: String,
    },
    Status {
        protocol_version: u32,
        request_id: RequestId,
    },
}

impl From<&RequestEnvelope> for WireRequestEnvelope {
    fn from(envelope: &RequestEnvelope) -> Self {
        let protocol_version = envelope.protocol_version;
        let request_id = envelope.request_id.clone();
        match &envelope.request {
            Request::AcquireLease {
                profile,
                owner_generation,
                ttl_ms,
            } => Self::AcquireLease {
                protocol_version,
                request_id,
                profile: profile.clone(),
                owner_generation: owner_generation.clone(),
                ttl_ms: *ttl_ms,
            },
            Request::RenewLease {
                lease_id,
                owner_generation,
                ttl_ms,
            } => Self::RenewLease {
                protocol_version,
                request_id,
                lease_id: lease_id.clone(),
                owner_generation: owner_generation.clone(),
                ttl_ms: *ttl_ms,
            },
            Request::ReleaseLease {
                lease_id,
                owner_generation,
            } => Self::ReleaseLease {
                protocol_version,
                request_id,
                lease_id: lease_id.clone(),
                owner_generation: owner_generation.clone(),
            },
            Request::Status => Self::Status {
                protocol_version,
                request_id,
            },
        }
    }
}

impl From<WireRequestEnvelope> for RequestEnvelope {
    fn from(wire: WireRequestEnvelope) -> Self {
        match wire {
            WireRequestEnvelope::AcquireLease {
                protocol_version,
                request_id,
                profile,
                owner_generation,
                ttl_ms,
            } => Self {
                protocol_version,
                request_id,
                request: Request::AcquireLease {
                    profile,
                    owner_generation,
                    ttl_ms,
                },
            },
            WireRequestEnvelope::RenewLease {
                protocol_version,
                request_id,
                lease_id,
                owner_generation,
                ttl_ms,
            } => Self {
                protocol_version,
                request_id,
                request: Request::RenewLease {
                    lease_id,
                    owner_generation,
                    ttl_ms,
                },
            },
            WireRequestEnvelope::ReleaseLease {
                protocol_version,
                request_id,
                lease_id,
                owner_generation,
            } => Self {
                protocol_version,
                request_id,
                request: Request::ReleaseLease {
                    lease_id,
                    owner_generation,
                },
            },
            WireRequestEnvelope::Status {
                protocol_version,
                request_id,
            } => Self {
                protocol_version,
                request_id,
                request: Request::Status,
            },
        }
    }
}

impl Serialize for RequestEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        WireRequestEnvelope::from(self).serialize(serializer)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "result",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum WireResponseEnvelope {
    Acquired {
        protocol_version: u32,
        request_id: RequestId,
        lease_id: String,
        granted_ttl_ms: u64,
    },
    Renewed {
        protocol_version: u32,
        request_id: RequestId,
        lease_id: String,
        granted_ttl_ms: u64,
    },
    Released {
        protocol_version: u32,
        request_id: RequestId,
        lease_id: String,
    },
    Status {
        protocol_version: u32,
        request_id: RequestId,
        active_leases: u32,
        mutation_active: bool,
        recovery_required: bool,
    },
    Error {
        protocol_version: u32,
        request_id: RequestId,
        code: ErrorCode,
    },
}

impl From<&ResponseEnvelope> for WireResponseEnvelope {
    fn from(envelope: &ResponseEnvelope) -> Self {
        let protocol_version = envelope.protocol_version;
        let request_id = envelope.request_id.clone();
        match &envelope.response {
            Response::Acquired {
                lease_id,
                granted_ttl_ms,
            } => Self::Acquired {
                protocol_version,
                request_id,
                lease_id: lease_id.clone(),
                granted_ttl_ms: *granted_ttl_ms,
            },
            Response::Renewed {
                lease_id,
                granted_ttl_ms,
            } => Self::Renewed {
                protocol_version,
                request_id,
                lease_id: lease_id.clone(),
                granted_ttl_ms: *granted_ttl_ms,
            },
            Response::Released { lease_id } => Self::Released {
                protocol_version,
                request_id,
                lease_id: lease_id.clone(),
            },
            Response::Status {
                active_leases,
                mutation_active,
                recovery_required,
            } => Self::Status {
                protocol_version,
                request_id,
                active_leases: *active_leases,
                mutation_active: *mutation_active,
                recovery_required: *recovery_required,
            },
            Response::Error { code } => Self::Error {
                protocol_version,
                request_id,
                code: *code,
            },
        }
    }
}

impl From<WireResponseEnvelope> for ResponseEnvelope {
    fn from(wire: WireResponseEnvelope) -> Self {
        match wire {
            WireResponseEnvelope::Acquired {
                protocol_version,
                request_id,
                lease_id,
                granted_ttl_ms,
            } => Self {
                protocol_version,
                request_id,
                response: Response::Acquired {
                    lease_id,
                    granted_ttl_ms,
                },
            },
            WireResponseEnvelope::Renewed {
                protocol_version,
                request_id,
                lease_id,
                granted_ttl_ms,
            } => Self {
                protocol_version,
                request_id,
                response: Response::Renewed {
                    lease_id,
                    granted_ttl_ms,
                },
            },
            WireResponseEnvelope::Released {
                protocol_version,
                request_id,
                lease_id,
            } => Self {
                protocol_version,
                request_id,
                response: Response::Released { lease_id },
            },
            WireResponseEnvelope::Status {
                protocol_version,
                request_id,
                active_leases,
                mutation_active,
                recovery_required,
            } => Self {
                protocol_version,
                request_id,
                response: Response::Status {
                    active_leases,
                    mutation_active,
                    recovery_required,
                },
            },
            WireResponseEnvelope::Error {
                protocol_version,
                request_id,
                code,
            } => Self {
                protocol_version,
                request_id,
                response: Response::Error { code },
            },
        }
    }
}

impl Serialize for ResponseEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        WireResponseEnvelope::from(self).serialize(serializer)
    }
}

fn validate_frame_size(bytes: &[u8]) -> Result<(), ProtocolError> {
    if bytes.len() > MAX_FRAME_BYTES {
        Err(ProtocolError::FrameTooLarge)
    } else {
        Ok(())
    }
}

fn validate_protocol_version(version: u32) -> Result<(), ProtocolError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::IncompatibleVersion)
    }
}

fn validate_ttl(ttl_ms: u64) -> Result<(), ProtocolError> {
    if (MIN_TTL_MS..=MAX_TTL_MS).contains(&ttl_ms) {
        Ok(())
    } else {
        Err(ProtocolError::InvalidRequest)
    }
}

fn validate_response_ttl(ttl_ms: u64) -> Result<(), ProtocolError> {
    validate_ttl(ttl_ms).map_err(|_| ProtocolError::InvalidResponse)
}

fn validate_identifier(value: &str) -> Result<(), ProtocolError> {
    let bytes = value.as_bytes();
    let valid = !bytes.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.iter().copied().all(
            |byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'),
        );
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidRequest)
    }
}

fn validate_lease_id(value: &str) -> Result<(), ProtocolError> {
    let valid = value.len() == LEASE_ID_BYTES
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidRequest)
    }
}

fn is_canonical_uuid_v7(value: &str) -> bool {
    if value.len() != UUID_TEXT_BYTES {
        return false;
    }

    let bytes = value.as_bytes();
    if bytes[8] != b'-' || bytes[13] != b'-' || bytes[18] != b'-' || bytes[23] != b'-' {
        return false;
    }
    if bytes[14] != b'7' || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return false;
    }

    bytes.iter().enumerate().all(|(index, byte)| {
        matches!(index, 8 | 13 | 18 | 23) || matches!(byte, b'0'..=b'9' | b'a'..=b'f')
    })
}
