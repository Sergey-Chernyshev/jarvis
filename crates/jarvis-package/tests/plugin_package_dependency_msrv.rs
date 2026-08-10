use std::io::{Read, Seek, SeekFrom, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use caseless::Caseless as _;
use rustix::fs::{
    fchmod, fstat, fsync, mkdirat, openat, statat, unlinkat, AtFlags, FileType, Mode, OFlags, CWD,
};
use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization as _;

#[test]
fn approved_package_dependency_apis_execute_on_declared_msrv() {
    let canonical_json = canonical_package_json();

    let mut hasher = Sha256::new();
    hasher.update(&canonical_json);
    let digest = hasher.finalize();
    assert_eq!(digest.len(), 32);

    let encoded = STANDARD.encode(&canonical_json);
    assert_eq!(STANDARD.decode(encoded).unwrap(), canonical_json);

    let archive = deterministic_tar_stream(&canonical_json);
    let mut spool = tempfile::tempfile().unwrap();
    spool.write_all(&archive).unwrap();
    spool.seek(SeekFrom::Start(0)).unwrap();
    let mut spooled_archive = Vec::new();
    spool.read_to_end(&mut spooled_archive).unwrap();
    assert_eq!(spooled_archive, archive);

    exercise_fd_relative_filesystem();

    let collision_key: String = "É".nfd().default_case_fold().nfd().collect();
    assert_eq!(collision_key, "e\u{301}");
    assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0));
    assert_eq!(caseless::UNICODE_VERSION, (16, 0, 0));
}

fn canonical_package_json() -> Vec<u8> {
    let value = serde_json::json!({
        "schemaVersion": 1,
        "publisher": "dev.jarvis",
    });
    let canonical = serde_json_canonicalizer::to_vec(&value).unwrap();
    assert_eq!(
        canonical,
        r#"{"publisher":"dev.jarvis","schemaVersion":1}"#.as_bytes()
    );
    canonical
}

fn deterministic_tar_stream(payload: &[u8]) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_path("package.json").unwrap();
    header.set_size(payload.len() as u64);
    header.set_mode(0o444);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();

    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        builder.append(&header, payload).unwrap();
        builder.finish().unwrap();
    }
    assert_eq!(archive.len() % 512, 0);
    archive
}

fn exercise_fd_relative_filesystem() {
    let scratch = tempfile::tempdir().unwrap();
    let root = openat(
        CWD,
        scratch.path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();

    mkdirat(&root, "quarantine", Mode::from_raw_mode(0o700)).unwrap();
    let quarantine = openat(
        &root,
        "quarantine",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let payload_fd = openat(
        &quarantine,
        "payload",
        OFlags::WRONLY
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .unwrap();
    let mut payload = std::fs::File::from(payload_fd);
    payload.write_all(b"jarvis-plugin").unwrap();
    fsync(&payload).unwrap();
    fchmod(&payload, Mode::from_raw_mode(0o444)).unwrap();

    let path_stat = statat(&quarantine, "payload", AtFlags::SYMLINK_NOFOLLOW).unwrap();
    let fd_stat = fstat(&payload).unwrap();
    assert_eq!(FileType::from_raw_mode(path_stat.st_mode), FileType::RegularFile);
    assert_eq!(path_stat.st_dev, fd_stat.st_dev);
    assert_eq!(path_stat.st_ino, fd_stat.st_ino);
    assert_eq!(path_stat.st_size, b"jarvis-plugin".len() as i64);
    assert_eq!(Mode::from_raw_mode(path_stat.st_mode).as_raw_mode(), 0o444);

    drop(payload);
    unlinkat(&quarantine, "payload", AtFlags::empty()).unwrap();
    drop(quarantine);
    unlinkat(&root, "quarantine", AtFlags::REMOVEDIR).unwrap();
}
