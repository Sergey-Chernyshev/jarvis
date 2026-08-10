#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc;
use std::time::Duration;

const UNREGISTER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceStatus {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BridgeError {
    OperationFailed,
}

pub(crate) type UnregisterCompletion = Box<dyn FnOnce(Result<(), BridgeError>) + Send + 'static>;

pub(crate) trait ServiceBridge: Send + Sync {
    fn status(&self) -> Result<ServiceStatus, BridgeError>;

    fn register(&self) -> Result<(), BridgeError>;

    fn unregister_async(&self, completion: UnregisterCompletion);
}

pub(crate) struct NativeServiceBridge;

#[cfg(target_os = "macos")]
impl ServiceBridge for NativeServiceBridge {
    fn status(&self) -> Result<ServiceStatus, BridgeError> {
        // SAFETY: the bridge has no parameters and returns a closed integer
        // status mapped below.
        match unsafe { jarvis_power_helper_service_status() } {
            0 => Ok(ServiceStatus::NotRegistered),
            1 => Ok(ServiceStatus::Enabled),
            2 => Ok(ServiceStatus::RequiresApproval),
            3 => Ok(ServiceStatus::NotFound),
            _ => Err(BridgeError::OperationFailed),
        }
    }

    fn register(&self) -> Result<(), BridgeError> {
        // SAFETY: the native bridge uses only the fixed bundled daemon plist.
        if unsafe { jarvis_power_helper_service_register() } == 0 {
            Ok(())
        } else {
            Err(BridgeError::OperationFailed)
        }
    }

    fn unregister_async(&self, completion: UnregisterCompletion) {
        let context = Box::into_raw(Box::new(completion)).cast::<c_void>();
        // SAFETY: context is a thin pointer to a boxed completion and the
        // native contract calls the callback exactly once after the async
        // SMAppService completion handler fires.
        unsafe {
            jarvis_power_helper_service_unregister(native_unregister_completion, context);
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl ServiceBridge for NativeServiceBridge {
    fn status(&self) -> Result<ServiceStatus, BridgeError> {
        Err(BridgeError::OperationFailed)
    }

    fn register(&self) -> Result<(), BridgeError> {
        Err(BridgeError::OperationFailed)
    }

    fn unregister_async(&self, completion: UnregisterCompletion) {
        completion(Err(BridgeError::OperationFailed));
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn native_unregister_completion(status: i32, context: *mut c_void) {
    if context.is_null() {
        return;
    }
    // SAFETY: unregister_async created this exact allocation and native calls
    // this callback once. Reconstructing the box transfers ownership back.
    let completion = unsafe { Box::from_raw(context.cast::<UnregisterCompletion>()) };
    let completion = *completion;
    completion(if status == 0 {
        Ok(())
    } else {
        Err(BridgeError::OperationFailed)
    });
}

#[cfg(target_os = "macos")]
extern "C" {
    fn jarvis_power_helper_service_status() -> i32;
    fn jarvis_power_helper_service_register() -> i32;
    fn jarvis_power_helper_service_unregister(
        completion: unsafe extern "C" fn(i32, *mut c_void),
        context: *mut c_void,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistrationOutcome {
    Registered,
    RequiresApproval,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleError {
    Bridge(BridgeError),
    NotFound,
    UnregisterTimeout,
    UnexpectedStatus,
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bridge(_) => "power-helper service operation failed",
            Self::NotFound => "power-helper service is missing from the application bundle",
            Self::UnregisterTimeout => "power-helper unregister completion timed out",
            Self::UnexpectedStatus => "power-helper service returned an unexpected status",
        })
    }
}

impl std::error::Error for LifecycleError {}

pub(crate) struct PowerHelperLifecycle<B> {
    bridge: B,
    unregister_timeout: Duration,
}

impl<B> PowerHelperLifecycle<B>
where
    B: ServiceBridge,
{
    pub(crate) fn new(bridge: B) -> Self {
        Self {
            bridge,
            unregister_timeout: UNREGISTER_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_timeout_for_testing(bridge: B, unregister_timeout: Duration) -> Self {
        Self {
            bridge,
            unregister_timeout,
        }
    }

    pub(crate) fn status(&self) -> Result<ServiceStatus, LifecycleError> {
        self.bridge.status().map_err(LifecycleError::Bridge)
    }

    pub(crate) fn ensure_registered(&self) -> Result<RegistrationOutcome, LifecycleError> {
        match self.status()? {
            ServiceStatus::Enabled => Ok(RegistrationOutcome::Registered),
            ServiceStatus::RequiresApproval => Ok(RegistrationOutcome::RequiresApproval),
            ServiceStatus::NotFound => Err(LifecycleError::NotFound),
            ServiceStatus::NotRegistered => self.register_and_verify(),
        }
    }

    pub(crate) fn replace(&self) -> Result<RegistrationOutcome, LifecycleError> {
        match self.status()? {
            ServiceStatus::NotFound => return Err(LifecycleError::NotFound),
            ServiceStatus::NotRegistered => return self.register_and_verify(),
            ServiceStatus::Enabled | ServiceStatus::RequiresApproval => {}
        }
        self.unregister_and_wait()?;
        if self.status()? != ServiceStatus::NotRegistered {
            return Err(LifecycleError::UnexpectedStatus);
        }
        self.register_and_verify()
    }

    fn register_and_verify(&self) -> Result<RegistrationOutcome, LifecycleError> {
        self.bridge.register().map_err(LifecycleError::Bridge)?;
        match self.status()? {
            ServiceStatus::Enabled => Ok(RegistrationOutcome::Registered),
            ServiceStatus::RequiresApproval => Ok(RegistrationOutcome::RequiresApproval),
            ServiceStatus::NotFound => Err(LifecycleError::NotFound),
            ServiceStatus::NotRegistered => Err(LifecycleError::UnexpectedStatus),
        }
    }

    fn unregister_and_wait(&self) -> Result<(), LifecycleError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.bridge.unregister_async(Box::new(move |result| {
            let _ = sender.send(result);
        }));
        receiver
            .recv_timeout(self.unregister_timeout)
            .map_err(|_| LifecycleError::UnregisterTimeout)?
            .map_err(LifecycleError::Bridge)
    }
}

const _: NativeServiceBridge = NativeServiceBridge;
const _: fn(NativeServiceBridge) -> PowerHelperLifecycle<NativeServiceBridge> =
    PowerHelperLifecycle::new;
const _: fn(&PowerHelperLifecycle<NativeServiceBridge>) -> Result<ServiceStatus, LifecycleError> =
    PowerHelperLifecycle::<NativeServiceBridge>::status;
const _: fn(
    &PowerHelperLifecycle<NativeServiceBridge>,
) -> Result<RegistrationOutcome, LifecycleError> =
    PowerHelperLifecycle::<NativeServiceBridge>::ensure_registered;
const _: fn(
    &PowerHelperLifecycle<NativeServiceBridge>,
) -> Result<RegistrationOutcome, LifecycleError> =
    PowerHelperLifecycle::<NativeServiceBridge>::replace;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        BridgeError, LifecycleError, PowerHelperLifecycle, RegistrationOutcome, ServiceBridge,
        ServiceStatus, UnregisterCompletion,
    };

    struct FakeBridge {
        status: Mutex<ServiceStatus>,
        register_status: ServiceStatus,
        events: Arc<Mutex<Vec<&'static str>>>,
        unregister_error: Option<BridgeError>,
    }

    impl ServiceBridge for FakeBridge {
        fn status(&self) -> Result<ServiceStatus, BridgeError> {
            Ok(*self.status.lock().unwrap())
        }

        fn register(&self) -> Result<(), BridgeError> {
            self.events.lock().unwrap().push("register");
            *self.status.lock().unwrap() = self.register_status;
            Ok(())
        }

        fn unregister_async(&self, completion: UnregisterCompletion) {
            self.events.lock().unwrap().push("unregister-start");
            let result = self.unregister_error.map_or(Ok(()), Err);
            if result.is_ok() {
                *self.status.lock().unwrap() = ServiceStatus::NotRegistered;
            }
            self.events.lock().unwrap().push("unregister-complete");
            completion(result);
        }
    }

    fn bridge(register_status: ServiceStatus) -> (FakeBridge, Arc<Mutex<Vec<&'static str>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            FakeBridge {
                status: Mutex::new(ServiceStatus::NotRegistered),
                register_status,
                events: events.clone(),
                unregister_error: None,
            },
            events,
        )
    }

    #[test]
    fn requires_approval_is_not_reported_as_registered_success() {
        let (bridge, _) = bridge(ServiceStatus::RequiresApproval);
        let lifecycle =
            PowerHelperLifecycle::with_timeout_for_testing(bridge, Duration::from_millis(100));
        assert_eq!(
            lifecycle.ensure_registered(),
            Ok(RegistrationOutcome::RequiresApproval)
        );
    }

    #[test]
    fn replacement_waits_for_async_unregister_completion_before_registering() {
        let (bridge, events) = bridge(ServiceStatus::Enabled);
        *bridge.status.lock().unwrap() = ServiceStatus::Enabled;
        let lifecycle =
            PowerHelperLifecycle::with_timeout_for_testing(bridge, Duration::from_millis(100));
        assert_eq!(lifecycle.replace(), Ok(RegistrationOutcome::Registered));
        assert_eq!(
            *events.lock().unwrap(),
            vec!["unregister-start", "unregister-complete", "register"]
        );
    }

    #[test]
    fn unregister_failure_prevents_reregistering() {
        let (mut bridge, events) = bridge(ServiceStatus::Enabled);
        *bridge.status.lock().unwrap() = ServiceStatus::Enabled;
        bridge.unregister_error = Some(BridgeError::OperationFailed);
        let lifecycle =
            PowerHelperLifecycle::with_timeout_for_testing(bridge, Duration::from_millis(100));
        assert_eq!(
            lifecycle.replace(),
            Err(LifecycleError::Bridge(BridgeError::OperationFailed))
        );
        assert_eq!(
            *events.lock().unwrap(),
            vec!["unregister-start", "unregister-complete"]
        );
    }
}
