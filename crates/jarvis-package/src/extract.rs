#[cfg(test)]
mod tests {
    use std::fs::{self, File, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageMetadataV1, PackageSignatureV1, PackageTarget,
    };

    use super::{
        extract_verified_package, extract_verified_package_with_hook, inspect_and_verify_package,
        ExtractionHook, PackageTrustError, PackageTrustVerifier, UntrustedPackageObservation,
        VerifiedPackageEvidence,
    };
    use crate::archive::{inspect_reader_with_limits, ArchiveLimits};
    use crate::{PackageDocumentAdapter, PackageError};

    const GOLDEN: &[u8] =
        include_bytes!("../tests/fixtures/plugin-packages/golden/darwin-arm64.jarvis-plugin");

    struct FixtureAdapter;

    impl PackageDocumentAdapter for FixtureAdapter {
        fn resolve_source_manifest(
            &self,
            bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(bytes).map_err(PackageError::manifest)
        }

        fn validate_packaged_manifest(
            &self,
            bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(bytes).map_err(PackageError::manifest)
        }

        fn validate_package_metadata_schema(&self, _bytes: &[u8]) -> Result<(), PackageError> {
            Ok(())
        }

        fn validate_package_signature_schema(&self, _bytes: &[u8]) -> Result<(), PackageError> {
            Ok(())
        }
    }

    struct ExactVerifier {
        package_json: Vec<u8>,
        signature: Vec<u8>,
        digest: jarvis_plugin_protocol::manifest::Digest,
    }

    impl ExactVerifier {
        fn from_golden() -> Self {
            let mut bytes = GOLDEN;
            let inspection =
                inspect_reader_with_limits(&mut bytes, ArchiveLimits::production()).unwrap();
            Self {
                package_json: inspection.package_json().to_vec(),
                signature: inspection.signature().to_vec(),
                digest: inspection.physical_digest().clone(),
            }
        }
    }

    impl PackageTrustVerifier for ExactVerifier {
        fn verify(
            &self,
            observation: &UntrustedPackageObservation<'_>,
        ) -> Result<(), PackageTrustError> {
            if observation.package_json() == self.package_json
                && observation.signature_bytes() == self.signature
                && observation.archive_digest() == &self.digest
                && observation.metadata().target == PackageTarget::DarwinArm64
                && observation.signature().key_id == "fixture.opaque:1"
            {
                Ok(())
            } else {
                Err(PackageTrustError::new("fixture_mismatch"))
            }
        }
    }

    struct RejectVerifier;

    impl PackageTrustVerifier for RejectVerifier {
        fn verify(
            &self,
            _observation: &UntrustedPackageObservation<'_>,
        ) -> Result<(), PackageTrustError> {
            Err(PackageTrustError::new("catalog_digest_mismatch"))
        }
    }

    fn archive_file() -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(GOLDEN).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    }

    fn verify_fixture(file: File) -> VerifiedPackageEvidence {
        inspect_and_verify_package(file, &FixtureAdapter, &ExactVerifier::from_golden()).unwrap()
    }

    fn held_directory(path: &Path) -> OwnedFd {
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap()
    }

    fn signature_body_offset() -> u64 {
        let mut bytes = GOLDEN;
        let inspection =
            inspect_reader_with_limits(&mut bytes, ArchiveLimits::production()).unwrap();
        let signature = inspection.entries().last().unwrap();
        let value_offset = inspection
            .signature()
            .windows(b"paWl".len())
            .position(|window| window == b"paWl")
            .unwrap();
        signature.body_offset() + value_offset as u64
    }

    #[test]
    fn bad_signature_never_creates_quarantine_output() {
        let parent = tempfile::tempdir().unwrap();
        let parent_fd = held_directory(parent.path());
        let file = archive_file();
        file.write_all_at(b"q", signature_body_offset()).unwrap();

        assert_eq!(
            inspect_and_verify_package(file, &FixtureAdapter, &ExactVerifier::from_golden())
                .unwrap_err()
                .code(),
            "package_trust"
        );
        assert!(!parent.path().join("quarantine").exists());
        drop(parent_fd);
    }

    #[test]
    fn catalog_digest_mismatch_never_creates_quarantine_output() {
        let parent = tempfile::tempdir().unwrap();
        let parent_fd = held_directory(parent.path());
        assert_eq!(
            inspect_and_verify_package(archive_file(), &FixtureAdapter, &RejectVerifier)
                .unwrap_err()
                .code(),
            "package_trust"
        );
        assert!(!parent.path().join("quarantine").exists());
        drop(parent_fd);
    }

    #[test]
    fn extract_requires_opaque_verified_package_evidence() {
        assert!(std::mem::size_of::<VerifiedPackageEvidence>() > 0);
    }

    #[test]
    fn archive_path_swap_after_verification_reads_same_fd() {
        let root = tempfile::tempdir().unwrap();
        let archive_path = root.path().join("package.jarvis-plugin");
        fs::write(&archive_path, GOLDEN).unwrap();
        let evidence = verify_fixture(File::open(&archive_path).unwrap());
        fs::rename(&archive_path, root.path().join("verified-original")).unwrap();
        fs::write(&archive_path, b"attacker").unwrap();

        let output = root.path().join("output");
        fs::create_dir(&output).unwrap();
        let output_fd = held_directory(&output);
        extract_verified_package(evidence, &output_fd, "quarantine").unwrap();
        assert_eq!(
            fs::read(output.join("quarantine/ui/index.html")).unwrap(),
            include_bytes!("../tests/fixtures/plugin-packages/pack-source/ui/index.html")
        );
    }

    #[test]
    fn same_inode_mutation_after_verification_is_rejected() {
        let file = archive_file();
        let mutator = file.try_clone().unwrap();
        let evidence = verify_fixture(file);
        mutator.write_all_at(b"q", signature_body_offset()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let parent_fd = held_directory(parent.path());
        assert_eq!(
            extract_verified_package(evidence, &parent_fd, "quarantine")
                .unwrap_err()
                .code(),
            "archive_changed_after_verification"
        );
        assert!(!parent.path().join("quarantine").exists());
    }

    #[test]
    fn second_pass_requires_identical_package_signature_and_entry_plan() {
        same_inode_mutation_after_verification_is_rejected();
    }

    struct RootSymlinkHook {
        root: PathBuf,
        outside: PathBuf,
    }

    impl ExtractionHook for RootSymlinkHook {
        fn after_root_created(&self) {
            symlink(&self.outside, self.root.join("plugin.json")).unwrap();
        }
    }

    #[test]
    fn symlink_parent_and_final_component_are_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), parent.path().join("preexisting")).unwrap();
        let parent_fd = held_directory(parent.path());
        assert!(extract_verified_package(
            verify_fixture(archive_file()),
            &parent_fd,
            "preexisting"
        )
        .is_err());

        let root = parent.path().join("quarantine");
        let hook = RootSymlinkHook {
            root: root.clone(),
            outside: outside.path().to_owned(),
        };
        assert!(extract_verified_package_with_hook(
            verify_fixture(archive_file()),
            &parent_fd,
            "quarantine",
            &hook,
        )
        .is_err());
        assert!(!outside.path().join("plugin.json").exists());
        fs::remove_file(root.join("plugin.json")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    enum PreexistingKind {
        File,
        Hardlink,
        Socket,
    }

    struct PreexistingHook {
        root: PathBuf,
        kind: PreexistingKind,
    }

    impl ExtractionHook for PreexistingHook {
        fn after_root_created(&self) {
            let path = self.root.join("plugin.json");
            match self.kind {
                PreexistingKind::File => fs::write(path, b"preexisting").unwrap(),
                PreexistingKind::Hardlink => {
                    let source = self.root.join("hardlink-source");
                    fs::write(&source, b"preexisting").unwrap();
                    fs::hard_link(source, path).unwrap();
                }
                PreexistingKind::Socket => {
                    UnixListener::bind(path).unwrap();
                }
            }
        }
    }

    #[test]
    fn preexisting_file_hardlink_and_special_file_are_rejected() {
        for (index, kind) in [
            PreexistingKind::File,
            PreexistingKind::Hardlink,
            PreexistingKind::Socket,
        ]
        .into_iter()
        .enumerate()
        {
            let parent = tempfile::tempdir().unwrap();
            let parent_fd = held_directory(parent.path());
            let name = format!("quarantine-{index}");
            let hook = PreexistingHook {
                root: parent.path().join(&name),
                kind,
            };
            assert!(extract_verified_package_with_hook(
                verify_fixture(archive_file()),
                &parent_fd,
                &name,
                &hook,
            )
            .is_err());
        }
    }

    enum FailureMode {
        ShortWrite,
        Digest,
        Fsync,
    }

    struct FailureHook(FailureMode);

    impl ExtractionHook for FailureHook {
        fn fail_write_after(&self, path: &str, copied: u64) -> bool {
            matches!(self.0, FailureMode::ShortWrite) && path == "plugin.json" && copied > 0
        }

        fn mutate_chunk(&self, path: &str, bytes: &mut [u8]) {
            if matches!(self.0, FailureMode::Digest) && path == "plugin.json" && !bytes.is_empty() {
                bytes[0] ^= 1;
            }
        }

        fn fail_sync(&self, path: &str) -> bool {
            matches!(self.0, FailureMode::Fsync) && path == "plugin.json"
        }
    }

    #[test]
    fn short_write_digest_mismatch_and_fsync_failure_cleanup_exactly() {
        for (index, mode) in [
            FailureMode::ShortWrite,
            FailureMode::Digest,
            FailureMode::Fsync,
        ]
        .into_iter()
        .enumerate()
        {
            let parent = tempfile::tempdir().unwrap();
            let parent_fd = held_directory(parent.path());
            let name = format!("quarantine-{index}");
            assert!(extract_verified_package_with_hook(
                verify_fixture(archive_file()),
                &parent_fd,
                &name,
                &FailureHook(mode),
            )
            .is_err());
            assert!(!parent.path().join(name).exists());
        }
    }

    struct CleanupRaceHook {
        root: PathBuf,
        outside: PathBuf,
    }

    impl ExtractionHook for CleanupRaceHook {
        fn fail_write_after(&self, path: &str, copied: u64) -> bool {
            path == "plugin.json" && copied > 0
        }

        fn before_cleanup(&self, path: &str) {
            if path == "plugin.json" {
                fs::remove_file(self.root.join(path)).unwrap();
                symlink(&self.outside, self.root.join(path)).unwrap();
            }
        }
    }

    #[test]
    fn cleanup_race_cannot_unlink_outside_quarantine() {
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        let parent_fd = held_directory(parent.path());
        let root = parent.path().join("quarantine");
        let result = extract_verified_package_with_hook(
            verify_fixture(archive_file()),
            &parent_fd,
            "quarantine",
            &CleanupRaceHook {
                root: root.clone(),
                outside: outside.path().to_owned(),
            },
        );
        assert_eq!(result.unwrap_err().code(), "quarantine_manual_cleanup");
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"keep");
        fs::remove_file(root.join("plugin.json")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn successful_extract_uses_declared_modes_and_link_count_one() {
        let parent = tempfile::tempdir().unwrap();
        let parent_fd = held_directory(parent.path());
        let extracted =
            extract_verified_package(verify_fixture(archive_file()), &parent_fd, "quarantine")
                .unwrap();
        assert_eq!(extracted.name(), "quarantine");
        for path in [
            "plugin.json",
            "schemas/message.schema.json",
            "ui/index.html",
        ] {
            let metadata = fs::metadata(parent.path().join("quarantine").join(path)).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o444);
            assert_eq!(metadata.nlink(), 1);
        }
    }

    trait WriteAllAt {
        fn write_all_at(&self, bytes: &[u8], offset: u64) -> std::io::Result<()>;
    }

    impl WriteAllAt for File {
        fn write_all_at(&self, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
            use std::os::unix::fs::FileExt;
            while !bytes.is_empty() {
                let written = self.write_at(bytes, offset)?;
                if written == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "zero-length positioned write",
                    ));
                }
                offset += written as u64;
                bytes = &bytes[written..];
            }
            Ok(())
        }
    }
}
