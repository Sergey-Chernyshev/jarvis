use jarvis_power_core::engine::{Effect, Engine, EngineConfig, EngineError, Plan, ProcessState};
use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, LeaseId, MonotonicTime, MutationPhase, Principal,
    StateError,
};

const BOOT_ID: &str = "boot-2026-07-31";
const SERVICE_VERSION: u64 = 2;
const MINIMUM_CLIENT_BUILD: u64 = 100;
const LEASE_A: &str = "0123456789abcdef0123456789abcdef";
const LEASE_B: &str = "fedcba9876543210fedcba9876543210";

fn config() -> EngineConfig {
    EngineConfig::new(SERVICE_VERSION, MINIMUM_CLIENT_BUILD, BOOT_ID).expect("valid engine config")
}

fn principal(seed: u8, build: u64) -> Principal {
    Principal::from_helper_attestation(
        501,
        i32::from(seed) + 100,
        DarwinProcessIdentity::new(1, 1_722_400_000 + u64::from(seed), u32::from(seed))
            .expect("valid process identity"),
        "app.jarvis.monitor",
        "TEAMID1234",
        [seed; 32],
        build,
    )
    .expect("valid attested principal")
}

fn owner() -> Principal {
    principal(1, MINIMUM_CLIENT_BUILD)
}

fn other_owner() -> Principal {
    principal(2, MINIMUM_CLIENT_BUILD)
}

fn lease_id(value: &str) -> LeaseId {
    LeaseId::parse(value).expect("valid lease id")
}

fn now(value: u64) -> MonotonicTime {
    MonotonicTime::from_millis(value)
}

fn first_acquire(baseline: bool) -> Plan {
    Engine::empty(config())
        .acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(0),
            5_000,
            baseline,
            1,
        )
        .expect("first acquire")
}

#[derive(Clone, Debug)]
struct World {
    persisted: Option<HelperState>,
    sleep_disabled: bool,
    verified: Vec<bool>,
}

impl World {
    fn baseline(sleep_disabled: bool) -> Self {
        Self {
            persisted: None,
            sleep_disabled,
            verified: Vec::new(),
        }
    }

    fn execute(&mut self, effect: &Effect) {
        match effect {
            Effect::PersistState(state) => self.persisted = Some(state.clone()),
            Effect::CompareAndSetDisabled(target) => self.sleep_disabled = *target,
            Effect::VerifyDisabled(expected) => {
                assert_eq!(self.sleep_disabled, *expected);
                self.verified.push(*expected);
            }
            Effect::ClearState => {
                assert_eq!(
                    self.verified.last(),
                    Some(&self.sleep_disabled),
                    "state may clear only after matching read-back"
                );
                self.persisted = None;
            }
        }
    }

    fn execute_plan(&mut self, plan: &Plan) {
        for effect in &plan.effects {
            self.execute(effect);
        }
    }

    fn execute_prefix(&mut self, plan: &Plan, count: usize) {
        for effect in plan.effects.iter().take(count) {
            self.execute(effect);
        }
    }

    fn engine(&self) -> Engine {
        match self.persisted.clone() {
            Some(state) => Engine::from_state(config(), state),
            None => Engine::empty(config()),
        }
    }

    fn converge(&mut self, at: u64) {
        for _ in 0..3 {
            let plan = self
                .engine()
                .reconcile(BOOT_ID, now(at), |_| ProcessState::Dead)
                .expect("reconciliation must converge");
            if plan.effects.is_empty() {
                return;
            }
            self.execute_plan(&plan);
        }
        panic!("world did not converge");
    }
}

fn applied_world(baseline: bool) -> World {
    let mut world = World::baseline(baseline);
    world.execute_plan(&first_acquire(baseline));
    world
}

fn assert_restore_order(plan: &Plan, baseline: bool, did_mutate: bool) {
    let mut index = 0;
    match &plan.effects[index] {
        Effect::PersistState(state) => {
            assert_eq!(state.phase, MutationPhase::RestorePending);
            assert!(state.leases.is_empty());
        }
        effect => panic!("expected restore tombstone first, got {effect:?}"),
    }
    index += 1;

    if did_mutate {
        assert_eq!(plan.effects[index], Effect::CompareAndSetDisabled(baseline));
        index += 1;
    }

    assert_eq!(plan.effects[index], Effect::VerifyDisabled(baseline));
    assert_eq!(plan.effects[index + 1], Effect::ClearState);
    assert_eq!(plan.effects.len(), index + 2);
}

#[test]
fn expired_last_mutating_lease_persists_restore_pending_before_restore_verify_and_clear() {
    let world = applied_world(false);
    let plan = world
        .engine()
        .reconcile(BOOT_ID, now(5_000), |_| ProcessState::AliveExact)
        .expect("expired lease reconciliation");

    assert_restore_order(&plan, false, true);
    assert!(plan.state.is_none());
}

#[test]
fn renew_never_resurrects_an_expired_lease() {
    let world = applied_world(false);
    let result = world.engine().renew(
        &owner(),
        &lease_id(LEASE_A),
        "generation-a",
        BOOT_ID,
        now(5_000),
        45_000,
    );

    assert_eq!(result, Err(EngineError::Expired));
}

#[test]
fn baseline_true_never_mutates_or_writes_false() {
    let plan = first_acquire(true);
    let state = plan.state.as_ref().expect("applied state");

    assert!(!state.did_mutate);
    assert!(!plan
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::CompareAndSetDisabled(false))));

    let mut world = World::baseline(true);
    for crash_after in 0..=plan.effects.len() {
        let mut crashed = world.clone();
        crashed.execute_prefix(&plan, crash_after);
        crashed.converge(5_000);
        assert!(crashed.sleep_disabled);
        assert!(crashed.persisted.is_none());
    }

    world.execute_plan(&plan);
    let restore = world
        .engine()
        .reconcile(BOOT_ID, now(5_000), |_| ProcessState::Dead)
        .expect("restore external baseline");
    assert_restore_order(&restore, true, false);
}

#[test]
fn crash_after_every_acquire_effect_converges_to_the_original_baseline() {
    let plan = first_acquire(false);

    for crash_after in 0..=plan.effects.len() {
        let mut world = World::baseline(false);
        world.execute_prefix(&plan, crash_after);
        world.converge(5_000);
        assert!(
            !world.sleep_disabled,
            "sleep remained disabled after crash point {crash_after}"
        );
        assert!(
            world.persisted.is_none(),
            "state remained after crash point {crash_after}"
        );
    }
}

#[test]
fn crash_after_every_restore_effect_resumes_without_clearing_before_verify() {
    let acquired = applied_world(false);
    let restore = acquired
        .engine()
        .reconcile(BOOT_ID, now(5_000), |_| ProcessState::Dead)
        .expect("initial restore");

    for crash_after in 0..=restore.effects.len() {
        let mut world = acquired.clone();
        world.execute_prefix(&restore, crash_after);
        world.converge(5_000);
        assert!(!world.sleep_disabled);
        assert!(world.persisted.is_none());
        assert_eq!(world.verified.last(), Some(&false));
    }
}

#[test]
fn prepared_and_restore_pending_partial_phases_resume_conservatively() {
    let acquire = first_acquire(false);
    let mut prepared_world = World::baseline(false);
    prepared_world.execute_prefix(&acquire, 1);
    assert_eq!(
        prepared_world
            .persisted
            .as_ref()
            .expect("prepared tombstone")
            .phase,
        MutationPhase::Prepared
    );

    let cleanup = prepared_world
        .engine()
        .reconcile(BOOT_ID, now(1), |_| ProcessState::AliveExact)
        .expect("prepared cleanup");
    assert_restore_order(&cleanup, false, true);

    prepared_world.execute_prefix(&cleanup, 1);
    assert_eq!(
        prepared_world
            .persisted
            .as_ref()
            .expect("restore tombstone")
            .phase,
        MutationPhase::RestorePending
    );
    let resumed = prepared_world
        .engine()
        .reconcile(BOOT_ID, now(2), |_| ProcessState::AliveExact)
        .expect("resume restore");
    assert_eq!(
        resumed.effects,
        vec![
            Effect::CompareAndSetDisabled(false),
            Effect::VerifyDisabled(false),
            Effect::ClearState,
        ]
    );
}

#[test]
fn dead_or_mismatched_process_restores_before_ttl_expiry() {
    for process_state in [ProcessState::Dead, ProcessState::Mismatch] {
        let world = applied_world(false);
        let plan = world
            .engine()
            .reconcile(BOOT_ID, now(1), |_| process_state)
            .expect("dead or mismatched owner cleanup");
        assert_restore_order(&plan, false, true);
    }
}

#[test]
fn unverifiable_process_and_boot_mismatch_fail_closed_with_state_retained() {
    let world = applied_world(false);
    assert_eq!(
        world
            .engine()
            .reconcile(BOOT_ID, now(1), |_| ProcessState::Unverifiable),
        Err(EngineError::ProcessUnverifiable)
    );
    assert_eq!(
        world
            .engine()
            .reconcile("different-boot", now(1), |_| ProcessState::AliveExact),
        Err(EngineError::BootMismatch)
    );
    assert!(world.persisted.is_some());
    assert!(world.sleep_disabled);
}

#[test]
fn corrupt_state_fails_closed_without_effects() {
    let world = applied_world(false);
    let mut wrong_schema = world.persisted.clone().expect("applied state");
    wrong_schema.schema_version += 1;
    assert_eq!(
        Engine::from_state(config(), wrong_schema)
            .reconcile(BOOT_ID, now(5_000), |_| ProcessState::Dead),
        Err(EngineError::CorruptState(
            StateError::UnsupportedSchemaVersion
        ))
    );

    let mut impossible_baseline = world.persisted.expect("applied state");
    impossible_baseline.baseline = true;
    assert_eq!(
        Engine::from_state(config(), impossible_baseline)
            .reconcile(BOOT_ID, now(5_000), |_| ProcessState::Dead),
        Err(EngineError::CorruptState(
            StateError::InvalidMutationInvariant
        ))
    );
}

#[test]
fn multiple_profiles_restore_only_when_the_last_lease_leaves() {
    let mut world = applied_world(false);
    let second = world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(10),
            5_000,
            true,
            1,
        )
        .expect("second acquire");
    assert_eq!(second.effects.len(), 1);
    assert!(matches!(second.effects[0], Effect::PersistState(_)));
    world.execute_plan(&second);

    let release_first = world
        .engine()
        .release(&owner(), &lease_id(LEASE_A), "generation-a", BOOT_ID)
        .expect("release first");
    assert_eq!(release_first.effects.len(), 1);
    assert!(!release_first.effects.iter().any(|effect| matches!(
        effect,
        Effect::CompareAndSetDisabled(_) | Effect::ClearState
    )));
    world.execute_plan(&release_first);
    assert!(world.sleep_disabled);

    let release_last = world
        .engine()
        .release(&other_owner(), &lease_id(LEASE_B), "generation-b", BOOT_ID)
        .expect("release last");
    assert_restore_order(&release_last, false, true);
}

#[test]
fn renew_and_release_require_exact_principal_and_owner_generation() {
    let world = applied_world(false);

    assert_eq!(
        world.engine().renew(
            &other_owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            5_000,
        ),
        Err(EngineError::PrincipalMismatch)
    );
    assert_eq!(
        world.engine().renew(
            &owner(),
            &lease_id(LEASE_A),
            "different-generation",
            BOOT_ID,
            now(1),
            5_000,
        ),
        Err(EngineError::OwnerGenerationMismatch)
    );
    assert_eq!(
        world
            .engine()
            .release(&other_owner(), &lease_id(LEASE_A), "generation-a", BOOT_ID,),
        Err(EngineError::PrincipalMismatch)
    );
}

#[test]
fn renew_extends_only_the_existing_exact_lease_and_persists_it() {
    let world = applied_world(false);
    let plan = world
        .engine()
        .renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1_000),
            45_000,
        )
        .expect("renew exact lease");

    assert_eq!(plan.effects.len(), 1);
    let state = plan.state.as_ref().expect("renewed state");
    assert_eq!(state.leases[0].deadline.as_millis(), 46_000);
    assert_eq!(plan.effects, vec![Effect::PersistState(state.clone())]);
}

#[test]
fn ttl_and_monotonic_deadline_overflow_are_rejected() {
    let engine = Engine::empty(config());
    assert_eq!(
        engine.acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(0),
            4_999,
            false,
            1,
        ),
        Err(EngineError::InvalidTtl)
    );
    assert_eq!(
        engine.acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(u64::MAX - 4_999),
            5_000,
            false,
            1,
        ),
        Err(EngineError::DeadlineOverflow)
    );
    assert_eq!(
        engine.acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(0),
            5_000,
            false,
            u64::MAX,
        ),
        Err(EngineError::InvalidMutationGeneration)
    );
}

#[test]
fn persisted_deadline_beyond_the_maximum_ttl_fails_closed() {
    let world = applied_world(false);
    let mut corrupt = world.persisted.expect("applied state");
    corrupt.leases[0].deadline = now(120_001);

    assert_eq!(
        Engine::from_state(config(), corrupt)
            .reconcile(BOOT_ID, now(0), |_| ProcessState::AliveExact),
        Err(EngineError::CorruptState(StateError::InvalidLease))
    );
}

#[test]
fn old_client_build_and_stale_mutation_generation_are_rejected() {
    assert_eq!(
        Engine::empty(config()).acquire(
            &principal(3, MINIMUM_CLIENT_BUILD - 1),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(0),
            5_000,
            false,
            1,
        ),
        Err(EngineError::ClientBuildTooOld)
    );

    let world = applied_world(false);
    assert_eq!(
        world.engine().acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(1),
            5_000,
            true,
            2,
        ),
        Err(EngineError::MutationGenerationMismatch)
    );
}

#[test]
fn duplicate_lease_ids_and_unsafe_identifiers_are_rejected() {
    let world = applied_world(false);
    assert_eq!(
        world.engine().acquire(
            &other_owner(),
            lease_id(LEASE_A),
            "work",
            "generation-b",
            now(1),
            5_000,
            true,
            1,
        ),
        Err(EngineError::DuplicateLease)
    );
    assert_eq!(
        Engine::empty(config()).acquire(
            &owner(),
            lease_id(LEASE_B),
            "..",
            "generation-b",
            now(1),
            5_000,
            false,
            1,
        ),
        Err(EngineError::InvalidIdentifier)
    );
}

#[test]
fn principal_validation_rejects_unverified_shapes() {
    assert_eq!(
        Principal::from_helper_attestation(
            501,
            101,
            DarwinProcessIdentity::new(1, 1, 0).expect("identity"),
            "app.jarvis.monitor",
            "short",
            [1; 32],
            100,
        ),
        Err(StateError::InvalidPrincipal)
    );
    assert_eq!(
        DarwinProcessIdentity::new(1, 1, 1_000_000),
        Err(StateError::InvalidProcessIdentity)
    );
}
