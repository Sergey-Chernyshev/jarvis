#[cfg(test)]
mod tests {
    use std::fs;

    use crate::source::snapshot_source;

    use super::checked_span;

    #[test]
    fn aggregate_spool_is_owner_only_unlinked_and_spans_are_exact() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("plugin.json"), b"{}").unwrap();
        fs::write(source.path().join("payload"), b"payload").unwrap();

        let snapshot = snapshot_source(source.path()).unwrap();
        let identity = snapshot.spool_identity().unwrap();
        assert_eq!(identity.mode & 0o777, 0o600);
        assert_eq!(identity.link_count, 0);
        assert_eq!(snapshot.read_file("plugin.json").unwrap(), b"{}");
        assert_eq!(snapshot.read_file("payload").unwrap(), b"payload");
    }

    #[test]
    fn checked_spans_reject_offset_and_length_overflow() {
        assert_eq!(checked_span(10, 20).unwrap(), 10..30);
        assert!(checked_span(u64::MAX, 1).is_err());
        assert!(checked_span(u64::MAX - 1, 2).is_err());
    }
}
