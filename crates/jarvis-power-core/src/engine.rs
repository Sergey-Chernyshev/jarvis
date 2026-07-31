use crate::protocol::{MAX_TTL_MS, MIN_TTL_MS};
use crate::state::{
    valid_identifier, HelperState, Lease, LeaseId, MonotonicTime, MutationPhase, Principal,
    StateError, MAX_ACTIVE_LEASES, STATE_SCHEMA_VERSION,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    service_version: u64,
    minimum_client_build: u64,
    boot_id: String,
}

impl EngineConfig {
    pub fn new(
        service_version: u64,
        minimum_client_build: u64,
        boot_id: impl Into<String>,
    ) -> Result<Self, StateError> {
        let config = Self {
            service_version,
            minimum_client_build,
            boot_id: boot_id.into(),
        };
        if config.service_version == 0 {
            return Err(StateError::InvalidServiceVersion);
        }
        if config.minimum_client_build == 0 {
            return Err(StateError::InvalidMinimumClientBuild);
        }
        if !valid_identifier(&config.boot_id) {
            return Err(StateError::InvalidBootId);
        }
        Ok(config)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// An ordered side effect that the privileged coordinator executes serially.
///
/// Execution must stop on the first failed effect. In particular,
/// `ClearState` is emitted only after a matching `VerifyDisabled`.
pub enum Effect {
    PersistState(HelperState),
    /// Abort the remaining plan when the helper's current monotonic time is
    /// greater than or equal to this deadline. For an idempotent
    /// [`AcquireOutcome::Existing`] response, this is the only and final
    /// permitted blocking action; the coordinator must reply immediately
    /// after it succeeds.
    CheckDeadline(MonotonicTime),
    CompareAndSetDisabled(bool),
    VerifyDisabled(bool),
    ClearState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A deterministic transition plan.
///
/// `state` is the projected state after every effect succeeds; it is provided
/// for inspection and response construction, not as a substitute for executing
/// the ordered persistence effects.
pub struct Plan {
    pub state: Option<HelperState>,
    pub effects: Vec<Effect>,
}

impl Plan {
    fn unchanged(state: Option<HelperState>) -> Self {
        Self {
            state,
            effects: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcquireOutcome {
    Created {
        lease_id: LeaseId,
        deadline: MonotonicTime,
        granted_ttl_ms: u64,
    },
    Existing {
        /// The authoritative ID from persisted state, which can differ from
        /// the retry's proposed ID after a lost response.
        lease_id: LeaseId,
        deadline: MonotonicTime,
        granted_ttl_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcquireResult {
    pub plan: Plan,
    /// The authoritative lease identity for the response. A coordinator must
    /// never answer with its proposed lease id instead of this outcome.
    pub outcome: AcquireOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    AliveExact,
    Dead,
    Mismatch,
    Unverifiable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Finite protocol failures for the unprivileged coordinator.
///
/// The Task 3 adapter should map `LeaseNearExpiry` and `Expired` to its
/// lease-expired response; `PolicyMismatch` and `RecoveryRequired` to its
/// recovery-required response; and `LeaseLimitReached` and `NotExtended` to
/// its conflict response. A failed runtime [`Effect::CheckDeadline`] must be
/// surfaced as lease-expired or recovery-required, never as a successful
/// acquire or renewal.
pub enum EngineError {
    CorruptState(StateError),
    InvalidIdentifier,
    InvalidTtl,
    DeadlineOverflow,
    InvalidMutationGeneration,
    MutationGenerationMismatch,
    ClientBuildTooOld,
    PolicyMismatch,
    DuplicateLease,
    LeaseLimitReached,
    LeaseNearExpiry,
    LeaseNotFound,
    Expired,
    NotExtended,
    PrincipalMismatch,
    OwnerGenerationMismatch,
    BootMismatch,
    ProcessUnverifiable,
    RecoveryRequired,
    ObservedStateMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Engine {
    config: EngineConfig,
    state: Option<HelperState>,
}

impl Engine {
    pub fn empty(config: EngineConfig) -> Self {
        Self {
            config,
            state: None,
        }
    }

    pub fn from_state(config: EngineConfig, state: HelperState) -> Self {
        Self {
            config,
            state: Some(state),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn acquire(
        &self,
        principal: &Principal,
        lease_id: LeaseId,
        profile: &str,
        owner_generation: &str,
        now: MonotonicTime,
        ttl_ms: u64,
        observed_disabled: bool,
        mutation_generation: u64,
    ) -> Result<AcquireResult, EngineError> {
        principal.validate().map_err(EngineError::CorruptState)?;
        if principal.signed_build() < self.config.minimum_client_build {
            return Err(EngineError::ClientBuildTooOld);
        }
        if !valid_identifier(profile) || !valid_identifier(owner_generation) {
            return Err(EngineError::InvalidIdentifier);
        }
        validate_ttl(ttl_ms)?;
        let deadline = now
            .checked_add_millis(ttl_ms)
            .ok_or(EngineError::DeadlineOverflow)?;
        if mutation_generation == 0 || mutation_generation == u64::MAX {
            return Err(EngineError::InvalidMutationGeneration);
        }

        let lease = Lease {
            lease_id,
            profile: profile.to_owned(),
            owner_generation: owner_generation.to_owned(),
            principal: principal.clone(),
            deadline,
        };

        match &self.state {
            None => self.acquire_first(lease, observed_disabled, mutation_generation, ttl_ms),
            Some(_) => {
                let mut state = self.checked_state_for_boot(&self.config.boot_id)?.clone();
                self.ensure_current_policy(&state)?;
                validate_deadline_horizon(&state, now)?;
                if state.phase != MutationPhase::Applied {
                    return Err(EngineError::RecoveryRequired);
                }
                if state.mutation_generation != mutation_generation {
                    return Err(EngineError::MutationGenerationMismatch);
                }
                if !observed_disabled {
                    return Err(EngineError::ObservedStateMismatch);
                }

                if state.leases.iter().any(|existing| existing.deadline <= now) {
                    return Err(EngineError::RecoveryRequired);
                }

                if let Some(existing) = state.leases.iter().find(|existing| {
                    existing.principal == *principal
                        && existing.profile == profile
                        && existing.owner_generation == owner_generation
                }) {
                    let remaining_ttl_ms = existing
                        .deadline
                        .as_millis()
                        .saturating_sub(now.as_millis());
                    if remaining_ttl_ms < MIN_TTL_MS {
                        return Err(EngineError::LeaseNearExpiry);
                    }
                    let existing_lease_id = existing.lease_id.clone();
                    let existing_deadline = existing.deadline;
                    return Ok(AcquireResult {
                        plan: Plan {
                            state: Some(state),
                            effects: vec![Effect::CheckDeadline(existing_deadline)],
                        },
                        outcome: AcquireOutcome::Existing {
                            lease_id: existing_lease_id,
                            deadline: existing_deadline,
                            granted_ttl_ms: remaining_ttl_ms,
                        },
                    });
                }

                if state.leases.len() >= MAX_ACTIVE_LEASES {
                    return Err(EngineError::LeaseLimitReached);
                }
                if state
                    .leases
                    .iter()
                    .any(|existing| existing.lease_id == lease.lease_id)
                {
                    return Err(EngineError::DuplicateLease);
                }
                let outcome = AcquireOutcome::Created {
                    lease_id: lease.lease_id.clone(),
                    deadline: lease.deadline,
                    granted_ttl_ms: ttl_ms,
                };
                state.leases.push(lease);
                state.validate().map_err(EngineError::CorruptState)?;
                Ok(AcquireResult {
                    plan: Plan {
                        state: Some(state.clone()),
                        effects: vec![Effect::CheckDeadline(deadline), Effect::PersistState(state)],
                    },
                    outcome,
                })
            }
        }
    }

    pub fn renew(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        current_boot_id: &str,
        now: MonotonicTime,
        ttl_ms: u64,
    ) -> Result<Plan, EngineError> {
        principal.validate().map_err(EngineError::CorruptState)?;
        if principal.signed_build() < self.config.minimum_client_build {
            return Err(EngineError::ClientBuildTooOld);
        }
        validate_ttl(ttl_ms)?;
        let deadline = now
            .checked_add_millis(ttl_ms)
            .ok_or(EngineError::DeadlineOverflow)?;
        let mut state = self.checked_state_for_boot(current_boot_id)?.clone();
        self.ensure_current_policy(&state)?;
        validate_deadline_horizon(&state, now)?;
        if state.phase != MutationPhase::Applied {
            return Err(EngineError::RecoveryRequired);
        }
        let lease = state
            .leases
            .iter_mut()
            .find(|lease| &lease.lease_id == lease_id)
            .ok_or(EngineError::LeaseNotFound)?;
        validate_binding(lease, principal, owner_generation)?;
        if now >= lease.deadline {
            return Err(EngineError::Expired);
        }
        if deadline <= lease.deadline {
            return Err(EngineError::NotExtended);
        }
        let old_deadline = lease.deadline;
        lease.deadline = deadline;
        state.validate().map_err(EngineError::CorruptState)?;
        Ok(Plan {
            state: Some(state.clone()),
            effects: vec![
                Effect::CheckDeadline(old_deadline),
                Effect::PersistState(state),
            ],
        })
    }

    pub fn release(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        current_boot_id: &str,
    ) -> Result<Plan, EngineError> {
        let mut state = self.checked_state_for_boot(current_boot_id)?.clone();
        if state.phase != MutationPhase::Applied {
            return Err(EngineError::RecoveryRequired);
        }
        let position = state
            .leases
            .iter()
            .position(|lease| &lease.lease_id == lease_id)
            .ok_or(EngineError::LeaseNotFound)?;
        validate_binding(&state.leases[position], principal, owner_generation)?;
        state.leases.remove(position);
        if state.leases.is_empty() || !self.current_policy_matches(&state) {
            Ok(restore_plan(state, true))
        } else {
            state.validate().map_err(EngineError::CorruptState)?;
            Ok(Plan {
                state: Some(state.clone()),
                effects: vec![Effect::PersistState(state)],
            })
        }
    }

    pub fn reconcile(
        &self,
        current_boot_id: &str,
        now: MonotonicTime,
        mut process_state: impl FnMut(&Principal) -> ProcessState,
    ) -> Result<Plan, EngineError> {
        if current_boot_id != self.config.boot_id {
            return Err(EngineError::BootMismatch);
        }
        let Some(state) = &self.state else {
            return Ok(Plan::unchanged(None));
        };
        state.validate().map_err(EngineError::CorruptState)?;
        if state.boot_id != current_boot_id {
            return Err(EngineError::BootMismatch);
        }
        if !self.current_policy_matches(state) {
            return Ok(restore_plan(
                state.clone(),
                state.phase != MutationPhase::RestorePending,
            ));
        }
        validate_deadline_horizon(state, now)?;

        match state.phase {
            MutationPhase::Prepared => Ok(restore_plan(state.clone(), true)),
            MutationPhase::RestorePending => Ok(restore_plan(state.clone(), false)),
            MutationPhase::Applied => {
                let mut retained = Vec::with_capacity(state.leases.len());
                for lease in &state.leases {
                    if now >= lease.deadline {
                        continue;
                    }
                    match process_state(&lease.principal) {
                        ProcessState::AliveExact => retained.push(lease.clone()),
                        ProcessState::Dead | ProcessState::Mismatch => {}
                        ProcessState::Unverifiable => {
                            return Err(EngineError::ProcessUnverifiable);
                        }
                    }
                }

                if retained.len() == state.leases.len() {
                    return Ok(Plan::unchanged(Some(state.clone())));
                }

                let mut next = state.clone();
                next.leases = retained;
                if next.leases.is_empty() {
                    Ok(restore_plan(next, true))
                } else {
                    next.validate().map_err(EngineError::CorruptState)?;
                    Ok(Plan {
                        state: Some(next.clone()),
                        effects: vec![Effect::PersistState(next)],
                    })
                }
            }
        }
    }

    fn acquire_first(
        &self,
        lease: Lease,
        baseline: bool,
        mutation_generation: u64,
        granted_ttl_ms: u64,
    ) -> Result<AcquireResult, EngineError> {
        let outcome = AcquireOutcome::Created {
            lease_id: lease.lease_id.clone(),
            deadline: lease.deadline,
            granted_ttl_ms,
        };
        let deadline = lease.deadline;
        let prepared = HelperState {
            schema_version: STATE_SCHEMA_VERSION,
            service_version: self.config.service_version,
            minimum_client_build: self.config.minimum_client_build,
            boot_id: self.config.boot_id.clone(),
            baseline,
            applied: false,
            did_mutate: !baseline,
            mutation_generation,
            phase: MutationPhase::Prepared,
            leases: vec![lease],
        };
        prepared.validate().map_err(EngineError::CorruptState)?;

        let mut applied = prepared.clone();
        applied.applied = true;
        applied.phase = MutationPhase::Applied;
        applied.validate().map_err(EngineError::CorruptState)?;

        let mut effects = vec![
            Effect::PersistState(prepared),
            Effect::CheckDeadline(deadline),
        ];
        if !baseline {
            effects.push(Effect::CompareAndSetDisabled(true));
        }
        effects.push(Effect::VerifyDisabled(true));
        effects.push(Effect::CheckDeadline(deadline));
        effects.push(Effect::PersistState(applied.clone()));

        Ok(AcquireResult {
            plan: Plan {
                state: Some(applied),
                effects,
            },
            outcome,
        })
    }

    fn checked_state_for_boot(&self, current_boot_id: &str) -> Result<&HelperState, EngineError> {
        if current_boot_id != self.config.boot_id {
            return Err(EngineError::BootMismatch);
        }
        let state = self.state.as_ref().ok_or(EngineError::LeaseNotFound)?;
        state.validate().map_err(EngineError::CorruptState)?;
        if state.boot_id != current_boot_id {
            return Err(EngineError::BootMismatch);
        }
        Ok(state)
    }

    fn ensure_current_policy(&self, state: &HelperState) -> Result<(), EngineError> {
        if self.current_policy_matches(state) {
            Ok(())
        } else {
            Err(EngineError::PolicyMismatch)
        }
    }

    fn current_policy_matches(&self, state: &HelperState) -> bool {
        state.service_version == self.config.service_version
            && state.minimum_client_build == self.config.minimum_client_build
    }
}

fn validate_binding(
    lease: &Lease,
    principal: &Principal,
    owner_generation: &str,
) -> Result<(), EngineError> {
    if &lease.principal != principal {
        return Err(EngineError::PrincipalMismatch);
    }
    if lease.owner_generation != owner_generation {
        return Err(EngineError::OwnerGenerationMismatch);
    }
    Ok(())
}

fn validate_ttl(ttl_ms: u64) -> Result<(), EngineError> {
    if (MIN_TTL_MS..=MAX_TTL_MS).contains(&ttl_ms) {
        Ok(())
    } else {
        Err(EngineError::InvalidTtl)
    }
}

fn validate_deadline_horizon(state: &HelperState, now: MonotonicTime) -> Result<(), EngineError> {
    let now_ms = now.as_millis();
    if state.leases.iter().any(|lease| {
        lease.deadline.as_millis() > now_ms && lease.deadline.as_millis() - now_ms > MAX_TTL_MS
    }) {
        Err(EngineError::CorruptState(StateError::InvalidLease))
    } else {
        Ok(())
    }
}

fn restore_plan(state: HelperState, persist_tombstone: bool) -> Plan {
    let mut tombstone = state;
    tombstone.phase = MutationPhase::RestorePending;
    tombstone.leases.clear();

    let mut effects = Vec::with_capacity(4);
    if persist_tombstone {
        effects.push(Effect::PersistState(tombstone.clone()));
    }
    if tombstone.did_mutate {
        effects.push(Effect::CompareAndSetDisabled(tombstone.baseline));
    }
    effects.push(Effect::VerifyDisabled(tombstone.baseline));
    effects.push(Effect::ClearState);

    Plan {
        state: None,
        effects,
    }
}
