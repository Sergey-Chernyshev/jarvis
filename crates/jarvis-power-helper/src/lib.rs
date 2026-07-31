#![deny(unsafe_op_in_unsafe_fn)]

use jarvis_power_core::state::MutationPhase;

pub mod coordinator;
#[cfg(feature = "dev-uds")]
pub(crate) mod dev_store;
pub mod pmset;
pub mod root_store;
pub mod watchdog;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperEvent {
    StartupRecovery,
    StartupReady,
    WatchdogSchedulerReady,
    WatchdogSchedulerStopped,
    WatchdogSchedulerJoined,
    WatchdogSchedulerTerminated,
    WatchdogRecovery,
    LockAcquired,
    PowerRead(bool),
    PowerWrite(bool),
    StateWriteStarted(MutationPhase),
    TempFileSynced,
    StateRenamed,
    ParentDirectorySynced,
    StateCleared,
    ReplyReady,
    LockReleased,
}

pub trait HelperEventSink: Send + Sync {
    fn record(&self, event: HelperEvent);
}

#[derive(Default)]
pub(crate) struct NoopEventSink;

impl HelperEventSink for NoopEventSink {
    fn record(&self, _event: HelperEvent) {}
}

#[cfg(test)]
extern crate self as jarvis_power_helper;

#[cfg(test)]
#[path = "../tests/support/root_store.rs"]
mod root_store_contract_tests;

#[cfg(test)]
#[path = "../tests/support/watchdog.rs"]
mod watchdog_contract_tests;

#[cfg(all(test, feature = "dev-uds"))]
#[path = "../tests/support/dev_uds.rs"]
mod dev_uds_contract_tests;
