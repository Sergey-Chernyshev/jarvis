pub(crate) mod client;
pub(crate) mod lifecycle;
pub(crate) mod renewal;
pub(crate) mod xpc;

#[cfg(feature = "power-helper-dev")]
pub(crate) mod dev_uds;

// Keep the feature-gated client seam type-checked in normal host builds before
// the lifecycle integration consumes it. These are compile-time API contracts;
// they do not open the socket or select the development helper at runtime.
#[cfg(feature = "power-helper-dev")]
const _: fn(Option<&std::ffi::OsStr>) -> Option<dev_uds::DevUdsClient> =
    client::select_for_runtime_value;

#[cfg(feature = "power-helper-dev")]
const _: fn(
    &dev_uds::DevUdsClient,
    jarvis_power_core::protocol::Request,
) -> Result<client::HelperReply, client::HelperClientError> =
    <dev_uds::DevUdsClient as client::HelperClient>::send;

#[cfg(feature = "power-helper-dev")]
const _: fn(&dev_uds::DevUdsClient) -> client::HelperTrust =
    <dev_uds::DevUdsClient as client::HelperClient>::trust;

#[cfg(all(test, not(feature = "power-helper-dev")))]
mod feature_off_tests {
    #[test]
    fn feature_off_build_has_no_dev_transport_module_or_selector() {
        let source = include_str!("mod.rs");
        assert!(source.contains("#[cfg(feature = \"power-helper-dev\")]"));
        assert!(!cfg!(feature = "power-helper-dev"));
    }
}
