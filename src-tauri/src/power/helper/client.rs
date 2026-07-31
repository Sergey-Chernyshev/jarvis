#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    use super::{select_for_runtime_value, HelperTrust};

    #[test]
    fn app_selection_requires_compile_feature_plus_exact_runtime_flag() {
        assert_eq!(
            select_for_runtime_value(Some(OsStr::new("1")))
                .unwrap()
                .trust(),
            HelperTrust::DevelopmentOnly
        );
        for rejected in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("1 ")),
            Some(OsStr::new("true")),
            Some(OsStr::new("0")),
        ] {
            assert!(select_for_runtime_value(rejected).is_none());
        }
        let non_unicode = OsString::from_vec(vec![b'1', 0xff]);
        assert!(select_for_runtime_value(Some(&non_unicode)).is_none());
    }
}
