use std::ffi::CStr;
use std::sync::Arc;

#[cfg(test)]
use jarvis_power_core::state::HelperState;

use crate::root_store::{
    sealed, DevRoot, LockedRootStore, RootStore, StateStore, StoreError, StoreFiles,
};
use crate::HelperEventSink;

#[cfg(test)]
pub const DEV_STATE_FILE: &str = "dev-helper-v2.json";
#[cfg(test)]
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
        root: &DevRoot,
        events: Arc<dyn HelperEventSink>,
    ) -> Result<Self, StoreError> {
        RootStore::open_development(
            root,
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
        root: &DevRoot,
        events: Arc<dyn HelperEventSink>,
    ) -> Result<Self, StoreError> {
        Self::open(root, events)
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
