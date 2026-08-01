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
