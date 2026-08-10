use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jarvis_power_core::protocol::{
    decode_response, encode_request, Request, RequestEnvelope, RequestId, Response,
    ResponseEnvelope, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use jarvis_power_core::state::Principal;
use jarvis_power_helper::xpc_server::{
    production_policy, AttestationClaims, AuthError, BuildFloorError, BuildFloorStore,
    MessageAttestor, MessageProcessor, ServerError, XpcRequestDispatcher,
};

const TEAM_ID: &str = "ABCDEFGHIJ";
const BUNDLE_ID: &str = "app.jarvis.monitor";
const MINIMUM_BUILD: u64 = 340;

#[derive(Clone)]
struct MemoryFloor {
    value: Arc<Mutex<u64>>,
}

impl MemoryFloor {
    fn new(value: u64) -> Self {
        Self {
            value: Arc::new(Mutex::new(value)),
        }
    }
}

impl BuildFloorStore for MemoryFloor {
    fn load(&self) -> Result<u64, BuildFloorError> {
        Ok(*self.value.lock().unwrap())
    }

    fn raise_to(&self, build: u64) -> Result<(), BuildFloorError> {
        let mut value = self.value.lock().unwrap();
        *value = (*value).max(build);
        Ok(())
    }
}

fn claims(team_id: &str, identifier: &str, build: u64) -> AttestationClaims {
    AttestationClaims {
        team_id: Some(team_id.to_owned()),
        identifier: Some(identifier.to_owned()),
        signed_build: Some(build),
        euid: Some(501),
        pid: Some(41),
        start_seconds: Some(1_700_000_000),
        start_microseconds: Some(123_456),
    }
}

#[test]
fn xpc_policy_pins_team_bundle_requirement_and_version_floor() {
    let policy = production_policy(TEAM_ID, MINIMUM_BUILD, MemoryFloor::new(MINIMUM_BUILD))
        .expect("valid fixed production policy");
    let valid = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD);
    let principal = policy
        .authorize_pair(&valid, &valid)
        .expect("valid signed app");
    assert_eq!(principal.team_id(), TEAM_ID);
    assert_eq!(principal.bundle_id(), BUNDLE_ID);
    assert_eq!(principal.signed_build(), MINIMUM_BUILD);
    assert!(principal.requirement_digest().iter().any(|byte| *byte != 0));

    let wrong_team = claims("ZZZZZZZZZZ", BUNDLE_ID, MINIMUM_BUILD);
    assert_eq!(
        policy.authorize_pair(&wrong_team, &wrong_team),
        Err(AuthError::WrongTeam)
    );
    let wrong_identifier = claims(TEAM_ID, "app.jarvis.fake", MINIMUM_BUILD);
    assert_eq!(
        policy.authorize_pair(&wrong_identifier, &wrong_identifier),
        Err(AuthError::WrongIdentifier)
    );
    let downgraded = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD - 1);
    assert_eq!(
        policy.authorize_pair(&downgraded, &downgraded),
        Err(AuthError::Downgrade)
    );
}

#[test]
fn xpc_missing_or_changing_identity_claims_fail_closed() {
    let policy = production_policy(TEAM_ID, MINIMUM_BUILD, MemoryFloor::new(MINIMUM_BUILD))
        .expect("valid fixed production policy");
    let complete = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD);

    let mut partial = complete.clone();
    partial.team_id = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.identifier = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.signed_build = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.euid = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.pid = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.start_seconds = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );
    partial = complete.clone();
    partial.start_microseconds = None;
    assert_eq!(
        policy.authorize_pair(&partial, &partial),
        Err(AuthError::MissingClaim)
    );

    let mut changed = complete.clone();
    changed.pid = Some(complete.pid.unwrap() + 1);
    assert_eq!(
        policy.authorize_pair(&complete, &changed),
        Err(AuthError::ChangedClaims)
    );
}

#[test]
fn xpc_accepted_higher_build_is_persisted_as_the_next_policy_floor() {
    let store = MemoryFloor::new(MINIMUM_BUILD);
    let first_policy =
        production_policy(TEAM_ID, MINIMUM_BUILD, store.clone()).expect("first policy");
    let higher = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD + 10);
    first_policy
        .authorize_pair(&higher, &higher)
        .expect("higher build accepted");
    drop(first_policy);

    let restarted_policy =
        production_policy(TEAM_ID, MINIMUM_BUILD, store).expect("restarted policy");
    let older = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD + 9);
    assert_eq!(
        restarted_policy.authorize_pair(&older, &older),
        Err(AuthError::Downgrade)
    );
}

struct QueueAttestor {
    claims: Mutex<Vec<(AttestationClaims, AttestationClaims)>>,
    count: Arc<AtomicUsize>,
}

impl MessageAttestor for QueueAttestor {
    fn attest(&self) -> Result<(AttestationClaims, AttestationClaims), AuthError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        let mut claims = self.claims.lock().unwrap();
        if claims.is_empty() {
            return Err(AuthError::MissingClaim);
        }
        Ok(claims.remove(0))
    }
}

struct StatusDispatcher {
    count: Arc<AtomicUsize>,
}

impl XpcRequestDispatcher for StatusDispatcher {
    fn dispatch(&self, _principal: &Principal, request: RequestEnvelope) -> ResponseEnvelope {
        self.count.fetch_add(1, Ordering::SeqCst);
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            response: Response::Status {
                active_leases: 0,
                mutation_active: false,
                recovery_required: false,
            },
        }
    }
}

fn status_payload() -> Vec<u8> {
    encode_request(&RequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::parse("018f0000-0000-7000-8000-000000000001").unwrap(),
        request: Request::Status,
    })
    .unwrap()
}

#[test]
fn xpc_each_message_is_attested_not_only_connection_setup() {
    let valid = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD);
    let wrong_team = claims("ZZZZZZZZZZ", BUNDLE_ID, MINIMUM_BUILD);
    let attestation_count = Arc::new(AtomicUsize::new(0));
    let dispatch_count = Arc::new(AtomicUsize::new(0));
    let processor = MessageProcessor::new(
        production_policy(TEAM_ID, MINIMUM_BUILD, MemoryFloor::new(MINIMUM_BUILD)).unwrap(),
        QueueAttestor {
            claims: Mutex::new(vec![
                (valid.clone(), valid),
                (wrong_team.clone(), wrong_team),
            ]),
            count: attestation_count.clone(),
        },
        StatusDispatcher {
            count: dispatch_count.clone(),
        },
    );

    let response = processor.process(&status_payload()).expect("first message");
    assert!(matches!(
        decode_response(response).unwrap().response,
        Response::Status { .. }
    ));
    assert_eq!(
        processor.process(&status_payload()),
        Err(ServerError::Auth(AuthError::WrongTeam))
    );
    assert_eq!(attestation_count.load(Ordering::SeqCst), 2);
    assert_eq!(dispatch_count.load(Ordering::SeqCst), 1);
}

#[test]
fn xpc_every_message_payload_is_fixed_nonempty_and_bounded_before_dispatch() {
    let valid = claims(TEAM_ID, BUNDLE_ID, MINIMUM_BUILD);
    let attestation_count = Arc::new(AtomicUsize::new(0));
    let dispatch_count = Arc::new(AtomicUsize::new(0));
    let processor = MessageProcessor::new(
        production_policy(TEAM_ID, MINIMUM_BUILD, MemoryFloor::new(MINIMUM_BUILD)).unwrap(),
        QueueAttestor {
            claims: Mutex::new(vec![(valid.clone(), valid.clone()), (valid.clone(), valid)]),
            count: attestation_count.clone(),
        },
        StatusDispatcher {
            count: dispatch_count.clone(),
        },
    );

    assert_eq!(processor.process(&[]), Err(ServerError::InvalidPayload));
    assert_eq!(
        processor.process(&vec![0_u8; MAX_FRAME_BYTES + 1]),
        Err(ServerError::InvalidPayload)
    );
    assert_eq!(attestation_count.load(Ordering::SeqCst), 2);
    assert_eq!(dispatch_count.load(Ordering::SeqCst), 0);
}
