use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;

use jarvis_power_core::state::HelperState;

use crate::root_store::{
    sealed, LockedRootStore, LockedState, RootStore, StateStore, StoreError, StoreFiles,
};
use crate::HelperEventSink;

pub const DEV_STATE_FILE: &str = "dev-helper-v2.json";
pub const DEV_LOCK_FILE: &str = "dev-helper-v2.lock";

const DEV_STATE_COMPONENT: &CStr = c"dev-helper-v2.json";
const DEV_LOCK_COMPONENT: &CStr = c"dev-helper-v2.lock";
const DEV_TEMPORARY_PREFIX: &str = ".dev-helper-v2.tmp-";

#[derive(Clone, Debug)]
pub(crate) struct DevStore {
    inner: RootStore,
}

impl DevStore {
    pub(crate) fn open(
        jarvis_directory: &Path,
        events: Arc<dyn HelperEventSink>,
    ) -> Result<Self, StoreError> {
        RootStore::open_development(
            jarvis_directory,
            StoreFiles::new(
                DEV_STATE_COMPONENT,
                DEV_LOCK_COMPONENT,
                DEV_TEMPORARY_PREFIX,
            ),
            events,
        )
        .map(|inner| Self { inner })
    }

    #[cfg(test)]
    pub(crate) fn open_for_testing(
        jarvis_directory: &Path,
        events: Arc<dyn HelperEventSink>,
    ) -> Result<Self, StoreError> {
        Self::open(jarvis_directory, events)
    }

    #[cfg(test)]
    pub(crate) fn load(&self) -> Result<Option<HelperState>, StoreError> {
        self.lock()?.load()
    }
}

impl sealed::Sealed for DevStore {}

impl StateStore for DevStore {
    type Locked<'a> = LockedRootStore<'a>;

    fn lock(&self) -> Result<Self::Locked<'_>, StoreError> {
        self.inner.lock()
    }

    fn events(&self) -> Arc<dyn HelperEventSink> {
        self.inner.events()
    }
}
