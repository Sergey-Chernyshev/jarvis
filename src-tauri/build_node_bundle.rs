//! Embed verified server binaries from this exact source tree into the app.
//! `npm run prepare:node` produces the manifest locally; CI supplies the same
//! files as build artifacts before packaging the desktop application.

use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};

const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

pub fn embed() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest_dir.parent().unwrap();
    let bundle = manifest_dir.join("node-binaries");
    println!("cargo:rerun-if-changed={}", bundle.display());
    println!("cargo:rerun-if-changed=build_node_bundle.rs");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("jarvis_node_bundle.rs");
    let manifest_path = bundle.join("manifest.json");
    if !manifest_path.is_file() {
        // Ordinary cargo check/test remains usable without cross-compilers.
        // Tauri's beforeBuildCommand requires the bundle for distributable apps.
        fs::write(output, "pub const BINARIES: &[(&str, &[u8])] = &[];\n").unwrap();
        return;
    }
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap())
        .expect("invalid node-binaries/manifest.json; run npm run prepare:node");
    assert_eq!(manifest["version"].as_str(), Some(env!("CARGO_PKG_VERSION")), "Node bundle version is stale; run npm run prepare:node");
    let mut sources = vec![
        root.join("src-tauri/node/Cargo.toml"),
        root.join("src-tauri/src/codex_hooks.rs"),
        root.join("src-tauri/shared/terminal_stream.rs"),
        root.join("src-tauri/Cargo.lock"),
    ];
    rust_sources(&manifest_dir.join("node/src"), &mut sources);
    sources.sort();
    let mut fingerprint = 0xcbf29ce484222325u64;
    for source in sources {
        println!("cargo:rerun-if-changed={}", source.display());
        let name = source.strip_prefix(root).unwrap().to_str().unwrap();
        for byte in name.bytes().chain([0]).chain(fs::read(&source).unwrap()).chain([0]) {
            fingerprint ^= u64::from(byte);
            fingerprint = fingerprint.wrapping_mul(0x100000001b3);
        }
    }
    assert_eq!(manifest["sourceFingerprint"].as_str(), Some(format!("{fingerprint:016x}").as_str()), "Node sources changed; run npm run prepare:node before rebuilding Jarvis");
    let artifacts = manifest["artifacts"].as_array().expect("node bundle artifacts missing");
    let mut generated = String::from("pub const BINARIES: &[(&str, &[u8])] = &[\n");
    for target in TARGETS {
        let matches: Vec<_> = artifacts.iter().filter(|a| a["target"].as_str() == Some(target)).collect();
        assert_eq!(matches.len(), 1, "Node bundle must contain exactly one {target}");
        let artifact = matches[0];
        let name = format!("jarvis-node-{target}");
        assert_eq!(artifact["file"].as_str(), Some(name.as_str()), "Unexpected node artifact path");
        let path = bundle.join(name);
        let bytes = fs::read(&path).expect("node binary is missing; run npm run prepare:node");
        assert_eq!(artifact["sha256"].as_str(), Some(format!("{:x}", Sha256::digest(&bytes)).as_str()), "Node binary checksum mismatch");
        let machine = if target.starts_with("x86_64") { 62 } else { 183 };
        assert!(bytes.len() > 20 && &bytes[..4] == b"\x7fELF" && bytes[4] == 2 && bytes[5] == 1 && u16::from_le_bytes([bytes[18], bytes[19]]) == machine, "Wrong node binary architecture: {target}");
        generated.push_str(&format!("({target:?}, include_bytes!({:?})),\n", path.to_str().unwrap()));
    }
    generated.push_str("];\n");
    fs::write(output, generated).unwrap();
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() { rust_sources(&path, out); }
        else if path.extension().and_then(|s| s.to_str()) == Some("rs") { out.push(path); }
    }
}
