use std::marker::PhantomData;
use std::time::Duration;

use jarvis_power_core::engine::ProcessState;
use jarvis_power_core::state::{LeaseId, MonotonicTime, Principal};

use crate::coordinator::{
    Coordinator, CoordinatorError, CoordinatorStatus, LeaseGrant, MonotonicClock,
    ProcessInspectionError, ProcessInspector, RandomSource, SystemRandom,
};
use crate::pmset::{PmsetBackend, SystemPmset};
use crate::root_store::RootStore;
use crate::HelperEvent;

pub const HELPER_SERVICE_VERSION: u64 = 1;
pub const MINIMUM_CLIENT_BUILD: u64 = 1;
pub const WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);

/// A helper instance that has not yet performed synchronous startup recovery.
///
/// Its only transition to [`ReadyRuntime`] runs the same serialized recovery
/// transaction as the later watchdog. No listener permit exists before then.
pub struct StartupRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    coordinator: Coordinator<B, C, P, R>,
}

impl<B, C, P, R> StartupRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    fn from_parts(
        store: RootStore,
        backend: B,
        clock: C,
        processes: P,
        random: R,
        service_version: u64,
        minimum_client_build: u64,
    ) -> Self {
        Self {
            coordinator: Coordinator::new(
                store,
                backend,
                clock,
                processes,
                random,
                service_version,
                minimum_client_build,
            ),
        }
    }

    /// Dependency injection exists only in crate unit-test builds. The
    /// production entry point is [`ProductionStartup::open`].
    #[cfg(test)]
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
        Self::from_parts(
            store,
            backend,
            clock,
            processes,
            random,
            service_version,
            minimum_client_build,
        )
    }

    pub fn reconcile_before_listener(
        mut self,
    ) -> Result<ReadyRuntime<B, C, P, R>, CoordinatorError> {
        let events = self.coordinator.store().events();
        events.record(HelperEvent::StartupRecovery);
        self.coordinator.reconcile_internal()?;
        events.record(HelperEvent::StartupReady);
        Ok(ReadyRuntime {
            coordinator: self.coordinator,
        })
    }
}

/// Runtime state that proves startup recovery completed before listener
/// publication.
pub struct ReadyRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    coordinator: Coordinator<B, C, P, R>,
}

impl<B, C, P, R> ReadyRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    pub fn listener_permit(&self) -> ListenerPermit<'_> {
        ListenerPermit {
            _ready: PhantomData,
        }
    }

    pub fn acquire(
        &mut self,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.coordinator
            .acquire(principal, profile, owner_generation, ttl_ms)
    }

    pub fn renew(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.coordinator
            .renew(principal, lease_id, owner_generation, ttl_ms)
    }

    pub fn release(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
    ) -> Result<(), CoordinatorError> {
        self.coordinator
            .release(principal, lease_id, owner_generation)
    }

    pub fn status(&mut self) -> Result<CoordinatorStatus, CoordinatorError> {
        self.coordinator.status()
    }

    pub fn watchdog(&mut self) -> Watchdog<'_, B, C, P, R> {
        Watchdog {
            coordinator: &mut self.coordinator,
        }
    }
}

/// Opaque proof that startup recovery completed. Transport code may require
/// this value before publishing its listener.
pub struct ListenerPermit<'a> {
    _ready: PhantomData<&'a ()>,
}

pub struct Watchdog<'a, B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    coordinator: &'a mut Coordinator<B, C, P, R>,
}

impl<B, C, P, R> Watchdog<'_, B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    pub const INTERVAL: Duration = WATCHDOG_INTERVAL;

    /// The sole public post-startup autonomous recovery trigger.
    pub fn tick(&mut self) -> Result<(), CoordinatorError> {
        self.coordinator
            .store()
            .events()
            .record(HelperEvent::WatchdogRecovery);
        self.coordinator.reconcile_internal()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMonotonicClock;

impl MonotonicClock for SystemMonotonicClock {
    fn now(&mut self) -> Result<MonotonicTime, CoordinatorError> {
        system_monotonic_millis().map(MonotonicTime::from_millis)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessInspector;

#[cfg(target_os = "macos")]
impl ProcessInspector for SystemProcessInspector {
    fn inspect(&mut self, principal: &Principal) -> Result<ProcessState, ProcessInspectionError> {
        const PROCESS_STATUS_ZOMBIE: u32 = 5;

        let pid = principal.pid();
        // SAFETY: proc_bsdinfo is plain old data and zero is a valid initial
        // buffer for proc_pidinfo.
        let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
        let expected = std::mem::size_of::<libc::proc_bsdinfo>();
        // SAFETY: info points to a buffer of exactly expected bytes. The call
        // is read-only process inspection for the helper-attested PID.
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                i32::try_from(expected).map_err(|_| ProcessInspectionError::Unverifiable)?,
            )
        };
        if received == 0 {
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) | Some(libc::ENOENT) => Ok(ProcessState::Dead),
                _ => Err(ProcessInspectionError::Unverifiable),
            };
        }
        if received < 0 || usize::try_from(received).ok() != Some(expected) {
            return Err(ProcessInspectionError::Unverifiable);
        }
        if info.pbi_status == PROCESS_STATUS_ZOMBIE {
            return Ok(ProcessState::Dead);
        }
        if info.pbi_pid != u32::try_from(pid).map_err(|_| ProcessInspectionError::Unverifiable)?
            || info.pbi_start_tvsec == 0
            || info.pbi_start_tvusec >= 1_000_000
        {
            return Err(ProcessInspectionError::Unverifiable);
        }
        let identity = principal.process_identity();
        if info.pbi_uid != principal.uid()
            || info.pbi_start_tvsec != identity.start_seconds()
            || info.pbi_start_tvusec != u64::from(identity.start_microseconds())
        {
            return Ok(ProcessState::Mismatch);
        }
        Ok(ProcessState::AliveExact)
    }
}

#[cfg(not(target_os = "macos"))]
impl ProcessInspector for SystemProcessInspector {
    fn inspect(&mut self, _principal: &Principal) -> Result<ProcessState, ProcessInspectionError> {
        Err(ProcessInspectionError::Unverifiable)
    }
}

pub struct ProductionStartup {
    inner: StartupRuntime<SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom>,
}

impl ProductionStartup {
    /// Opens only the fixed root-owned state directory and fixed system
    /// adapters. No path, owner, executable, argv, clock, boot id, or process
    /// identity override is accepted.
    pub fn open() -> Result<Self, CoordinatorError> {
        let store = RootStore::open_production()?;
        Ok(Self {
            inner: StartupRuntime::from_parts(
                store,
                SystemPmset,
                SystemMonotonicClock,
                SystemProcessInspector,
                SystemRandom,
                HELPER_SERVICE_VERSION,
                MINIMUM_CLIENT_BUILD,
            ),
        })
    }

    pub fn reconcile_before_listener(self) -> Result<ProductionRuntime, CoordinatorError> {
        self.inner
            .reconcile_before_listener()
            .map(|inner| ProductionRuntime { inner })
    }
}

pub struct ProductionRuntime {
    inner: ReadyRuntime<SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom>,
}

impl ProductionRuntime {
    pub fn listener_permit(&self) -> ListenerPermit<'_> {
        self.inner.listener_permit()
    }

    pub fn acquire(
        &mut self,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.inner
            .acquire(principal, profile, owner_generation, ttl_ms)
    }

    pub fn renew(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.inner
            .renew(principal, lease_id, owner_generation, ttl_ms)
    }

    pub fn release(
        &mut self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
    ) -> Result<(), CoordinatorError> {
        self.inner.release(principal, lease_id, owner_generation)
    }

    pub fn status(&mut self) -> Result<CoordinatorStatus, CoordinatorError> {
        self.inner.status()
    }

    pub fn watchdog(
        &mut self,
    ) -> Watchdog<'_, SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom> {
        self.inner.watchdog()
    }
}

#[cfg(target_os = "macos")]
fn system_monotonic_millis() -> Result<u64, CoordinatorError> {
    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    #[link(name = "System")]
    extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }

    let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: timebase is a valid output pointer and both functions have no
    // caller-controlled memory beyond that fixed structure.
    if unsafe { mach_timebase_info(&mut timebase) } != 0 || timebase.denom == 0 {
        return Err(CoordinatorError::ClockUnavailable);
    }
    // SAFETY: mach_continuous_time has no arguments and returns a clock tick.
    let ticks = unsafe { mach_continuous_time() };
    let nanos = u128::from(ticks)
        .checked_mul(u128::from(timebase.numer))
        .ok_or(CoordinatorError::ClockUnavailable)?
        / u128::from(timebase.denom);
    u64::try_from(nanos / 1_000_000).map_err(|_| CoordinatorError::ClockUnavailable)
}

#[cfg(not(target_os = "macos"))]
fn system_monotonic_millis() -> Result<u64, CoordinatorError> {
    // Non-production portability for CI. ProductionStartup is macOS-only at
    // the packaging boundary.
    // SAFETY: timespec is plain old data and clock_gettime writes its fixed
    // output buffer.
    let mut time = unsafe { std::mem::zeroed::<libc::timespec>() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0
        || time.tv_sec < 0
        || time.tv_nsec < 0
    {
        return Err(CoordinatorError::ClockUnavailable);
    }
    let millis = u64::try_from(time.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|base| base.checked_add(u64::try_from(time.tv_nsec / 1_000_000).ok()?))
        .ok_or(CoordinatorError::ClockUnavailable)?;
    Ok(millis)
}
