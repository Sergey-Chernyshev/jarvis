use jarvis_power_core::engine::{
    AcquireOutcome, AcquireResult, Effect, Engine, EngineConfig, EngineError, Plan, ProcessState,
    RuntimeGuardError, RuntimeGuardFailureOutcome,
};
use jarvis_power_core::protocol::{ErrorCode, ProtocolError, Response, MIN_TTL_MS};
use jarvis_power_core::state::{
    DarwinProcessIdentity, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    StateError, MAX_ACTIVE_LEASES,
};

const BOOT_ID: &str = "boot-2026-07-31";
const SERVICE_VERSION: u64 = 2;
const MINIMUM_CLIENT_BUILD: u64 = 100;
const LEASE_A: &str = "0123456789abcdef0123456789abcdef";
const LEASE_B: &str = "fedcba9876543210fedcba9876543210";
const LEASE_C: &str = "00112233445566778899aabbccddeeff";

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

fn acquire_with_ttl(baseline: bool, ttl_ms: u64) -> AcquireResult {
    Engine::empty(config())
        .acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(0),
            ttl_ms,
            baseline,
            1,
        )
        .expect("first acquire")
}

fn first_acquire(baseline: bool) -> Plan {
    acquire_with_ttl(baseline, 5_000).plan
}

type RuntimeEffectError = RuntimeGuardError;

#[derive(Clone, Debug)]
struct World {
    persisted: Option<HelperState>,
    sleep_disabled: bool,
    verified: Vec<bool>,
    monotonic_now: MonotonicTime,
    immediate_reconciliations: usize,
    reconciliation_failure_injected: bool,
}

impl World {
    fn baseline(sleep_disabled: bool) -> Self {
        Self {
            persisted: None,
            sleep_disabled,
            verified: Vec::new(),
            monotonic_now: now(0),
            immediate_reconciliations: 0,
            reconciliation_failure_injected: false,
        }
    }

    fn execute(&mut self, effect: &Effect) {
        self.try_execute(effect).expect("runtime effect");
    }

    fn try_execute(&mut self, effect: &Effect) -> Result<Option<u64>, RuntimeEffectError> {
        match effect {
            Effect::PersistState(state) => self.persisted = Some(state.clone()),
            Effect::CheckDeadline(deadline) => {
                if self.monotonic_now >= *deadline {
                    return Err(RuntimeEffectError::DeadlineExpired);
                }
            }
            Effect::CheckRemainingTtl(deadline, minimum_ttl_ms) => {
                let remaining_ttl_ms = deadline
                    .as_millis()
                    .saturating_sub(self.monotonic_now.as_millis());
                if remaining_ttl_ms < *minimum_ttl_ms {
                    return Err(RuntimeEffectError::RemainingTtlTooShort);
                }
                return Ok(Some(remaining_ttl_ms));
            }
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
        Ok(None)
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

    fn execute_plan_with_slow_persist(
        &mut self,
        plan: &Plan,
        slow_persist_index: usize,
        completes_at: MonotonicTime,
    ) -> Result<Option<u64>, RuntimeGuardFailureOutcome> {
        self.execute_guarded_plan(plan, Some((slow_persist_index, completes_at)))
    }

    fn execute_response_plan(&mut self, plan: &Plan) -> Result<u64, RuntimeGuardFailureOutcome> {
        self.execute_guarded_plan(plan, None)
            .map(|ttl_ms| ttl_ms.expect("response plan must measure remaining TTL"))
    }

    fn execute_guarded_plan(
        &mut self,
        plan: &Plan,
        slow_persist: Option<(usize, MonotonicTime)>,
    ) -> Result<Option<u64>, RuntimeGuardFailureOutcome> {
        let mut persisted_in_plan = false;
        let mut response_ttl_ms = None;
        for (index, effect) in plan.effects.iter().enumerate() {
            match self.try_execute(effect) {
                Ok(measured_ttl_ms) => {
                    if measured_ttl_ms.is_some() {
                        assert!(response_ttl_ms.is_none(), "TTL may be measured only once");
                        response_ttl_ms = measured_ttl_ms;
                    }
                }
                Err(error) => {
                    let needs_reconciliation =
                        persisted_in_plan || matches!(effect, Effect::CheckRemainingTtl(_, _));
                    let outcome = if needs_reconciliation && self.reconcile_alive_now().is_err() {
                        RuntimeGuardFailureOutcome::RecoveryRequired(error)
                    } else {
                        RuntimeGuardFailureOutcome::Recovered(error)
                    };
                    return Err(outcome);
                }
            }
            if matches!(effect, Effect::PersistState(_)) {
                persisted_in_plan = true;
                if let Some((slow_persist_index, completes_at)) = slow_persist {
                    if index == slow_persist_index {
                        self.monotonic_now = completes_at;
                    }
                }
            }
        }
        Ok(response_ttl_ms)
    }

    fn engine(&self) -> Engine {
        match self.persisted.clone() {
            Some(state) => Engine::from_state(config(), state),
            None => Engine::empty(config()),
        }
    }

    fn converge(&mut self, at: u64) {
        self.monotonic_now = now(at);
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

    fn reconcile_alive_now(&mut self) -> Result<(), ()> {
        self.immediate_reconciliations += 1;
        if self.reconciliation_failure_injected {
            return Err(());
        }
        let at = self.monotonic_now;
        for _ in 0..3 {
            let plan = self
                .engine()
                .reconcile(BOOT_ID, at, |_| ProcessState::AliveExact)
                .map_err(|_| ())?;
            if plan.effects.is_empty() {
                return Ok(());
            }
            self.execute_plan(&plan);
        }
        Err(())
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

fn assert_single_final_ttl_sample(plan: &Plan, deadline: MonotonicTime) {
    assert_eq!(
        plan.effects
            .iter()
            .filter(|effect| matches!(effect, Effect::CheckRemainingTtl(_, _)))
            .count(),
        1
    );
    assert_eq!(
        plan.effects.last(),
        Some(&Effect::CheckRemainingTtl(deadline, MIN_TTL_MS))
    );
}

#[test]
fn runtime_ttl_guard_failures_have_a_closed_protocol_mapping() {
    for failure in [
        RuntimeGuardError::DeadlineExpired,
        RuntimeGuardError::RemainingTtlTooShort,
    ] {
        assert_eq!(
            RuntimeGuardFailureOutcome::Recovered(failure).protocol_error_code(),
            ErrorCode::LeaseExpired
        );
        assert_eq!(
            RuntimeGuardFailureOutcome::RecoveryRequired(failure).protocol_error_code(),
            ErrorCode::RecoveryRequired
        );
    }

    for response in [
        Response::Acquired {
            lease_id: LEASE_A.to_owned(),
            granted_ttl_ms: 1,
        },
        Response::Renewed {
            lease_id: LEASE_A.to_owned(),
            granted_ttl_ms: 1,
        },
    ] {
        assert_eq!(response.validate(), Err(ProtocolError::InvalidResponse));
    }
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
    let second_result = world
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
    assert!(matches!(
        second_result.outcome,
        AcquireOutcome::Created { .. }
    ));
    let second = second_result.plan;
    assert_eq!(second.effects.len(), 3);
    assert_eq!(second.effects[0], Effect::CheckDeadline(now(5_010)));
    assert!(matches!(second.effects[1], Effect::PersistState(_)));
    assert_eq!(
        second.effects[2],
        Effect::CheckRemainingTtl(now(5_010), MIN_TTL_MS)
    );
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
fn active_lease_cap_accepts_the_operational_limit_and_rejects_abuse_sizes() {
    let mut capped = applied_world(false).persisted.expect("applied state");
    capped.leases = (0..MAX_ACTIVE_LEASES)
        .map(|index| Lease {
            lease_id: lease_id(&format!("{index:032x}")),
            profile: format!("profile-{index}"),
            owner_generation: format!("generation-{index}"),
            principal: owner(),
            deadline: now(5_000),
        })
        .collect();
    assert_eq!(capped.validate(), Ok(()));

    assert_eq!(
        Engine::from_state(config(), capped.clone()).acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "overflow",
            "generation-overflow",
            now(1),
            5_000,
            true,
            1,
        ),
        Err(EngineError::LeaseLimitReached)
    );

    let template = capped.leases[0].clone();
    let mut over_cap = capped.clone();
    over_cap.leases.push(template.clone());
    assert_eq!(over_cap.validate(), Err(StateError::TooManyLeases));

    for abusive_size in [2_048, 10_000] {
        let mut abusive = capped.clone();
        abusive.leases.resize(abusive_size, template.clone());
        assert_eq!(abusive.validate(), Err(StateError::TooManyLeases));
    }
}

#[test]
fn policy_drift_blocks_admission_but_release_and_reconcile_restore() {
    let mut stale = applied_world(false).persisted.expect("applied state");
    stale.service_version = 1;
    stale.minimum_client_build = 1;
    assert_eq!(stale.validate(), Ok(()));
    let engine = Engine::from_state(config(), stale.clone());

    assert_eq!(
        engine.acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(1),
            5_000,
            true,
            1,
        ),
        Err(EngineError::PolicyMismatch)
    );
    assert_eq!(
        engine.renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            45_000,
        ),
        Err(EngineError::PolicyMismatch)
    );

    let release = engine
        .release(&owner(), &lease_id(LEASE_A), "generation-a", BOOT_ID)
        .expect("release remains risk-reducing");
    assert_restore_order(&release, false, true);

    let reconcile = Engine::from_state(config(), stale)
        .reconcile(BOOT_ID, now(1), |_| ProcessState::AliveExact)
        .expect("stale policy restores instead of retaining");
    assert_restore_order(&reconcile, false, true);
}

#[test]
fn release_under_stale_policy_restores_even_when_another_lease_remains() {
    let mut stale = applied_world(false).persisted.expect("applied state");
    stale.leases.push(Lease {
        lease_id: lease_id(LEASE_B),
        profile: "work".to_owned(),
        owner_generation: "generation-b".to_owned(),
        principal: other_owner(),
        deadline: now(5_000),
    });
    stale.service_version = 1;
    stale.minimum_client_build = 1;
    assert_eq!(stale.validate(), Ok(()));

    let release = Engine::from_state(config(), stale)
        .release(&owner(), &lease_id(LEASE_A), "generation-a", BOOT_ID)
        .expect("exact release remains available under stale policy");

    assert_restore_order(&release, false, true);
}

#[test]
fn lost_acquire_response_returns_the_existing_authoritative_lease() {
    let created = acquire_with_ttl(false, 45_000);
    assert_eq!(
        created.outcome,
        AcquireOutcome::Created {
            lease_id: lease_id(LEASE_A),
            deadline: now(45_000),
        }
    );
    let mut world = World::baseline(false);
    world.execute_plan(&created.plan);

    let retry = world
        .engine()
        .acquire(
            &owner(),
            lease_id(LEASE_B),
            "prod",
            "generation-a",
            now(1_000),
            45_000,
            true,
            1,
        )
        .expect("lost-response retry");
    let AcquireResult { plan, outcome } = retry;
    assert_eq!(
        plan.effects,
        vec![Effect::CheckRemainingTtl(now(45_000), MIN_TTL_MS)]
    );
    assert_eq!(plan.state.as_ref().expect("state").leases.len(), 1);
    let existing_id = match outcome {
        AcquireOutcome::Existing { lease_id, deadline } => {
            assert_eq!(deadline, now(45_000));
            lease_id
        }
        outcome => panic!("expected existing lease, got {outcome:?}"),
    };
    assert_eq!(existing_id, lease_id(LEASE_A));
    assert_ne!(existing_id, lease_id(LEASE_B));

    world.monotonic_now = now(2_000);
    assert_eq!(
        world
            .execute_response_plan(&plan)
            .expect("runtime TTL remains admissible"),
        43_000
    );

    let release = world
        .engine()
        .release(&owner(), &existing_id, "generation-a", BOOT_ID)
        .expect("release authoritative lease");
    assert_restore_order(&release, false, true);
}

#[test]
fn idempotent_acquire_rejects_existing_lease_below_minimum_remaining_ttl() {
    let mut world = applied_world(false);
    let exact_boundary = world
        .engine()
        .acquire(
            &owner(),
            lease_id(LEASE_B),
            "prod",
            "generation-a",
            now(0),
            45_000,
            true,
            1,
        )
        .expect("minimum remaining TTL is reusable");
    assert_eq!(
        exact_boundary.outcome,
        AcquireOutcome::Existing {
            lease_id: lease_id(LEASE_A),
            deadline: now(5_000),
        }
    );
    assert_eq!(
        exact_boundary.plan.effects,
        vec![Effect::CheckRemainingTtl(now(5_000), MIN_TTL_MS)]
    );

    assert_eq!(
        world.engine().acquire(
            &owner(),
            lease_id(LEASE_B),
            "prod",
            "generation-a",
            now(1),
            45_000,
            true,
            1,
        ),
        Err(EngineError::LeaseNearExpiry)
    );

    world.converge(5_000);
    assert!(!world.sleep_disabled);
    assert!(world.persisted.is_none());
}

#[test]
fn idempotent_retry_cannot_mask_another_expired_lease() {
    let mut world = applied_world(false);
    let second = world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(0),
            45_000,
            true,
            1,
        )
        .expect("second lease");
    world.execute_plan(&second.plan);

    assert_eq!(
        world.engine().acquire(
            &other_owner(),
            lease_id(LEASE_C),
            "work",
            "generation-b",
            now(5_000),
            45_000,
            true,
            1,
        ),
        Err(EngineError::RecoveryRequired)
    );
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

    assert_eq!(plan.effects.len(), 3);
    let state = plan.state.as_ref().expect("renewed state");
    assert_eq!(state.leases[0].deadline.as_millis(), 46_000);
    assert_eq!(
        plan.effects,
        vec![
            Effect::CheckDeadline(now(5_000)),
            Effect::PersistState(state.clone()),
            Effect::CheckRemainingTtl(now(46_000), MIN_TTL_MS),
        ]
    );
}

#[test]
fn renew_rejects_shorter_or_equal_deadline() {
    let mut world = World::baseline(false);
    world.execute_plan(&acquire_with_ttl(false, 45_000).plan);

    for (renew_at, ttl_ms) in [(1_000, 5_000), (40_000, 5_000)] {
        assert_eq!(
            world.engine().renew(
                &owner(),
                &lease_id(LEASE_A),
                "generation-a",
                BOOT_ID,
                now(renew_at),
                ttl_ms,
            ),
            Err(EngineError::NotExtended)
        );
    }
}

#[test]
fn runtime_deadline_checks_guard_slow_first_acquire_additional_acquire_and_renew() {
    let acquire = first_acquire(false);
    assert!(matches!(acquire.effects[0], Effect::PersistState(_)));
    assert_eq!(acquire.effects[1], Effect::CheckDeadline(now(5_000)));
    assert_eq!(acquire.effects[2], Effect::CompareAndSetDisabled(true));
    assert_eq!(acquire.effects[3], Effect::VerifyDisabled(true));
    assert_eq!(acquire.effects[4], Effect::CheckDeadline(now(5_000)));
    assert!(matches!(acquire.effects[5], Effect::PersistState(_)));

    let mut delayed_before_mutation = World::baseline(false);
    delayed_before_mutation.execute(&acquire.effects[0]);
    delayed_before_mutation.monotonic_now = now(8_000);
    assert_eq!(
        delayed_before_mutation.try_execute(&acquire.effects[1]),
        Err(RuntimeEffectError::DeadlineExpired)
    );
    assert!(!delayed_before_mutation.sleep_disabled);
    assert_eq!(
        delayed_before_mutation
            .persisted
            .as_ref()
            .expect("prepared recovery evidence")
            .phase,
        MutationPhase::Prepared
    );
    delayed_before_mutation.converge(8_000);
    assert!(delayed_before_mutation.persisted.is_none());

    let mut delayed_before_applied_persist = World::baseline(false);
    delayed_before_applied_persist.execute_prefix(&acquire, 4);
    delayed_before_applied_persist.monotonic_now = now(8_000);
    assert_eq!(
        delayed_before_applied_persist.try_execute(&acquire.effects[4]),
        Err(RuntimeEffectError::DeadlineExpired)
    );
    assert!(delayed_before_applied_persist.sleep_disabled);
    assert_eq!(
        delayed_before_applied_persist
            .persisted
            .as_ref()
            .expect("prepared recovery evidence")
            .phase,
        MutationPhase::Prepared
    );
    delayed_before_applied_persist.converge(8_000);
    assert!(!delayed_before_applied_persist.sleep_disabled);
    assert!(delayed_before_applied_persist.persisted.is_none());

    let mut additional_world = World::baseline(false);
    additional_world.execute_plan(&acquire_with_ttl(false, 45_000).plan);
    let additional = additional_world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(0),
            5_000,
            true,
            1,
        )
        .expect("additional acquire");
    assert_eq!(
        additional.plan.effects[0],
        Effect::CheckDeadline(now(5_000))
    );
    additional_world.monotonic_now = now(8_000);
    assert_eq!(
        additional_world.try_execute(&additional.plan.effects[0]),
        Err(RuntimeEffectError::DeadlineExpired)
    );
    assert_eq!(
        additional_world
            .persisted
            .as_ref()
            .expect("old state retained")
            .leases
            .len(),
        1
    );

    let mut renew_world = applied_world(false);
    let renew = renew_world
        .engine()
        .renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            5_000,
        )
        .expect("one millisecond extension");
    assert_eq!(renew.effects[0], Effect::CheckDeadline(now(5_000)));
    renew_world.monotonic_now = now(8_000);
    assert_eq!(
        renew_world.try_execute(&renew.effects[0]),
        Err(RuntimeEffectError::DeadlineExpired)
    );
    assert_eq!(
        renew_world.persisted.as_ref().expect("old lease").leases[0].deadline,
        now(5_000)
    );
    renew_world.converge(8_000);
    assert!(!renew_world.sleep_disabled);
    assert!(renew_world.persisted.is_none());

    let existing = Engine::from_state(
        config(),
        acquire_with_ttl(false, 5_000)
            .plan
            .state
            .expect("applied state"),
    )
    .acquire(
        &owner(),
        lease_id(LEASE_B),
        "prod",
        "generation-a",
        now(0),
        45_000,
        true,
        1,
    )
    .expect("idempotent retry");
    assert_eq!(
        existing.plan.effects,
        vec![Effect::CheckRemainingTtl(now(5_000), MIN_TTL_MS)]
    );
    let mut delayed_reply = World::baseline(false);
    delayed_reply.monotonic_now = now(8_000);
    assert_eq!(
        delayed_reply.try_execute(&existing.plan.effects[0]),
        Err(RuntimeEffectError::RemainingTtlTooShort)
    );
}

#[test]
fn idempotent_retry_with_one_millisecond_remaining_never_returns_stale_ttl() {
    let mut world = World::baseline(false);
    world.execute_plan(&acquire_with_ttl(false, 5_000).plan);
    let retry = world
        .engine()
        .acquire(
            &owner(),
            lease_id(LEASE_B),
            "prod",
            "generation-a",
            now(0),
            45_000,
            true,
            1,
        )
        .expect("retry is initially admissible");
    assert_eq!(
        retry.outcome,
        AcquireOutcome::Existing {
            lease_id: lease_id(LEASE_A),
            deadline: now(5_000),
        }
    );

    world.monotonic_now = now(4_999);
    assert_eq!(
        world.execute_response_plan(&retry.plan),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.persisted.is_some());
    assert!(world.sleep_disabled);
}

#[test]
fn first_acquire_fsync_past_new_deadline_recovers_before_success() {
    let acquire = first_acquire(false);
    let mut world = World::baseline(false);

    assert_eq!(
        world.execute_plan_with_slow_persist(&acquire, 5, now(8_000)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(!world.sleep_disabled);
    assert!(world.persisted.is_none());
}

#[test]
fn additional_acquire_fsync_past_new_deadline_recovers_before_success() {
    let mut world = World::baseline(false);
    world.execute_plan(&acquire_with_ttl(false, 45_000).plan);
    let additional = world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(0),
            5_000,
            true,
            1,
        )
        .expect("additional acquire");

    assert_eq!(
        world.execute_plan_with_slow_persist(&additional.plan, 1, now(8_000)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.sleep_disabled);
    let recovered = world.persisted.expect("live first lease is retained");
    assert_eq!(recovered.leases.len(), 1);
    assert_eq!(recovered.leases[0].lease_id, lease_id(LEASE_A));
}

#[test]
fn renew_fsync_past_new_deadline_recovers_before_success() {
    let mut world = applied_world(false);
    let renew = world
        .engine()
        .renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            5_000,
        )
        .expect("renew");

    assert_eq!(
        world.execute_plan_with_slow_persist(&renew, 1, now(8_000)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(!world.sleep_disabled);
    assert!(world.persisted.is_none());
}

#[test]
fn first_acquire_success_uses_one_final_runtime_ttl_sample() {
    let acquire = acquire_with_ttl(false, 45_000);
    assert_single_final_ttl_sample(&acquire.plan, now(45_000));

    let mut world = World::baseline(false);
    world.monotonic_now = now(1_000);
    assert_eq!(
        world
            .execute_response_plan(&acquire.plan)
            .expect("runtime TTL sample"),
        44_000
    );
}

#[test]
fn additional_acquire_success_uses_one_final_runtime_ttl_sample() {
    let mut world = World::baseline(false);
    world.execute_plan(&acquire_with_ttl(false, 45_000).plan);
    let additional = world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(1_000),
            45_000,
            true,
            1,
        )
        .expect("additional acquire");

    assert_single_final_ttl_sample(&additional.plan, now(46_000));
    world.monotonic_now = now(2_000);
    assert_eq!(
        world
            .execute_response_plan(&additional.plan)
            .expect("runtime TTL sample"),
        44_000
    );
}

#[test]
fn renew_success_uses_one_final_runtime_ttl_sample() {
    let world = applied_world(false);
    let renew = world
        .engine()
        .renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            45_000,
        )
        .expect("renew");

    assert_single_final_ttl_sample(&renew, now(45_001));
    let mut runtime = world;
    runtime.monotonic_now = now(1_001);
    assert_eq!(
        runtime
            .execute_response_plan(&renew)
            .expect("runtime TTL sample"),
        44_000
    );
}

#[test]
fn first_acquire_at_deadline_minus_one_millisecond_cannot_report_success() {
    let acquire = first_acquire(false);
    let mut world = World::baseline(false);

    assert_eq!(
        world.execute_plan_with_slow_persist(&acquire, 5, now(4_999)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.sleep_disabled);
    let retained = world.persisted.expect("applied recovery evidence retained");
    assert_eq!(retained.leases[0].deadline, now(5_000));
}

#[test]
fn guard_failure_with_failed_reconciliation_requires_recovery_and_retains_evidence() {
    let acquire = first_acquire(false);
    let mut world = World::baseline(false);
    world.reconciliation_failure_injected = true;

    let result = world.execute_plan_with_slow_persist(&acquire, 5, now(4_999));
    assert_eq!(
        result,
        Err(RuntimeGuardFailureOutcome::RecoveryRequired(
            RuntimeGuardError::RemainingTtlTooShort
        ))
    );
    assert_eq!(
        result
            .expect_err("guard failure cannot report success")
            .protocol_error_code(),
        ErrorCode::RecoveryRequired
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.sleep_disabled);
    let retained = world
        .persisted
        .expect("failed reconciliation retains applied evidence");
    assert_eq!(retained.phase, MutationPhase::Applied);
    assert_eq!(retained.leases[0].deadline, now(5_000));
}

#[test]
fn additional_acquire_at_deadline_minus_one_millisecond_cannot_report_success() {
    let mut world = World::baseline(false);
    world.execute_plan(&acquire_with_ttl(false, 45_000).plan);
    let additional = world
        .engine()
        .acquire(
            &other_owner(),
            lease_id(LEASE_B),
            "work",
            "generation-b",
            now(0),
            5_000,
            true,
            1,
        )
        .expect("additional acquire");

    assert_eq!(
        world.execute_plan_with_slow_persist(&additional.plan, 1, now(4_999)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.sleep_disabled);
    let retained = world.persisted.expect("both live leases retained");
    assert_eq!(retained.leases.len(), 2);
    assert_eq!(retained.leases[1].deadline, now(5_000));
}

#[test]
fn renew_at_deadline_minus_one_millisecond_cannot_report_success() {
    let mut world = applied_world(false);
    let renew = world
        .engine()
        .renew(
            &owner(),
            &lease_id(LEASE_A),
            "generation-a",
            BOOT_ID,
            now(1),
            5_000,
        )
        .expect("renew");

    assert_eq!(
        world.execute_plan_with_slow_persist(&renew, 1, now(5_000)),
        Err(RuntimeGuardFailureOutcome::Recovered(
            RuntimeEffectError::RemainingTtlTooShort
        ))
    );
    assert_eq!(world.immediate_reconciliations, 1);
    assert!(world.sleep_disabled);
    let retained = world.persisted.expect("renewed evidence retained");
    assert_eq!(retained.leases[0].deadline, now(5_001));
}

#[test]
fn created_outcome_uses_relative_deadline_at_nonzero_monotonic_time() {
    let result = Engine::empty(config())
        .acquire(
            &owner(),
            lease_id(LEASE_A),
            "prod",
            "generation-a",
            now(10_000),
            5_000,
            false,
            1,
        )
        .expect("nonzero monotonic acquire");

    assert_eq!(
        result.outcome,
        AcquireOutcome::Created {
            lease_id: lease_id(LEASE_A),
            deadline: now(15_000),
        }
    );
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
