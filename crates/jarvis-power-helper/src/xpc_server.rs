#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::ffi::{c_char, c_void, CStr, CString};
use std::fmt;
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::fs::{self, File, OpenOptions};
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::io::{self, Read, Write};
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::path::Path;
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use std::sync::Mutex;

use jarvis_power_core::protocol::{
    decode_request, encode_response, ProtocolError, RequestEnvelope, ResponseEnvelope,
    MAX_FRAME_BYTES,
};
use jarvis_power_core::state::{DarwinProcessIdentity, Principal};
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use jarvis_power_core::{
    engine::EngineError,
    protocol::{ErrorCode, Request, Response, PROTOCOL_VERSION},
    state::LeaseId,
};
use sha2::{Digest, Sha256};

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
use crate::{
    coordinator::CoordinatorError,
    root_store::PRODUCTION_STATE_DIRECTORY,
    watchdog::{ProductionRuntime, ProductionStartup, SchedulerArmError},
};

pub const PRODUCTION_APP_IDENTIFIER: &str = "app.jarvis.monitor";
pub const PRODUCTION_SERVICE_LABEL: &str = "app.jarvis.monitor.power-helper";
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
const BUILD_FLOOR_FILE: &str = "client-build-floor";
#[cfg(all(target_os = "macos", feature = "production-xpc"))]
const BUILD_FLOOR_TEMP_FILE: &str = ".client-build-floor.replace";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationClaims {
    pub team_id: Option<String>,
    pub identifier: Option<String>,
    pub signed_build: Option<u64>,
    pub euid: Option<u32>,
    pub pid: Option<i32>,
    pub start_seconds: Option<u64>,
    pub start_microseconds: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidPolicy,
    MissingClaim,
    ChangedClaims,
    WrongTeam,
    WrongIdentifier,
    Downgrade,
    InvalidProcess,
    FloorUnavailable,
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPolicy => "production attestation policy is invalid",
            Self::MissingClaim => "production attestation claim is missing",
            Self::ChangedClaims => "production attestation claims changed during validation",
            Self::WrongTeam => "production client Team ID is unauthorized",
            Self::WrongIdentifier => "production client signing identifier is unauthorized",
            Self::Downgrade => "production client build is below the persisted floor",
            Self::InvalidProcess => "production client process identity is invalid",
            Self::FloorUnavailable => "production client build floor is unavailable",
        })
    }
}

impl std::error::Error for AuthError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildFloorError {
    Unavailable,
}

impl fmt::Display for BuildFloorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("production client build floor is unavailable")
    }
}

impl std::error::Error for BuildFloorError {}

pub trait BuildFloorStore: Send + Sync {
    fn load(&self) -> Result<u64, BuildFloorError>;

    fn raise_to(&self, build: u64) -> Result<(), BuildFloorError>;

    fn accept_and_raise(
        &self,
        minimum_build: u64,
        candidate_build: u64,
    ) -> Result<bool, BuildFloorError> {
        let effective_floor = self.load()?.max(minimum_build);
        if candidate_build < effective_floor {
            return Ok(false);
        }
        self.raise_to(candidate_build)?;
        Ok(true)
    }
}

pub struct AttestationPolicy<S> {
    expected_team_id: String,
    minimum_build: u64,
    requirement_digest: [u8; 32],
    floor: S,
}

pub fn production_requirement(team_id: &str) -> Result<String, AuthError> {
    if !valid_team_id(team_id) {
        return Err(AuthError::InvalidPolicy);
    }
    Ok(format!(
        "anchor apple generic and identifier \"{PRODUCTION_APP_IDENTIFIER}\" \
and certificate leaf[subject.OU] = \"{team_id}\" \
and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
    ))
}

pub fn production_policy<S>(
    team_id: &str,
    minimum_build: u64,
    floor: S,
) -> Result<AttestationPolicy<S>, AuthError>
where
    S: BuildFloorStore,
{
    if minimum_build == 0 {
        return Err(AuthError::InvalidPolicy);
    }
    let requirement = production_requirement(team_id)?;
    let requirement_digest: [u8; 32] = Sha256::digest(requirement.as_bytes()).into();
    Ok(AttestationPolicy {
        expected_team_id: team_id.to_owned(),
        minimum_build,
        requirement_digest,
        floor,
    })
}

impl<S> AttestationPolicy<S>
where
    S: BuildFloorStore,
{
    pub fn authorize_pair(
        &self,
        first: &AttestationClaims,
        second: &AttestationClaims,
    ) -> Result<Principal, AuthError> {
        if first != second {
            return Err(AuthError::ChangedClaims);
        }
        self.authorize(first)
    }

    fn authorize(&self, claims: &AttestationClaims) -> Result<Principal, AuthError> {
        let team_id = claims.team_id.as_deref().ok_or(AuthError::MissingClaim)?;
        let identifier = claims
            .identifier
            .as_deref()
            .ok_or(AuthError::MissingClaim)?;
        let signed_build = claims.signed_build.ok_or(AuthError::MissingClaim)?;
        let euid = claims.euid.ok_or(AuthError::MissingClaim)?;
        let pid = claims.pid.ok_or(AuthError::MissingClaim)?;
        let start_seconds = claims.start_seconds.ok_or(AuthError::MissingClaim)?;
        let start_microseconds = claims.start_microseconds.ok_or(AuthError::MissingClaim)?;

        if team_id != self.expected_team_id {
            return Err(AuthError::WrongTeam);
        }
        if identifier != PRODUCTION_APP_IDENTIFIER {
            return Err(AuthError::WrongIdentifier);
        }
        if euid == 0
            || pid <= 0
            || start_seconds == 0
            || start_microseconds >= 1_000_000
            || signed_build == 0
        {
            return Err(AuthError::InvalidProcess);
        }

        if !self
            .floor
            .accept_and_raise(self.minimum_build, signed_build)
            .map_err(|_| AuthError::FloorUnavailable)?
        {
            return Err(AuthError::Downgrade);
        }

        let process_identity = DarwinProcessIdentity::new(1, start_seconds, start_microseconds)
            .map_err(|_| AuthError::InvalidProcess)?;
        Principal::from_helper_attestation(
            euid,
            pid,
            process_identity,
            identifier,
            team_id,
            self.requirement_digest,
            signed_build,
        )
        .map_err(|_| AuthError::InvalidProcess)
    }
}

fn valid_team_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

pub trait MessageAttestor: Send + Sync {
    fn attest(&self) -> Result<(AttestationClaims, AttestationClaims), AuthError>;
}

pub trait XpcRequestDispatcher: Send + Sync {
    fn dispatch(&self, principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerError {
    Auth(AuthError),
    InvalidPayload,
    Protocol(ProtocolError),
    ResponseRequestIdMismatch,
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(error) => write!(formatter, "XPC attestation failed: {error}"),
            Self::InvalidPayload => formatter.write_str("XPC payload is empty or oversized"),
            Self::Protocol(error) => write!(formatter, "XPC protocol failed: {error}"),
            Self::ResponseRequestIdMismatch => {
                formatter.write_str("XPC response request id does not match")
            }
        }
    }
}

impl std::error::Error for ServerError {}

pub struct MessageProcessor<S, A, D> {
    policy: AttestationPolicy<S>,
    attestor: A,
    dispatcher: D,
}

impl<S, A, D> MessageProcessor<S, A, D>
where
    S: BuildFloorStore,
    A: MessageAttestor,
    D: XpcRequestDispatcher,
{
    pub fn new(policy: AttestationPolicy<S>, attestor: A, dispatcher: D) -> Self {
        Self {
            policy,
            attestor,
            dispatcher,
        }
    }

    pub fn process(&self, payload: &[u8]) -> Result<Vec<u8>, ServerError> {
        let (first, second) = self.attestor.attest().map_err(ServerError::Auth)?;
        process_attested(&self.policy, &self.dispatcher, &first, &second, payload)
    }
}

fn process_attested<S, D>(
    policy: &AttestationPolicy<S>,
    dispatcher: &D,
    first: &AttestationClaims,
    second: &AttestationClaims,
    payload: &[u8],
) -> Result<Vec<u8>, ServerError>
where
    S: BuildFloorStore,
    D: XpcRequestDispatcher,
{
    let principal = policy
        .authorize_pair(first, second)
        .map_err(ServerError::Auth)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(ServerError::InvalidPayload);
    }
    let request = decode_request(payload).map_err(ServerError::Protocol)?;
    let request_id = request.request_id.clone();
    let response = dispatcher.dispatch(&principal, request);
    if response.request_id != request_id {
        return Err(ServerError::ResponseRequestIdMismatch);
    }
    let encoded = encode_response(&response).map_err(ServerError::Protocol)?;
    if encoded.is_empty() || encoded.len() > MAX_FRAME_BYTES {
        return Err(ServerError::InvalidPayload);
    }
    Ok(encoded)
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
struct RuntimeDispatcher<'a> {
    runtime: &'a crate::watchdog::ProductionRuntime,
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl XpcRequestDispatcher for RuntimeDispatcher<'_> {
    fn dispatch(&self, principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope {
        let RequestEnvelope {
            request_id,
            request,
            ..
        } = request;
        let response = match request {
            Request::AcquireLease {
                profile,
                owner_generation,
                ttl_ms,
            } => self
                .runtime
                .acquire(principal, &profile, &owner_generation, ttl_ms)
                .map(|grant| Response::Acquired {
                    lease_id: grant.lease_id.as_str().to_owned(),
                    granted_ttl_ms: grant.granted_ttl_ms,
                }),
            Request::RenewLease {
                lease_id,
                owner_generation,
                ttl_ms,
            } => LeaseId::parse(lease_id)
                .map_err(|_| CoordinatorError::Internal)
                .and_then(|lease_id| {
                    self.runtime
                        .renew(principal, &lease_id, &owner_generation, ttl_ms)
                })
                .map(|grant| Response::Renewed {
                    lease_id: grant.lease_id.as_str().to_owned(),
                    granted_ttl_ms: grant.granted_ttl_ms,
                }),
            Request::ReleaseLease {
                lease_id,
                owner_generation,
            } => LeaseId::parse(lease_id)
                .map_err(|_| CoordinatorError::Internal)
                .and_then(|lease_id| {
                    self.runtime
                        .release(principal, &lease_id, &owner_generation)
                        .map(|()| lease_id)
                })
                .map(|lease_id| Response::Released {
                    lease_id: lease_id.as_str().to_owned(),
                }),
            Request::Status => self.runtime.status().map(|status| Response::Status {
                active_leases: status.active_leases,
                mutation_active: status.mutation_active,
                recovery_required: status.recovery_required,
            }),
        }
        .unwrap_or_else(|error| Response::Error {
            code: coordinator_error_code(error),
        });
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response,
        }
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
fn coordinator_error_code(error: CoordinatorError) -> ErrorCode {
    match error {
        CoordinatorError::Store(_) => ErrorCode::StateUnavailable,
        CoordinatorError::Pmset(_)
        | CoordinatorError::Random(_)
        | CoordinatorError::ClockUnavailable => ErrorCode::HelperUnavailable,
        CoordinatorError::Process(_)
        | CoordinatorError::VerificationFailed { .. }
        | CoordinatorError::RecoveryRequired => ErrorCode::RecoveryRequired,
        CoordinatorError::RuntimeGuard(outcome) => outcome.protocol_error_code(),
        CoordinatorError::Engine(engine) => match engine {
            EngineError::LeaseNotFound => ErrorCode::LeaseNotFound,
            EngineError::Expired | EngineError::LeaseNearExpiry => ErrorCode::LeaseExpired,
            EngineError::PrincipalMismatch => ErrorCode::Unauthorized,
            EngineError::OwnerGenerationMismatch => ErrorCode::OwnerMismatch,
            EngineError::DuplicateLease
            | EngineError::LeaseLimitReached
            | EngineError::NotExtended => ErrorCode::Conflict,
            EngineError::ClientBuildTooOld => ErrorCode::IncompatibleVersion,
            EngineError::PolicyMismatch
            | EngineError::MutationGenerationMismatch
            | EngineError::BootMismatch
            | EngineError::ProcessUnverifiable
            | EngineError::RecoveryRequired
            | EngineError::ObservedStateMismatch => ErrorCode::RecoveryRequired,
            EngineError::CorruptState(_) => ErrorCode::StateUnavailable,
            EngineError::InvalidIdentifier
            | EngineError::InvalidTtl
            | EngineError::DeadlineOverflow
            | EngineError::InvalidMutationGeneration => ErrorCode::InvalidRequest,
        },
        CoordinatorError::Internal => ErrorCode::Internal,
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
pub struct ProductionBuildFloorStore {
    lock: Mutex<()>,
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl ProductionBuildFloorStore {
    pub fn open() -> Result<Self, BuildFloorError> {
        validate_directory(Path::new(PRODUCTION_STATE_DIRECTORY))?;
        Ok(Self {
            lock: Mutex::new(()),
        })
    }

    fn floor_path() -> std::path::PathBuf {
        Path::new(PRODUCTION_STATE_DIRECTORY).join(BUILD_FLOOR_FILE)
    }

    fn temp_path() -> std::path::PathBuf {
        Path::new(PRODUCTION_STATE_DIRECTORY).join(BUILD_FLOOR_TEMP_FILE)
    }

    fn load_unlocked(&self) -> Result<u64, BuildFloorError> {
        let path = Self::floor_path();
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(_) => return Err(BuildFloorError::Unavailable),
        };
        validate_floor_file(&file)?;
        let mut text = String::new();
        Read::by_ref(&mut file)
            .take(21)
            .read_to_string(&mut text)
            .map_err(|_| BuildFloorError::Unavailable)?;
        if text.is_empty() || text.len() > 20 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(BuildFloorError::Unavailable);
        }
        text.parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(BuildFloorError::Unavailable)
    }

    fn replace_unlocked(&self, build: u64) -> Result<(), BuildFloorError> {
        let temp_path = Self::temp_path();
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|_| BuildFloorError::Unavailable)?;
        validate_floor_file(&temp)?;
        write!(temp, "{build}").map_err(|_| BuildFloorError::Unavailable)?;
        temp.sync_all().map_err(|_| BuildFloorError::Unavailable)?;
        fs::rename(&temp_path, Self::floor_path()).map_err(|_| BuildFloorError::Unavailable)?;
        File::open(PRODUCTION_STATE_DIRECTORY)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| BuildFloorError::Unavailable)
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl BuildFloorStore for ProductionBuildFloorStore {
    fn load(&self) -> Result<u64, BuildFloorError> {
        let _guard = self.lock.lock().map_err(|_| BuildFloorError::Unavailable)?;
        self.load_unlocked()
    }

    fn raise_to(&self, build: u64) -> Result<(), BuildFloorError> {
        let _guard = self.lock.lock().map_err(|_| BuildFloorError::Unavailable)?;
        let current = self.load_unlocked()?;
        if build <= current {
            return Ok(());
        }
        self.replace_unlocked(build)
    }

    fn accept_and_raise(
        &self,
        minimum_build: u64,
        candidate_build: u64,
    ) -> Result<bool, BuildFloorError> {
        let _guard = self.lock.lock().map_err(|_| BuildFloorError::Unavailable)?;
        let current = self.load_unlocked()?;
        if candidate_build < current.max(minimum_build) {
            return Ok(false);
        }
        if candidate_build > current {
            self.replace_unlocked(candidate_build)?;
        }
        Ok(true)
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
fn validate_directory(path: &Path) -> Result<(), BuildFloorError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| BuildFloorError::Unavailable)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(BuildFloorError::Unavailable);
    }
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
fn validate_floor_file(file: &File) -> Result<(), BuildFloorError> {
    let metadata = file.metadata().map_err(|_| BuildFloorError::Unavailable)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(BuildFloorError::Unavailable);
    }
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
#[repr(C)]
struct NativeClaims {
    team_id: [c_char; 11],
    identifier: [c_char; 129],
    signed_build: u64,
    euid: u32,
    pid: i32,
    start_seconds: u64,
    start_microseconds: u32,
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
struct NativeServer {
    policy: AttestationPolicy<ProductionBuildFloorStore>,
    runtime: ProductionRuntime,
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl NativeServer {
    fn process(
        &self,
        payload: &[u8],
        first: &NativeClaims,
        second: &NativeClaims,
    ) -> Result<Vec<u8>, ServerError> {
        let first = claims_from_native(first).map_err(ServerError::Auth)?;
        let second = claims_from_native(second).map_err(ServerError::Auth)?;
        process_attested(
            &self.policy,
            &RuntimeDispatcher {
                runtime: &self.runtime,
            },
            &first,
            &second,
            payload,
        )
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
fn claims_from_native(claims: &NativeClaims) -> Result<AttestationClaims, AuthError> {
    fn copy_text(value: &[c_char]) -> Result<String, AuthError> {
        // SAFETY: the native bridge always zero-initializes both fixed arrays
        // and rejects strings that do not fit with a trailing NUL byte.
        let value = unsafe { CStr::from_ptr(value.as_ptr()) };
        value
            .to_str()
            .ok()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .ok_or(AuthError::MissingClaim)
    }
    Ok(AttestationClaims {
        team_id: Some(copy_text(&claims.team_id)?),
        identifier: Some(copy_text(&claims.identifier)?),
        signed_build: Some(claims.signed_build),
        euid: Some(claims.euid),
        pid: Some(claims.pid),
        start_seconds: Some(claims.start_seconds),
        start_microseconds: Some(claims.start_microseconds),
    })
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
unsafe extern "C" fn native_message_handler(
    payload: *const u8,
    payload_length: usize,
    first: *const NativeClaims,
    second: *const NativeClaims,
    response: *mut u8,
    response_capacity: usize,
    response_length: *mut usize,
    context: *mut c_void,
) -> i32 {
    if payload.is_null()
        || payload_length == 0
        || payload_length > MAX_FRAME_BYTES
        || first.is_null()
        || second.is_null()
        || response.is_null()
        || response_capacity < MAX_FRAME_BYTES
        || response_length.is_null()
        || context.is_null()
    {
        return 1;
    }
    // SAFETY: every pointer and bound comes from the fixed native callback
    // contract and was checked above for nullness and maximum capacity.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let server = &*context.cast::<NativeServer>();
        let payload = std::slice::from_raw_parts(payload, payload_length);
        server.process(payload, &*first, &*second)
    }));
    let Ok(Ok(encoded)) = result else {
        return 1;
    };
    if encoded.is_empty() || encoded.len() > response_capacity {
        return 1;
    }
    // SAFETY: native supplied response_capacity bytes and encoded was checked
    // to fit. The regions do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), response, encoded.len());
        *response_length = encoded.len();
    }
    0
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductionRunError {
    NotRoot,
    Startup(CoordinatorError),
    Scheduler(SchedulerArmError),
    Policy(AuthError),
    Floor(BuildFloorError),
    Native,
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl fmt::Display for ProductionRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotRoot => "production power-helper must run as root",
            Self::Startup(_) => "production power-helper startup recovery failed",
            Self::Scheduler(_) => "production power-helper watchdog failed to arm",
            Self::Policy(_) => "production power-helper attestation policy is invalid",
            Self::Floor(_) => "production power-helper build floor is unavailable",
            Self::Native => "production power-helper XPC listener failed",
        })
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
impl std::error::Error for ProductionRunError {}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
pub fn run_production() -> Result<(), ProductionRunError> {
    // SAFETY: reads the effective uid without changing process state.
    if unsafe { libc::geteuid() } != 0 {
        return Err(ProductionRunError::NotRoot);
    }
    let team_id = env!("JARVIS_POWER_TEAM_ID");
    let minimum_build = env!("JARVIS_POWER_MINIMUM_CLIENT_BUILD")
        .parse::<u64>()
        .map_err(|_| ProductionRunError::Policy(AuthError::InvalidPolicy))?;
    let requirement = production_requirement(team_id).map_err(ProductionRunError::Policy)?;
    let requirement = CString::new(requirement)
        .map_err(|_| ProductionRunError::Policy(AuthError::InvalidPolicy))?;
    let runtime = ProductionStartup::open()
        .map_err(ProductionRunError::Startup)?
        .reconcile_before_listener()
        .map_err(ProductionRunError::Startup)?
        .arm_watchdog()
        .map_err(ProductionRunError::Scheduler)?;
    let _permit = runtime.listener_permit();
    let floor = ProductionBuildFloorStore::open().map_err(ProductionRunError::Floor)?;
    let policy =
        production_policy(team_id, minimum_build, floor).map_err(ProductionRunError::Policy)?;
    let mut server = NativeServer { policy, runtime };
    // SAFETY: all strings are fixed NUL-terminated values; server remains live
    // for the entire blocking listener call; callback ABI matches the header.
    let status = unsafe {
        jarvis_power_xpc_server_run(
            c"app.jarvis.monitor.power-helper".as_ptr(),
            requirement.as_ptr(),
            native_message_handler,
            (&mut server as *mut NativeServer).cast(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(ProductionRunError::Native)
    }
}

#[cfg(all(target_os = "macos", feature = "production-xpc"))]
extern "C" {
    fn jarvis_power_xpc_server_run(
        service_label: *const c_char,
        requirement_text: *const c_char,
        handler: unsafe extern "C" fn(
            payload: *const u8,
            payload_length: usize,
            first: *const NativeClaims,
            second: *const NativeClaims,
            response: *mut u8,
            response_capacity: usize,
            response_length: *mut usize,
            context: *mut c_void,
        ) -> i32,
        context: *mut c_void,
    ) -> i32;
}
