pub(crate) mod client;

#[cfg(feature = "power-helper-dev")]
pub(crate) mod dev_uds;

#[cfg(all(test, not(feature = "power-helper-dev")))]
mod feature_off_tests {
    #[test]
    fn feature_off_build_has_no_dev_transport_module_or_selector() {
        let source = include_str!("mod.rs");
        assert!(source.contains("#[cfg(feature = \"power-helper-dev\")]"));
        assert!(!cfg!(feature = "power-helper-dev"));
    }
}
