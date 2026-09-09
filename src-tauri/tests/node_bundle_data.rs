#[allow(dead_code)]
#[path = "../build_node_bundle.rs"]
mod node_bundle;

use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("jarvis-node-data-{}-{stamp}", std::process::id()));
        fs::create_dir_all(root.join("src-tauri/node-binaries")).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        Self { root }
    }
    fn emit(&self) {
        node_bundle::embed_at(
            &self.root.join("src-tauri"),
            &self.root.join("out"),
            "9.8.7",
        );
    }
    fn manifest(&self) -> PathBuf {
        self.root.join("src-tauri/node-binaries/manifest.json")
    }
    fn bundle_manifest(&self) -> serde_json::Value {
        let files = [
            "src-tauri/Cargo.lock",
            "src-tauri/node/Cargo.toml",
            "src-tauri/node/src/main.rs",
            "src-tauri/shared/Cargo.toml",
            "src-tauri/shared/src/lib.rs",
        ];
        let mut fingerprint = 0xcbf29ce484222325u64;
        for name in files {
            let file = self.root.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, b"fixture\n").unwrap();
            for byte in name
                .bytes()
                .chain([0])
                .chain(b"fixture\n".iter().copied())
                .chain([0])
            {
                fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
        let artifacts: Vec<_> = TARGETS.iter().enumerate().map(|(index, target)| {
            let mut bytes = vec![0; 64];
            bytes[..4].copy_from_slice(b"\x7fELF"); bytes[4] = 2; bytes[5] = 1;
            bytes[18..20].copy_from_slice(&(if index == 0 { 62u16 } else { 183u16 }).to_le_bytes());
            let name = format!("jarvis-node-{target}");
            fs::write(self.root.join("src-tauri/node-binaries").join(&name), &bytes).unwrap();
            serde_json::json!({"target": target, "file": name, "sha256": format!("{:x}", Sha256::digest(&bytes))})
        }).collect();
        serde_json::json!({"version":"9.8.7", "sourceFingerprint":format!("{fingerprint:016x}"), "artifacts":artifacts})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn unbundled_build_emits_only_empty_data_files() {
    let fixture = Fixture::new();
    fixture.emit();
    for target in TARGETS {
        assert!(fs::read(
            fixture
                .root
                .join("out")
                .join(format!("jarvis-node-{target}.bin"))
        )
        .unwrap()
        .is_empty());
    }
    assert_eq!(fs::read_dir(fixture.root.join("out")).unwrap().count(), 2);
}

#[test]
fn verified_bundle_emits_exact_bytes_without_generated_rust() {
    let fixture = Fixture::new();
    let manifest = fixture.bundle_manifest();
    fs::write(fixture.manifest(), serde_json::to_vec(&manifest).unwrap()).unwrap();
    fixture.emit();
    for target in TARGETS {
        assert_eq!(
            fs::read(
                fixture
                    .root
                    .join("out")
                    .join(format!("jarvis-node-{target}.bin"))
            )
            .unwrap(),
            fs::read(
                fixture
                    .root
                    .join("src-tauri/node-binaries")
                    .join(format!("jarvis-node-{target}"))
            )
            .unwrap()
        );
    }
    assert_eq!(fs::read_dir(fixture.root.join("out")).unwrap().count(), 2);
}

#[test]
fn changed_shared_source_checksum_target_and_version_fail_closed() {
    for failure in ["shared", "checksum", "target", "version"] {
        let fixture = Fixture::new();
        let mut manifest = fixture.bundle_manifest();
        match failure {
            "shared" => {
                fs::write(fixture.root.join("src-tauri/shared/src/lib.rs"), b"changed").unwrap()
            }
            "checksum" => manifest["artifacts"][0]["sha256"] = "wrong".into(),
            "target" => manifest["artifacts"][0]["target"] = "foreign-target".into(),
            "version" => manifest["version"] = "stale".into(),
            _ => unreachable!(),
        }
        fs::write(fixture.manifest(), serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            std::panic::catch_unwind(|| fixture.emit()).is_err(),
            "accepted {failure}"
        );
    }
}
