use std::fmt;

use jarvis_power_core::engine::{
    AcquireOutcome, Effect, Engine, EngineConfig, EngineError, ProcessState, RuntimeGuardError,
    RuntimeGuardFailureOutcome,
};
use jarvis_power_core::state::{HelperState, LeaseId, MonotonicTime, MutationPhase, Principal};

use crate::pmset::{PmsetBackend, PmsetError};
use crate::root_store::{LockedRootStore, RootStore, StoreError};
use crate::HelperEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessInspectionError {
    Unverifiable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RandomError {
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinatorError {
    Store(StoreError),
    Pmset(PmsetError),
    Engine(EngineError),
    Process(ProcessInspectionError),
    Random(RandomError),
    ClockUnavailable,
    VerificationFailed { expected: bool, actual: bool },
    RuntimeGuard(RuntimeGuardFailureOutcome),
    RecoveryRequired,
    Internal,
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Store(_) => "power state storage failed",
            Self::Pmset(_) => "power backend failed",
            Self::Engine(_) => "power lease transition was rejected",
            Self::Process(_) => "process identity is unverifiable",
            Self::Random(_) => "secure random generation failed",
            Self::ClockUnavailable => "monotonic clock is unavailable",
            Self::VerificationFailed { .. } => "power read-back did not match",
            Self::RuntimeGuard(_) => "power lease expired during the transaction",
            Self::RecoveryRequired => "power recovery is required",
            Self::Internal => "power helper internal invariant failed",
        })
    }
}

impl std::error::Error for CoordinatorError {}

impl From<StoreError> for CoordinatorError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<PmsetError> for CoordinatorError {
    fn from(error: PmsetError) -> Self {
        Self::Pmset(error)
    }
}

impl From<EngineError> for CoordinatorError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<RandomError> for CoordinatorError {
    fn from(error: RandomError) -> Self {
        Self::Random(error)
    }
}

pub trait MonotonicClock: Send {
    fn now(&mut self) -> Result<MonotonicTime, CoordinatorError>;
}

pub trait ProcessInspector: Send {
    fn inspect(&mut self, principal: &Principal) -> Result<ProcessState, ProcessInspectionError>;
}

pub trait RandomSource: Send {
    fn next_lease_id(&mut self) -> Result<LeaseId, RandomError>;
    fn next_mutation_generation(&mut self) -> Result<u64, RandomError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRandom;

impl RandomSource for SystemRandom {
    fn next_lease_id(&mut self) -> Result<LeaseId, RandomError> {
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random).map_err(|_| RandomError::Unavailable)?;
        let mut lease = String::with_capacity(32);
        for byte in random {
            use std::fmt::Write as _;
            write!(&mut lease, "{byte:02x}").map_err(|_| RandomError::Unavailable)?;
        }
        LeaseId::parse(lease).map_err(|_| RandomError::Unavailable)
    }

    fn next_mutation_generation(&mut self) -> Result<u64, RandomError> {
        let mut random = [0_u8; 8];
        getrandom::getrandom(&mut random).map_err(|_| RandomError::Unavailable)?;
        let mut value = u64::from_ne_bytes(random);
        if value == 0 || value == u64::MAX {
            value ^= 0x5a5a_a5a5_5a5a_a5a5;
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseGrant {
    pub lease_id: LeaseId,
    pub granted_ttl_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoordinatorStatus {
    pub active_leases: u32,
    pub mutation_active: bool,
    pub recovery_required: bool,
    pub phase: Option<MutationPhase>,
}

pub(crate) struct Coordinator<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    store: RootStore,
    backend: B,
    clock: C,
    processes: P,
    random: R,
    service_version: u64,
    minimum_client_build: u64,
}

impl<B, C, P, R> Coordinator<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: RootStore,
        backend: B,
        clock: C,
        processes: P,
        random: R,
        service_version: u64,
        minimum_client_build: u64,
    ) -> Self {
        Self {
            store,
            backend,
            clock,
            processes,
            random,
            service_version,
            minimum_client_build,
        }
    }

    pub(crate) fn store(&self) -> &RootStore {
        &self.store
    }

    pub(crate) fn acquire(
        &mut self,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        let store = self.store.clone();
        let mut transaction = store.lock()?;
        let result = self.acquire_locked(
            &mut transaction,
            principal,
            profile,
            owner_generation,
            ttl_ms,
        );
        if result.is_ok() {
            store.events().record(HelperEvent::ReplyReady);
        }
        result
    }

    fn acquire_locked(
        &mut self,
        transaction: &mut LockedRootStore<'_>,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        let state = transaction.load()?;
        let boot_id = self.backend.boot_id()?;
        let config = self.engine_config(&boot_id)?;
        let now = self.clock.now()?;
        let observed_disabled = self.backend.read_disabled()?;
        self.store
            .events()
            .record(HelperEvent::PowerRead(observed_disabled));
        let lease_id = self.random.next_lease_id()?;
        let mutation_generation = match &state {
            Some(state) => state.mutation_generation,
            None => self.random.next_mutation_generation()?,
        };
        let engine = engine_from_state(config, state);
        let result = engine.acquire(
            principal,
            lease_id,
            profile,
            owner_generation,
            now,
            ttl_ms,
            observed_disabled,
            mutation_generation,
        )?;
        let authoritative_lease = match &result.outcome {
            AcquireOutcome::Created { lease_id, .. }
            | AcquireOutcome::Existing { lease_id, .. } => lease_id.clone(),
        };
        let granted_ttl_ms = self
            .execute_request_plan(transaction, result.plan.effects)?
            .granted_ttl_ms
            .ok_or(CoordinatorError::Internal)?;
        Ok(LeaseGrant {
            lease_id: authoritative_lease,
            granted_ttl_ms,
        })
    }

    pub(crate) fn renew(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        let store = self.store.clone();
        let mut transaction = store.lock()?;
        let state = transaction.load()?;
        let boot_id = self.backend.boot_id()?;
        let config = self.engine_config(&boot_id)?;
        let now = self.clock.now()?;
        let engine = engine_from_state(config, state);
        let plan = engine.renew(principal, lease_id, owner_generation, &boot_id, now, ttl_ms)?;
        let granted_ttl_ms = self
            .execute_request_plan(&mut transaction, plan.effects)?
            .granted_ttl_ms
            .ok_or(CoordinatorError::Internal)?;
        store.events().record(HelperEvent::ReplyReady);
        Ok(LeaseGrant {
            lease_id: lease_id.clone(),
            granted_ttl_ms,
        })
    }

    pub(crate) fn release(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
    ) -> Result<(), CoordinatorError> {
        let store = self.store.clone();
        let mut transaction = store.lock()?;
        let state = transaction.load()?;
        let boot_id = self.backend.boot_id()?;
        let config = self.engine_config(&boot_id)?;
        let engine = engine_from_state(config, state);
        let plan = engine.release(principal, lease_id, owner_generation, &boot_id)?;
        let _ = self.execute_request_plan(&mut transaction, plan.effects)?;
        store.events().record(HelperEvent::ReplyReady);
        Ok(())
    }

    pub(crate) fn status(&mut self) -> Result<CoordinatorStatus, CoordinatorError> {
        let store = self.store.clone();
        let transaction = store.lock()?;
        let state = transaction.load()?;
        let status = status_from_state(state.as_ref());
        store.events().record(HelperEvent::ReplyReady);
        Ok(status)
    }

    pub(crate) fn reconcile_internal(&mut self) -> Result<(), CoordinatorError> {
        let store = self.store.clone();
        let mut transaction = store.lock()?;
        self.reconcile_locked(&mut transaction)
    }

    fn reconcile_locked(
        &mut self,
        transaction: &mut LockedRootStore<'_>,
    ) -> Result<(), CoordinatorError> {
        let state = transaction.load()?;
        let boot_id = self.backend.boot_id()?;
        let config = self.engine_config(&boot_id)?;
        let now = self.clock.now()?;
        let engine = engine_from_state(config, state);
        let mut process_error = None;
        let plan = engine.reconcile(&boot_id, now, |principal| {
            match self.processes.inspect(principal) {
                Ok(ProcessState::Unverifiable) => {
                    process_error = Some(ProcessInspectionError::Unverifiable);
                    ProcessState::Unverifiable
                }
                Ok(state) => state,
                Err(error) => {
                    process_error = Some(error);
                    ProcessState::Unverifiable
                }
            }
        });
        if let Some(error) = process_error {
            return Err(CoordinatorError::Process(error));
        }
        let plan = plan?;
        self.execute_effects(transaction, plan.effects)
            .map(|_| ())
            .map_err(|failure| failure.error)
    }

    fn execute_request_plan(
        &mut self,
        transaction: &mut LockedRootStore<'_>,
        effects: Vec<Effect>,
    ) -> Result<EffectExecution, CoordinatorError> {
        match self.execute_effects(transaction, effects) {
            Ok(execution) => Ok(execution),
            Err(failure) => match failure.runtime_guard {
                Some(runtime_guard) => {
                    let outcome = if self.reconcile_locked(transaction).is_ok() {
                        RuntimeGuardFailureOutcome::Recovered(runtime_guard)
                    } else {
                        RuntimeGuardFailureOutcome::RecoveryRequired(runtime_guard)
                    };
                    Err(CoordinatorError::RuntimeGuard(outcome))
                }
                None if failure.requires_reconciliation => {
                    if self.reconcile_locked(transaction).is_err() {
                        Err(CoordinatorError::RecoveryRequired)
                    } else {
                        Err(failure.error)
                    }
                }
                None => Err(failure.error),
            },
        }
    }

    fn execute_effects(
        &mut self,
        transaction: &mut LockedRootStore<'_>,
        effects: Vec<Effect>,
    ) -> Result<EffectExecution, EffectFailure> {
        let mut granted_ttl_ms = None;
        let mut durable_state_exists = false;
        for effect in effects {
            let result = match effect {
                Effect::PersistState(state) => {
                    let result = transaction.persist(&state);
                    if result.is_ok() {
                        durable_state_exists = true;
                    }
                    result.map_err(CoordinatorError::Store)
                }
                Effect::CheckDeadline(deadline) => match self.clock.now() {
                    Ok(now) if now < deadline => Ok(()),
                    Ok(_) => {
                        return Err(EffectFailure::runtime(
                            RuntimeGuardError::DeadlineExpired,
                            durable_state_exists,
                        ))
                    }
                    Err(error) => Err(error),
                },
                Effect::CheckRemainingTtl(deadline, minimum) => match self.clock.now() {
                    Ok(now) => {
                        let remaining = deadline.as_millis().saturating_sub(now.as_millis());
                        if remaining < minimum {
                            return Err(EffectFailure::runtime(
                                RuntimeGuardError::RemainingTtlTooShort,
                                durable_state_exists,
                            ));
                        }
                        granted_ttl_ms = Some(remaining);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                Effect::CompareAndSetDisabled(value) => {
                    let result = self.backend.set_disabled(value);
                    if result.is_ok() {
                        self.store.events().record(HelperEvent::PowerWrite(value));
                    }
                    result.map_err(CoordinatorError::Pmset)
                }
                Effect::VerifyDisabled(expected) => match self.backend.read_disabled() {
                    Ok(actual) => {
                        self.store.events().record(HelperEvent::PowerRead(actual));
                        if actual == expected {
                            Ok(())
                        } else {
                            Err(CoordinatorError::VerificationFailed { expected, actual })
                        }
                    }
                    Err(error) => Err(CoordinatorError::Pmset(error)),
                },
                Effect::ClearState => transaction.clear().map_err(CoordinatorError::Store),
            };
            if let Err(error) = result {
                let durability_unknown =
                    error == CoordinatorError::Store(StoreError::DurabilityUnknown);
                return Err(EffectFailure {
                    error,
                    runtime_guard: None,
                    requires_reconciliation: durable_state_exists || durability_unknown,
                });
            }
        }
        Ok(EffectExecution { granted_ttl_ms })
    }

    fn engine_config(&self, boot_id: &str) -> Result<EngineConfig, CoordinatorError> {
        EngineConfig::new(self.service_version, self.minimum_client_build, boot_id)
            .map_err(|error| CoordinatorError::Engine(EngineError::CorruptState(error)))
    }
}

struct EffectExecution {
    granted_ttl_ms: Option<u64>,
}

struct EffectFailure {
    error: CoordinatorError,
    runtime_guard: Option<RuntimeGuardError>,
    requires_reconciliation: bool,
}

impl EffectFailure {
    fn runtime(error: RuntimeGuardError, requires_reconciliation: bool) -> Self {
        Self {
            error: CoordinatorError::Internal,
            runtime_guard: Some(error),
            requires_reconciliation,
        }
    }
}

fn engine_from_state(config: EngineConfig, state: Option<HelperState>) -> Engine {
    match state {
        Some(state) => Engine::from_state(config, state),
        None => Engine::empty(config),
    }
}

fn status_from_state(state: Option<&HelperState>) -> CoordinatorStatus {
    match state {
        Some(state) => CoordinatorStatus {
            active_leases: u32::try_from(state.leases.len()).unwrap_or(u32::MAX),
            mutation_active: state.applied && state.did_mutate,
            recovery_required: state.phase != MutationPhase::Applied,
            phase: Some(state.phase),
        },
        None => CoordinatorStatus {
            active_leases: 0,
            mutation_active: false,
            recovery_required: false,
            phase: None,
        },
    }
}
