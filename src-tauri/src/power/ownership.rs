use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub profile: String,
    pub pid: u32,
    pub process_identity: String,
    pub owner_generation: String,
    pub acquired_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipState {
    pub schema_version: u32,
    pub boot_id: String,
    pub baseline: bool,
    pub applied: bool,
    pub did_mutate: bool,
    pub generation: u64,
    pub leases: Vec<Lease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseDecision {
    KeepApplied,
    Restore(bool),
    ClearWithoutMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecision {
    KeepApplied,
    Restore(bool),
    ClearWithoutMutation,
}

impl OwnershipState {
    pub fn new(baseline: bool, boot_id: impl Into<String>, generation: u64) -> Self {
        Self {
            schema_version: 1,
            boot_id: boot_id.into(),
            baseline,
            applied: true,
            did_mutate: !baseline,
            generation,
            leases: Vec::new(),
        }
    }

    pub fn acquire(&mut self, lease: Lease) {
        self.leases.retain(|existing| {
            existing.profile != lease.profile || existing.owner_generation != lease.owner_generation
        });
        self.leases.push(lease);
    }

    pub fn release(&mut self, profile: &str, owner_generation: &str) -> ReleaseDecision {
        self.leases
            .retain(|lease| lease.profile != profile || lease.owner_generation != owner_generation);

        if !self.leases.is_empty() {
            ReleaseDecision::KeepApplied
        } else if self.did_mutate {
            ReleaseDecision::Restore(self.baseline)
        } else {
            ReleaseDecision::ClearWithoutMutation
        }
    }

    pub fn recover(&mut self, mut is_alive: impl FnMut(&Lease) -> bool) -> RecoveryDecision {
        self.leases.retain(|lease| is_alive(lease));

        if !self.leases.is_empty() {
            RecoveryDecision::KeepApplied
        } else if self.did_mutate {
            RecoveryDecision::Restore(self.baseline)
        } else {
            RecoveryDecision::ClearWithoutMutation
        }
    }
}

#[cfg(test)]
impl Lease {
    fn test(profile: &str, pid: u32, owner_generation: &str) -> Self {
        Self {
            profile: profile.into(),
            pid,
            process_identity: format!("pid-{pid}"),
            owner_generation: owner_generation.into(),
            acquired_at_ms: 0,
            expires_at_ms: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_lease_is_recorded_on_new_applied_state() {
        let mut state = OwnershipState::new(false, "boot-a", 7);
        state.acquire(Lease::test("prod", 101, "a"));

        assert_eq!(state.schema_version, 1);
        assert_eq!(state.boot_id, "boot-a");
        assert!(!state.baseline);
        assert!(state.applied);
        assert!(state.did_mutate);
        assert_eq!(state.generation, 7);
        assert_eq!(state.leases, vec![Lease::test("prod", 101, "a")]);
    }

    #[test]
    fn acquire_replaces_only_an_identical_owner() {
        let mut state = OwnershipState::new(false, "boot-a", 7);
        state.acquire(Lease::test("prod", 101, "a"));
        state.acquire(Lease::test("prod", 202, "b"));
        state.acquire(Lease::test("prod", 303, "a"));

        assert_eq!(
            state.leases,
            vec![Lease::test("prod", 202, "b"), Lease::test("prod", 303, "a")]
        );
    }

    #[test]
    fn last_mutating_lease_restores_original_baseline() {
        let mut state = OwnershipState::new(false, "boot-a", 7);
        state.acquire(Lease::test("prod", 101, "a"));
        state.acquire(Lease::test("dev", 202, "b"));
        assert_eq!(state.release("prod", "a"), ReleaseDecision::KeepApplied);
        assert_eq!(state.release("dev", "b"), ReleaseDecision::Restore(false));
    }

    #[test]
    fn baseline_already_on_is_never_changed() {
        let mut state = OwnershipState::new(true, "boot-a", 7);
        state.acquire(Lease::test("prod", 101, "a"));
        assert_eq!(
            state.release("prod", "a"),
            ReleaseDecision::ClearWithoutMutation
        );
    }

    #[test]
    fn dead_leases_are_removed_before_recovery() {
        let mut state = OwnershipState::new(false, "boot-a", 7);
        state.acquire(Lease::test("prod", 101, "dead"));
        assert_eq!(
            state.recover(|lease| lease.pid != 101),
            RecoveryDecision::Restore(false)
        );
    }
}
