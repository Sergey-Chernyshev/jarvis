use std::io::{Read, Seek, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use caseless::default_case_fold_str;
use rustix::fs::{open, openat, statat, AtFlags, Mode, OFlags};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization;

#[test]
fn exact_dependency_apis_execute() {
    let plugin_id =
        jarvis_plugin_protocol::manifest::PluginId::new("dev.example.package-probe").unwrap();
    assert_eq!(plugin_id.as_str(), "dev.example.package-probe");

    let encoded = STANDARD.encode(b"jarvis-plugin");
    assert_eq!(STANDARD.decode(encoded).unwrap(), b"jarvis-plugin");
    assert_eq!(default_case_fold_str("Jarvis"), "jarvis");

    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).unwrap();

    let canonical = serde_json_canonicalizer::to_vec(&json!({"z": 2, "a": 1})).unwrap();
    assert_eq!(canonical, br#"{"a":1,"z":2}"#);

    let digest = Sha256::digest(b"jarvis-plugin");
    assert_eq!(digest.len(), 32);

    let mut header = tar::Header::new_gnu();
    header.set_path("payload/probe").unwrap();
    header.set_mode(0o444);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(b"jarvis-plugin".len() as u64);
    header.set_cksum();
    let mut archive = tar::Builder::new(Vec::new());
    archive.append(&header, &b"jarvis-plugin"[..]).unwrap();
    archive.finish().unwrap();
    assert!(!archive.into_inner().unwrap().is_empty());

    let mut spool = tempfile::tempfile().unwrap();
    spool.write_all(b"jarvis-plugin").unwrap();
    spool.rewind().unwrap();
    let mut spooled = Vec::new();
    spool.read_to_end(&mut spooled).unwrap();
    assert_eq!(spooled, b"jarvis-plugin");

    let source = tempfile::tempdir().unwrap();
    let directory = open(
        source.path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let probe = openat(
        &directory,
        "probe",
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .unwrap();
    assert_eq!(rustix::io::write(&probe, b"jarvis-plugin").unwrap(), 13);
    let path_stat = statat(&directory, "probe", AtFlags::SYMLINK_NOFOLLOW).unwrap();
    assert_eq!(path_stat.st_size, b"jarvis-plugin".len() as libc::off_t,);

    let normalized: String = "e\u{301}".nfc().collect();
    assert_eq!(normalized, "é");

    #[cfg(target_os = "macos")]
    {
        let names = crate::macos_dir::read_directory_names(&directory).unwrap();
        assert!(names.iter().any(|name| name == "probe"));
    }
}
