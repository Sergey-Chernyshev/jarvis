use std::collections::BTreeSet;

pub const STATE_SCHEMA_VERSION: u32 = 2;

const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_ATTESTATION_TEXT_BYTES: usize = 255;
const LEASE_ID_BYTES: usize = 32;
const TEAM_ID_BYTES: usize = 10;
const PROCESS_IDENTITY_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateError {
    UnsupportedSchemaVersion,
    InvalidServiceVersion,
    InvalidMinimumClientBuild,
    InvalidBootId,
    InvalidMutationGeneration,
    InvalidMutationInvariant,
    InvalidPhase,
    InvalidLease,
    DuplicateLease,
    InvalidIdentifier,
    InvalidPrincipal,
    InvalidProcessIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DarwinProcessIdentity {
    version: u16,
    start_seconds: u64,
    start_microseconds: u32,
}

impl DarwinProcessIdentity {
    pub fn new(
        version: u16,
        start_seconds: u64,
        start_microseconds: u32,
    ) -> Result<Self, StateError> {
        if version != PROCESS_IDENTITY_VERSION
            || start_seconds == 0
            || start_microseconds >= 1_000_000
        {
            return Err(StateError::InvalidProcessIdentity);
        }
        Ok(Self {
            version,
            start_seconds,
            start_microseconds,
        })
    }

    pub fn version(self) -> u16 {
        self.version
    }

    pub fn start_seconds(self) -> u64 {
        self.start_seconds
    }

    pub fn start_microseconds(self) -> u32 {
        self.start_microseconds
    }
}

/// Identity evidence produced by the helper's platform attestation boundary.
///
/// This type intentionally has no serde implementation. Wire protocol DTOs
/// cannot be converted into a principal.
///
/// ```compile_fail
/// use jarvis_power_core::state::Principal;
///
/// let _: Principal = serde_json::from_str("{}").unwrap();
/// ```
///
/// ```compile_fail
/// use jarvis_power_core::state::Principal;
///
/// fn encode(principal: &Principal) {
///     let _ = serde_json::to_string(principal).unwrap();
/// }
/// ```
///
/// ```compile_fail
/// use jarvis_power_core::protocol::{Request, RequestEnvelope, RequestId, PROTOCOL_VERSION};
/// use jarvis_power_core::state::Principal;
///
/// let request = RequestEnvelope {
///     protocol_version: PROTOCOL_VERSION,
///     request_id: RequestId::parse("018f0000-0000-7000-8000-000000000001").unwrap(),
///     request: Request::Status,
/// };
/// let _: Principal = request.into();
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Principal {
    uid: u32,
    pid: i32,
    process_identity: DarwinProcessIdentity,
    bundle_id: String,
    team_id: String,
    requirement_digest: [u8; 32],
    signed_build: u64,
}

impl Principal {
    /// Builds a principal from evidence already verified by the privileged
    /// helper. This constructor is not a wire boundary: callers must never
    /// populate it from client-provided protocol fields.
    #[allow(clippy::too_many_arguments)]
    pub fn from_helper_attestation(
        uid: u32,
        pid: i32,
        process_identity: DarwinProcessIdentity,
        bundle_id: impl Into<String>,
        team_id: impl Into<String>,
        requirement_digest: [u8; 32],
        signed_build: u64,
    ) -> Result<Self, StateError> {
        let principal = Self {
            uid,
            pid,
            process_identity,
            bundle_id: bundle_id.into(),
            team_id: team_id.into(),
            requirement_digest,
            signed_build,
        };
        principal.validate()?;
        Ok(principal)
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn process_identity(&self) -> DarwinProcessIdentity {
        self.process_identity
    }

    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    pub fn team_id(&self) -> &str {
        &self.team_id
    }

    pub fn requirement_digest(&self) -> &[u8; 32] {
        &self.requirement_digest
    }

    pub fn signed_build(&self) -> u64 {
        self.signed_build
    }

    pub fn validate(&self) -> Result<(), StateError> {
        let valid_team_id = self.team_id.len() == TEAM_ID_BYTES
            && self
                .team_id
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
        if self.uid == 0
            || self.pid <= 0
            || self.signed_build == 0
            || !valid_attestation_text(&self.bundle_id)
            || !valid_team_id
            || self.requirement_digest.iter().all(|byte| *byte == 0)
        {
            return Err(StateError::InvalidPrincipal);
        }
        DarwinProcessIdentity::new(
            self.process_identity.version,
            self.process_identity.start_seconds,
            self.process_identity.start_microseconds,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseId(String);

impl LeaseId {
    pub fn parse(value: impl Into<String>) -> Result<Self, StateError> {
        let value = value.into();
        let valid = value.len() == LEASE_ID_BYTES
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            Ok(Self(value))
        } else {
            Err(StateError::InvalidLease)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Milliseconds in the helper's monotonic clock domain for the current boot.
///
/// It is deliberately neither a Unix timestamp nor serializable wire data.
/// A persisted value is meaningful only while its enclosing state's `boot_id`
/// still matches the helper's current boot.
pub struct MonotonicTime(u64);

impl MonotonicTime {
    pub const fn from_millis(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }

    pub(crate) fn checked_add_millis(self, value: u64) -> Option<Self> {
        self.0.checked_add(value).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationPhase {
    Prepared,
    Applied,
    RestorePending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub lease_id: LeaseId,
    pub profile: String,
    pub owner_generation: String,
    pub principal: Principal,
    pub deadline: MonotonicTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperState {
    pub schema_version: u32,
    pub service_version: u64,
    pub minimum_client_build: u64,
    pub boot_id: String,
    pub baseline: bool,
    pub applied: bool,
    pub did_mutate: bool,
    pub mutation_generation: u64,
    pub phase: MutationPhase,
    pub leases: Vec<Lease>,
}

impl HelperState {
    pub fn validate(&self) -> Result<(), StateError> {
        if self.schema_version != STATE_SCHEMA_VERSION {
            return Err(StateError::UnsupportedSchemaVersion);
        }
        if self.service_version == 0 {
            return Err(StateError::InvalidServiceVersion);
        }
        if self.minimum_client_build == 0 {
            return Err(StateError::InvalidMinimumClientBuild);
        }
        if !valid_identifier(&self.boot_id) {
            return Err(StateError::InvalidBootId);
        }
        if self.mutation_generation == 0 || self.mutation_generation == u64::MAX {
            return Err(StateError::InvalidMutationGeneration);
        }
        if self.baseline == self.did_mutate {
            return Err(StateError::InvalidMutationInvariant);
        }

        let phase_is_valid = match self.phase {
            MutationPhase::Prepared => !self.applied && !self.leases.is_empty(),
            MutationPhase::Applied => self.applied && !self.leases.is_empty(),
            MutationPhase::RestorePending => self.leases.is_empty(),
        };
        if !phase_is_valid {
            return Err(StateError::InvalidPhase);
        }

        let mut lease_ids = BTreeSet::new();
        for lease in &self.leases {
            if !lease_ids.insert(lease.lease_id.as_str())
                || !valid_identifier(&lease.profile)
                || !valid_identifier(&lease.owner_generation)
                || lease.deadline.as_millis() == 0
                || lease.principal.signed_build() < self.minimum_client_build
                || lease.principal.validate().is_err()
            {
                return Err(StateError::InvalidLease);
            }
        }
        Ok(())
    }
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_IDENTIFIER_BYTES
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.iter().copied().all(
            |byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'),
        )
}

fn valid_attestation_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ATTESTATION_TEXT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
}
