use std::fmt;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jarvis_power_core::engine::ProcessState;
use jarvis_power_core::state::{LeaseId, MonotonicTime, Principal};

use crate::coordinator::{
    Coordinator, CoordinatorError, CoordinatorStatus, LeaseGrant, MonotonicClock,
    ProcessInspectionError, ProcessInspector, RandomSource, SystemRandom,
};
use crate::pmset::{PmsetBackend, SystemPmset};
use crate::root_store::RootStore;
use crate::{HelperEvent, HelperEventSink};

pub const HELPER_SERVICE_VERSION: u64 = 1;
pub const MINIMUM_CLIENT_BUILD: u64 = 1;
pub const WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);
pub const WATCHDOG_READY_TIMEOUT: Duration = Duration::from_secs(1);

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
///
/// Recovery alone is deliberately insufficient for serving. Only an armed
/// [`ServingRuntime`] may publish a listener or dispatch requests.
///
/// `StartupRuntime` cannot publish before synchronous recovery:
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     StartupRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Startup = StartupRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn publish_too_early(startup: &Startup) {
///     let _ = startup.listener_permit();
/// }
/// ```
///
/// `ReadyRuntime` independently rejects listener publication and every request
/// or manual-recovery surface:
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn publish_too_early(ready: &Ready) {
///     let _ = ready.listener_permit();
/// }
/// ```
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn acquire_too_early(ready: &mut Ready) {
///     let _ = ready.acquire(todo!(), "prod", "generation", 5_000);
/// }
/// ```
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn renew_too_early(ready: &mut Ready) {
///     let _ = ready.renew(todo!(), todo!(), "generation", 5_000);
/// }
/// ```
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn release_too_early(ready: &mut Ready) {
///     let _ = ready.release(todo!(), todo!(), "generation");
/// }
/// ```
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn status_too_early(ready: &mut Ready) {
///     let _ = ready.status();
/// }
/// ```
///
/// ```compile_fail,E0599
/// use jarvis_power_helper::coordinator::SystemRandom;
/// use jarvis_power_helper::pmset::SystemPmset;
/// use jarvis_power_helper::watchdog::{
///     ReadyRuntime, SystemMonotonicClock, SystemProcessInspector,
/// };
/// type Ready = ReadyRuntime<
///     SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom,
/// >;
/// fn tick_too_early(ready: &mut Ready) {
///     let _ = ready.watchdog().tick();
/// }
/// ```
pub struct ReadyRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    coordinator: Coordinator<B, C, P, R>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeHealth {
    Healthy,
    Unhealthy {
        last_error: CoordinatorError,
        consecutive_failures: u32,
    },
    SchedulerTerminated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerArmError {
    SpawnFailed,
    ReadyTimeout,
}

impl fmt::Display for SchedulerArmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SpawnFailed => "watchdog scheduler failed to start",
            Self::ReadyTimeout => "watchdog scheduler did not become ready",
        })
    }
}

impl std::error::Error for SchedulerArmError {}

pub(crate) type WatchdogTask = Box<dyn FnMut() + Send + 'static>;
pub(crate) type WatchdogTermination = Box<dyn Fn() + Send + 'static>;

pub(crate) trait WatchdogGuard: Send + Sync {}

pub(crate) trait SchedulerFactory {
    fn start(
        self,
        interval: Duration,
        ready_timeout: Duration,
        task: WatchdogTask,
        termination: WatchdogTermination,
    ) -> Result<Box<dyn WatchdogGuard>, SchedulerArmError>;
}

struct RuntimeCore<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    coordinator: Coordinator<B, C, P, R>,
    health: RuntimeHealth,
}

impl<B, C, P, R> RuntimeCore<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    fn watchdog_tick(&mut self) {
        self.coordinator
            .store()
            .events()
            .record(HelperEvent::WatchdogRecovery);
        match self.coordinator.reconcile_internal() {
            Ok(()) => self.health = RuntimeHealth::Healthy,
            Err(error) => {
                let failures = match self.health {
                    RuntimeHealth::Unhealthy {
                        consecutive_failures,
                        ..
                    } => consecutive_failures.saturating_add(1),
                    RuntimeHealth::Healthy | RuntimeHealth::SchedulerTerminated => 1,
                };
                self.health = RuntimeHealth::Unhealthy {
                    last_error: error,
                    consecutive_failures: failures,
                };
            }
        }
    }

    fn require_healthy(&self) -> Result<(), CoordinatorError> {
        if self.health == RuntimeHealth::Healthy {
            Ok(())
        } else {
            Err(CoordinatorError::RecoveryRequired)
        }
    }
}

pub struct ServingRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    core: Arc<Mutex<RuntimeCore<B, C, P, R>>>,
    scheduler: Option<Box<dyn WatchdogGuard>>,
}

impl<B, C, P, R> ReadyRuntime<B, C, P, R>
where
    B: PmsetBackend + 'static,
    C: MonotonicClock + 'static,
    P: ProcessInspector + 'static,
    R: RandomSource + 'static,
{
    fn arm<S>(
        self,
        scheduler: S,
        interval: Duration,
        ready_timeout: Duration,
    ) -> Result<ServingRuntime<B, C, P, R>, SchedulerArmError>
    where
        S: SchedulerFactory,
    {
        let core = Arc::new(Mutex::new(RuntimeCore {
            coordinator: self.coordinator,
            health: RuntimeHealth::Healthy,
        }));
        let tick_core = core.clone();
        let task: WatchdogTask = Box::new(move || {
            lock_runtime_core(&tick_core).watchdog_tick();
        });
        let termination_core = core.clone();
        let termination: WatchdogTermination = Box::new(move || {
            lock_runtime_core(&termination_core).health = RuntimeHealth::SchedulerTerminated;
        });
        let scheduler = scheduler.start(interval, ready_timeout, task, termination)?;
        Ok(ServingRuntime {
            core,
            scheduler: Some(scheduler),
        })
    }

    #[cfg(test)]
    pub(crate) fn arm_with_scheduler<S>(
        self,
        scheduler: S,
    ) -> Result<ServingRuntime<B, C, P, R>, SchedulerArmError>
    where
        S: SchedulerFactory,
    {
        self.arm(scheduler, WATCHDOG_INTERVAL, WATCHDOG_READY_TIMEOUT)
    }

    #[cfg(test)]
    pub(crate) fn arm_system_for_testing(
        self,
        interval: Duration,
        ready_timeout: Duration,
        mode: SystemSchedulerTestMode,
    ) -> Result<ServingRuntime<B, C, P, R>, SchedulerArmError> {
        let events = self.coordinator.store().events();
        self.arm(
            SystemSchedulerFactory {
                events,
                mode: mode.into(),
            },
            interval,
            ready_timeout,
        )
    }
}

impl<B, C, P, R> ServingRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    pub fn listener_permit(&self) -> ListenerPermit<'_> {
        ListenerPermit {
            _serving: PhantomData,
        }
    }

    pub fn acquire(
        &self,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        let mut core = lock_runtime_core(&self.core);
        core.require_healthy()?;
        core.coordinator
            .acquire(principal, profile, owner_generation, ttl_ms)
    }

    pub fn renew(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        let mut core = lock_runtime_core(&self.core);
        core.require_healthy()?;
        core.coordinator
            .renew(principal, lease_id, owner_generation, ttl_ms)
    }

    pub fn release(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
    ) -> Result<(), CoordinatorError> {
        let mut core = lock_runtime_core(&self.core);
        core.require_healthy()?;
        core.coordinator
            .release(principal, lease_id, owner_generation)
    }

    pub fn status(&self) -> Result<CoordinatorStatus, CoordinatorError> {
        let mut core = lock_runtime_core(&self.core);
        core.coordinator.status()
    }

    pub fn health(&self) -> RuntimeHealth {
        lock_runtime_core(&self.core).health
    }
}

impl<B, C, P, R> Drop for ServingRuntime<B, C, P, R>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    fn drop(&mut self) {
        // The guard's destructor signals and joins without holding `core`.
        // Dropping it explicitly keeps the coordinator alive until the worker
        // can no longer call its task or termination callback.
        drop(self.scheduler.take());
    }
}

/// Opaque proof that startup recovery and scheduler arming completed.
pub struct ListenerPermit<'a> {
    _serving: PhantomData<&'a ()>,
}

fn lock_runtime_core<B, C, P, R>(
    core: &Arc<Mutex<RuntimeCore<B, C, P, R>>>,
) -> MutexGuard<'_, RuntimeCore<B, C, P, R>>
where
    B: PmsetBackend,
    C: MonotonicClock,
    P: ProcessInspector,
    R: RandomSource,
{
    core.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy)]
enum SystemSchedulerMode {
    Normal,
    #[cfg(test)]
    ExitAfterReady,
    #[cfg(test)]
    PanicAfterReady,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum SystemSchedulerTestMode {
    Normal,
    ExitAfterReady,
    PanicAfterReady,
}

#[cfg(test)]
impl From<SystemSchedulerTestMode> for SystemSchedulerMode {
    fn from(mode: SystemSchedulerTestMode) -> Self {
        match mode {
            SystemSchedulerTestMode::Normal => Self::Normal,
            SystemSchedulerTestMode::ExitAfterReady => Self::ExitAfterReady,
            SystemSchedulerTestMode::PanicAfterReady => Self::PanicAfterReady,
        }
    }
}

struct SystemSchedulerFactory {
    events: Arc<dyn HelperEventSink>,
    mode: SystemSchedulerMode,
}

impl SystemSchedulerFactory {
    fn production(events: Arc<dyn HelperEventSink>) -> Self {
        Self {
            events,
            mode: SystemSchedulerMode::Normal,
        }
    }
}

type StopControl = Arc<(Mutex<bool>, Condvar)>;

struct SystemWatchdogGuard {
    stop: StopControl,
    worker: Mutex<Option<JoinHandle<()>>>,
    events: Arc<dyn HelperEventSink>,
}

impl WatchdogGuard for SystemWatchdogGuard {}

impl Drop for SystemWatchdogGuard {
    fn drop(&mut self) {
        signal_stop(&self.stop);
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
            self.events.record(HelperEvent::WatchdogSchedulerJoined);
        }
    }
}

#[derive(Clone, Copy)]
enum WorkerExit {
    Stopped,
    #[cfg(test)]
    Unexpected,
}

impl SchedulerFactory for SystemSchedulerFactory {
    fn start(
        self,
        interval: Duration,
        ready_timeout: Duration,
        mut task: WatchdogTask,
        termination: WatchdogTermination,
    ) -> Result<Box<dyn WatchdogGuard>, SchedulerArmError> {
        let stop: StopControl = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stop = stop.clone();
        let worker_events = self.events.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (activate_tx, activate_rx) = mpsc::sync_channel(1);
        let mode = self.mode;
        let worker = thread::Builder::new()
            .name("jarvis-power-watchdog".to_owned())
            .spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    if ready_tx.send(()).is_err() || activate_rx.recv().is_err() {
                        return WorkerExit::Stopped;
                    }
                    match mode {
                        SystemSchedulerMode::Normal => {}
                        #[cfg(test)]
                        SystemSchedulerMode::ExitAfterReady => return WorkerExit::Unexpected,
                        #[cfg(test)]
                        SystemSchedulerMode::PanicAfterReady => {
                            panic!("test-only watchdog panic")
                        }
                    }
                    loop {
                        let stopped = worker_stop
                            .1
                            .wait_timeout_while(
                                worker_stop
                                    .0
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                                interval,
                                |stopped| !*stopped,
                            )
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .0;
                        if *stopped {
                            return WorkerExit::Stopped;
                        }
                        drop(stopped);
                        task();
                    }
                }));
                match outcome {
                    Ok(WorkerExit::Stopped) => {
                        worker_events.record(HelperEvent::WatchdogSchedulerStopped);
                    }
                    #[cfg(test)]
                    Ok(WorkerExit::Unexpected) => {
                        termination();
                        worker_events.record(HelperEvent::WatchdogSchedulerTerminated);
                    }
                    Err(_) => {
                        termination();
                        worker_events.record(HelperEvent::WatchdogSchedulerTerminated);
                    }
                }
            })
            .map_err(|_| SchedulerArmError::SpawnFailed)?;

        match ready_rx.recv_timeout(ready_timeout) {
            Ok(()) => {
                self.events.record(HelperEvent::WatchdogSchedulerReady);
                if activate_tx.send(()).is_err() {
                    signal_stop(&stop);
                    let _ = worker.join();
                    self.events.record(HelperEvent::WatchdogSchedulerJoined);
                    return Err(SchedulerArmError::SpawnFailed);
                }
                Ok(Box::new(SystemWatchdogGuard {
                    stop,
                    worker: Mutex::new(Some(worker)),
                    events: self.events,
                }))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                signal_stop(&stop);
                let _ = activate_tx.send(());
                let _ = worker.join();
                self.events.record(HelperEvent::WatchdogSchedulerJoined);
                Err(SchedulerArmError::ReadyTimeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                signal_stop(&stop);
                let _ = worker.join();
                self.events.record(HelperEvent::WatchdogSchedulerJoined);
                Err(SchedulerArmError::SpawnFailed)
            }
        }
    }
}

fn signal_stop(stop: &StopControl) {
    *stop
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    stop.1.notify_all();
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

    pub fn reconcile_before_listener(self) -> Result<ProductionReadyRuntime, CoordinatorError> {
        self.inner
            .reconcile_before_listener()
            .map(|inner| ProductionReadyRuntime { inner })
    }
}

pub struct ProductionReadyRuntime {
    inner: ReadyRuntime<SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom>,
}

impl ProductionReadyRuntime {
    pub fn arm_watchdog(self) -> Result<ProductionRuntime, SchedulerArmError> {
        let events = self.inner.coordinator.store().events();
        self.inner
            .arm(
                SystemSchedulerFactory::production(events),
                WATCHDOG_INTERVAL,
                WATCHDOG_READY_TIMEOUT,
            )
            .map(|inner| ProductionRuntime { inner })
    }
}

pub struct ProductionRuntime {
    inner: ServingRuntime<SystemPmset, SystemMonotonicClock, SystemProcessInspector, SystemRandom>,
}

impl ProductionRuntime {
    pub fn listener_permit(&self) -> ListenerPermit<'_> {
        self.inner.listener_permit()
    }

    pub fn acquire(
        &self,
        principal: &Principal,
        profile: &str,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.inner
            .acquire(principal, profile, owner_generation, ttl_ms)
    }

    pub fn renew(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
        ttl_ms: u64,
    ) -> Result<LeaseGrant, CoordinatorError> {
        self.inner
            .renew(principal, lease_id, owner_generation, ttl_ms)
    }

    pub fn release(
        &self,
        principal: &Principal,
        lease_id: &LeaseId,
        owner_generation: &str,
    ) -> Result<(), CoordinatorError> {
        self.inner.release(principal, lease_id, owner_generation)
    }

    pub fn status(&self) -> Result<CoordinatorStatus, CoordinatorError> {
        self.inner.status()
    }

    pub fn health(&self) -> RuntimeHealth {
        self.inner.health()
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
