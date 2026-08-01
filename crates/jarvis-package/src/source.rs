#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};

    use super::{
        snapshot_source, snapshot_source_with_hook, SnapshotHook,
    };

    struct Hook<F, G, H> {
        after_enumeration: F,
        before_open: G,
        after_copy_chunk: H,
    }

    impl<F, G, H> SnapshotHook for Hook<F, G, H>
    where
        F: Fn(),
        G: Fn(&str),
        H: Fn(&str, u64),
    {
        fn after_enumeration(&self) {
            (self.after_enumeration)();
        }

        fn before_open(&self, path: &str) {
            (self.before_open)(path);
        }

        fn after_copy_chunk(&self, path: &str, copied: u64) {
            (self.after_copy_chunk)(path, copied);
        }
    }

    fn no_op_hook() -> Hook<impl Fn(), impl Fn(&str), impl Fn(&str, u64)> {
        Hook {
            after_enumeration: || {},
            before_open: |_| {},
            after_copy_chunk: |_, _| {},
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("plugin.json"), b"{}").unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("nested/payload.txt"), b"original").unwrap();
        (root, source)
    }

    fn assert_raced(result: Result<crate::spool::SourceSnapshot, crate::PackageError>) {
        assert_eq!(result.unwrap_err().code(), "source_raced");
    }

    #[test]
    fn source_file_replaced_after_enumeration_never_packages_outside_bytes() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        let original = source.join("nested/original.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::rename(&payload, &original).unwrap();
                fs::write(&payload, b"attacker").unwrap();
            },
            before_open: |_| {},
            after_copy_chunk: |_, _| {},
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_file_changed_to_symlink_before_open_is_source_raced() {
        let (root, source) = fixture();
        let outside = root.path().join("outside");
        fs::write(&outside, b"outside-secret").unwrap();
        let payload = source.join("nested/payload.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::remove_file(&payload).unwrap();
                symlink(&outside, &payload).unwrap();
            },
            before_open: |_| {},
            after_copy_chunk: |_, _| {},
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_parent_directory_swap_is_source_raced() {
        let (_root, source) = fixture();
        let nested = source.join("nested");
        let original = source.join("held-original");
        let hook = Hook {
            after_enumeration: || {
                fs::rename(&nested, &original).unwrap();
                fs::create_dir(&nested).unwrap();
                fs::write(nested.join("payload.txt"), b"attacker").unwrap();
            },
            before_open: |_| {},
            after_copy_chunk: |_, _| {},
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_file_mutated_during_copy_is_source_raced() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        fs::write(&payload, vec![b'a'; 192 * 1024]).unwrap();
        let mutated = Cell::new(false);
        let hook = Hook {
            after_enumeration: || {},
            before_open: |_| {},
            after_copy_chunk: |path: &str, copied: u64| {
                if path == "nested/payload.txt" && copied > 0 && !mutated.replace(true) {
                    fs::write(&payload, vec![b'b'; 192 * 1024]).unwrap();
                }
            },
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn source_inode_reused_with_different_metadata_is_source_raced() {
        let (_root, source) = fixture();
        let payload = source.join("nested/payload.txt");
        let hook = Hook {
            after_enumeration: || {
                fs::set_permissions(&payload, fs::Permissions::from_mode(0o600)).unwrap();
            },
            before_open: |_| {},
            after_copy_chunk: |_, _| {},
        };

        assert_raced(snapshot_source_with_hook(&source, &hook));
    }

    #[test]
    fn tar_writer_reads_only_spool_after_source_snapshot() {
        let (_root, source) = fixture();
        let snapshot = snapshot_source(&source).unwrap();
        fs::write(source.join("nested/payload.txt"), b"changed").unwrap();

        assert_eq!(
            snapshot.read_file("nested/payload.txt").unwrap(),
            b"original"
        );
    }

    #[test]
    fn clean_snapshot_accepts_only_regular_files_and_directories() {
        let (_root, source) = fixture();
        let snapshot = snapshot_source_with_hook(&source, &no_op_hook()).unwrap();
        assert_eq!(snapshot.read_file("plugin.json").unwrap(), b"{}");

        let link = source.join("link");
        symlink(Path::new("plugin.json"), link).unwrap();
        assert!(snapshot_source(&source).is_err());
    }
}
