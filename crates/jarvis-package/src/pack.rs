#[cfg(test)]
mod tests {
    use jarvis_plugin_protocol::manifest::{ManifestV2, RuntimeKind};
    use jarvis_plugin_protocol::package::{
        MacOsVersion, PackageFileMode, PackagePath, PackageSignatureV1, PackageTarget,
    };

    use super::{
        fixed_opaque_observation_matches, prepare_package_documents, FixedOpaqueSignature,
        PackOptions, PackageDocumentAdapter, PackageError, PayloadObservation,
        FIXED_OPAQUE_SIGNATURE_VALUE, SIGNATURE_MESSAGE_DOMAIN,
    };
    use crate::hash::sha256_digest;

    const SOURCE_MANIFEST: &[u8] = include_bytes!(
        "../tests/fixtures/plugin-packages/pack-source/plugin.json"
    );
    const UI: &[u8] =
        include_bytes!("../tests/fixtures/plugin-packages/pack-source/ui/index.html");
    const SCHEMA: &[u8] = include_bytes!(
        "../tests/fixtures/plugin-packages/pack-source/schemas/message.schema.json"
    );

    struct FixtureAdapter;

    impl PackageDocumentAdapter for FixtureAdapter {
        fn resolve_source_manifest(
            &self,
            spooled_bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(spooled_bytes).map_err(PackageError::manifest)
        }

        fn validate_packaged_manifest(
            &self,
            canonical_bytes: &[u8],
            _target: PackageTarget,
        ) -> Result<ManifestV2, PackageError> {
            ManifestV2::parse(canonical_bytes).map_err(PackageError::manifest)
        }

        fn validate_package_metadata_schema(
            &self,
            _canonical_bytes: &[u8],
        ) -> Result<(), PackageError> {
            Ok(())
        }

        fn validate_package_signature_schema(
            &self,
            _canonical_bytes: &[u8],
        ) -> Result<(), PackageError> {
            Ok(())
        }
    }

    fn observation(path: &str, bytes: &[u8]) -> PayloadObservation {
        PayloadObservation::new(
            PackagePath::new(path).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
            sha256_digest(bytes),
        )
    }

    fn ui_payload() -> Vec<PayloadObservation> {
        vec![
            observation("plugin.json", SOURCE_MANIFEST),
            observation("ui/index.html", UI),
            observation("schemas/message.schema.json", SCHEMA),
        ]
    }

    fn options() -> PackOptions {
        PackOptions {
            target: PackageTarget::DarwinArm64,
            minimum_macos: MacOsVersion::parse("14.0.0").unwrap(),
        }
    }

    #[test]
    fn metadata_equals_concrete_manifest() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        let manifest = prepared.manifest();
        let metadata = prepared.metadata();

        assert_eq!(manifest.runtime.kind, RuntimeKind::UiOnly);
        assert_eq!(metadata.plugin_id, manifest.id);
        assert_eq!(metadata.publisher, manifest.publisher);
        assert_eq!(metadata.version, manifest.version);
        assert_eq!(metadata.jarvis_range, manifest.compatibility.jarvis);
        assert_eq!(metadata.plugin_api, manifest.compatibility.plugin_api);
        assert_eq!(metadata.state, manifest.state);
        assert_eq!(metadata.manifest_digest, sha256_digest(prepared.manifest_bytes()));
        assert_eq!(
            metadata
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["plugin.json", "schemas/message.schema.json", "ui/index.html"]
        );
        assert!(metadata
            .files
            .iter()
            .all(|file| file.mode == PackageFileMode::ReadOnly));
        assert_eq!(
            serde_json_canonicalizer::to_vec(metadata).unwrap(),
            prepared.metadata_bytes()
        );
    }

    #[test]
    fn verified_native_entries_alone_are_executable() {
        let manifest = native_manifest();
        let payload = vec![
            observation("plugin.json", &manifest),
            observation("bin/bridge", b"bridge"),
            observation("bin/controller", b"controller"),
            observation("bin/not-declared", b"data"),
        ];
        let prepared = prepare_package_documents(
            &manifest,
            payload,
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();

        let modes = prepared
            .metadata()
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.mode))
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            vec![
                ("plugin.json", PackageFileMode::ReadOnly),
                ("bin/bridge", PackageFileMode::Executable),
                ("bin/controller", PackageFileMode::Executable),
                ("bin/not-declared", PackageFileMode::ReadOnly),
            ]
        );
    }

    #[test]
    fn missing_declared_native_entry_is_rejected() {
        let manifest = native_manifest();
        let payload = vec![
            observation("plugin.json", &manifest),
            observation("bin/bridge", b"bridge"),
        ];
        assert_eq!(
            prepare_package_documents(
                &manifest,
                payload,
                options(),
                &FixtureAdapter,
                &FixedOpaqueSignature,
            )
            .unwrap_err()
            .code(),
            "package_metadata"
        );
    }

    #[test]
    fn fixed_opaque_signature_matches_golden() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        assert_eq!(prepared.signature().value, FIXED_OPAQUE_SIGNATURE_VALUE);
        assert_eq!(
            prepared.signature_bytes(),
            serde_json_canonicalizer::to_vec(prepared.signature())
                .unwrap()
                .as_slice()
        );

        let mut expected_message = SIGNATURE_MESSAGE_DOMAIN.to_vec();
        expected_message.push(0);
        expected_message.extend_from_slice(prepared.metadata_bytes());
        assert_eq!(prepared.signature_message(), expected_message);
        assert!(fixed_opaque_observation_matches(
            &expected_message,
            prepared.signature(),
            &expected_message
        ));
    }

    #[test]
    fn fixed_opaque_fixture_rejects_one_bit_message_or_signature_changes() {
        let prepared = prepare_package_documents(
            SOURCE_MANIFEST,
            ui_payload(),
            options(),
            &FixtureAdapter,
            &FixedOpaqueSignature,
        )
        .unwrap();
        let expected_message = prepared.signature_message().to_vec();

        let mut changed_message = expected_message.clone();
        changed_message[0] ^= 1;
        assert!(!fixed_opaque_observation_matches(
            &changed_message,
            prepared.signature(),
            &expected_message
        ));

        let mut changed_signature: PackageSignatureV1 = prepared.signature().clone();
        changed_signature.value.replace_range(0..1, "q");
        assert!(!fixed_opaque_observation_matches(
            &expected_message,
            &changed_signature,
            &expected_message
        ));
    }

    fn native_manifest() -> Vec<u8> {
        br#"{
          "schemaVersion":2,
          "id":"dev.example.native",
          "name":"Native",
          "version":"1.0.0",
          "publisher":"example",
          "compatibility":{"jarvis":">=0.4.0, <0.5.0","pluginApi":2},
          "runtime":{
            "kind":"verified-native",
            "lifecycle":"service-bridge",
            "bridgeEntry":"bin/bridge",
            "service":{
              "id":"controller",
              "manager":"launchd-user",
              "entry":"bin/controller",
              "survivesCoreExit":true
            },
            "protocol":2,
            "activationEvents":[]
          },
          "permissions":[],
          "state":{"schemaVersion":1,"migrations":[],"rollbackCompatibleThrough":1},
          "contributes":{
            "pages":[],"commands":[],"actions":[],"hotkeys":[],"settings":[],
            "projectRuntimes":[],"dataContracts":[]
          }
        }"#
        .to_vec()
    }
}
