use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use jarvis_plugin_protocol::json::JsonLimits;
use jarvis_plugin_protocol::manifest::{Digest, ManifestV2, RuntimeKind};
use jarvis_plugin_protocol::package::{
    PackageFileMode, PackageMetadataV1, PackagePath, PackageSignatureV1, PACKAGE_SCHEMA_VERSION,
};
use rustix::fs::{
    fchmod, fcntl_fullfsync, fstat, fsync, mkdirat, openat, statat, unlinkat, AtFlags, FileType,
    Mode, OFlags, Stat,
};
use sha2::{Digest as _, Sha256};

use crate::archive::{
    inspect_reader_with_limits, ArchiveInspection, ArchiveLimits, ObservedArchiveEntry,
};
use crate::hash::{merkle_root, sha256_digest};
use crate::jcs::parse_exact_jcs;
use crate::pack::SIGNATURE_MESSAGE_DOMAIN;
use crate::{PackageDocumentAdapter, PackageError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageTrustError {
    code: &'static str,
}

impl PackageTrustError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PackageTrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for PackageTrustError {}

pub trait PackageTrustVerifier {
    fn verify(
        &self,
        observation: &UntrustedPackageObservation<'_>,
    ) -> Result<(), PackageTrustError>;
}

pub struct UntrustedPackageObservation<'a> {
    package_json: &'a [u8],
    signature_bytes: &'a [u8],
    archive_digest: &'a Digest,
    metadata: &'a PackageMetadataV1,
    signature: &'a PackageSignatureV1,
    signature_message: &'a [u8],
}

impl UntrustedPackageObservation<'_> {
    pub fn package_json(&self) -> &[u8] {
        self.package_json
    }

    pub fn signature_bytes(&self) -> &[u8] {
        self.signature_bytes
    }

    pub fn archive_digest(&self) -> &Digest {
        self.archive_digest
    }

    pub fn metadata(&self) -> &PackageMetadataV1 {
        self.metadata
    }

    pub fn signature(&self) -> &PackageSignatureV1 {
        self.signature
    }

    pub fn signature_message(&self) -> &[u8] {
        self.signature_message
    }
}

/// Opaque proof that the exact held archive file passed strict inspection and
/// the caller-provided trust verifier.
///
/// ```compile_fail
/// use jarvis_package::VerifiedPackageEvidence;
///
/// let forged = VerifiedPackageEvidence {};
/// ```
pub struct VerifiedPackageEvidence {
    archive: File,
    inspection: ArchiveInspection,
    identity: FileIdentity,
    metadata: PackageMetadataV1,
    signature: PackageSignatureV1,
    manifest: ManifestV2,
}

impl fmt::Debug for VerifiedPackageEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPackageEvidence")
            .field("archive_digest", self.inspection.physical_digest())
            .field("plugin_id", &self.metadata.plugin_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedPackage {
    name: String,
    device: libc::dev_t,
    inode: libc::ino_t,
}

impl ExtractedPackage {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn device(&self) -> libc::dev_t {
        self.device
    }

    pub fn inode(&self) -> libc::ino_t {
        self.inode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    object: ObjectIdentity,
    size: libc::off_t,
}

impl FileIdentity {
    fn from_stat(stat: &Stat) -> Self {
        Self {
            object: ObjectIdentity::from_stat(stat),
            size: stat.st_size,
        }
    }

    fn archive(file: &File) -> Result<Self, PackageError> {
        let identity =
            Self::from_stat(&fstat(file).map_err(|_| PackageError::archive_truncated())?);
        if identity.object.file_type != FileType::RegularFile || identity.size < 0 {
            return Err(PackageError::archive_header());
        }
        Ok(identity)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
    file_type: FileType,
}

impl ObjectIdentity {
    fn from_stat(stat: &Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            file_type: FileType::from_raw_mode(stat.st_mode),
        }
    }
}

pub fn inspect_and_verify_package<A, V>(
    mut archive: File,
    adapter: &A,
    verifier: &V,
) -> Result<VerifiedPackageEvidence, PackageError>
where
    A: PackageDocumentAdapter,
    V: PackageTrustVerifier,
{
    let identity = FileIdentity::archive(&archive)?;
    archive
        .seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::archive_truncated())?;
    let limits = ArchiveLimits::production();
    let inspection = inspect_reader_with_limits(&mut archive, limits)?;
    if FileIdentity::archive(&archive)? != identity
        || u64::try_from(identity.size).map_err(|_| PackageError::archive_header())?
            != inspection.physical_bytes()
    {
        return Err(PackageError::archive_changed_after_verification());
    }

    let (metadata, signature, manifest, signature_message) =
        validate_inspection_documents(&inspection, adapter, limits)?;
    let observation = UntrustedPackageObservation {
        package_json: inspection.package_json(),
        signature_bytes: inspection.signature(),
        archive_digest: inspection.physical_digest(),
        metadata: &metadata,
        signature: &signature,
        signature_message: &signature_message,
    };
    verifier
        .verify(&observation)
        .map_err(|_| PackageError::package_trust())?;
    if FileIdentity::archive(&archive)? != identity {
        return Err(PackageError::archive_changed_after_verification());
    }
    Ok(VerifiedPackageEvidence {
        archive,
        inspection,
        identity,
        metadata,
        signature,
        manifest,
    })
}

fn validate_inspection_documents<A: PackageDocumentAdapter>(
    inspection: &ArchiveInspection,
    adapter: &A,
    limits: ArchiveLimits,
) -> Result<(PackageMetadataV1, PackageSignatureV1, ManifestV2, Vec<u8>), PackageError> {
    let json_limits = JsonLimits {
        max_bytes: usize::try_from(limits.max_package_json_bytes)
            .map_err(|_| PackageError::archive_quota())?,
        max_depth: limits.max_json_depth,
        max_nodes: limits.max_json_nodes,
        max_string_bytes: limits.max_json_string_bytes,
    };
    let metadata_value = parse_exact_jcs(inspection.package_json(), json_limits)
        .map_err(|_| PackageError::package_metadata())?;
    adapter.validate_package_metadata_schema(inspection.package_json())?;
    let metadata: PackageMetadataV1 =
        serde_json::from_value(metadata_value).map_err(|_| PackageError::package_metadata())?;
    if metadata.schema_version != PACKAGE_SCHEMA_VERSION {
        return Err(PackageError::package_metadata());
    }

    let signature_limits = JsonLimits {
        max_bytes: usize::try_from(limits.max_signature_bytes)
            .map_err(|_| PackageError::archive_quota())?,
        max_depth: 8,
        max_nodes: 16,
        max_string_bytes: usize::try_from(limits.max_signature_bytes)
            .map_err(|_| PackageError::archive_quota())?,
    };
    let signature_value = parse_exact_jcs(inspection.signature(), signature_limits)
        .map_err(|_| PackageError::package_metadata())?;
    adapter.validate_package_signature_schema(inspection.signature())?;
    let signature: PackageSignatureV1 =
        serde_json::from_value(signature_value).map_err(|_| PackageError::package_metadata())?;
    signature
        .validate()
        .map_err(|_| PackageError::package_metadata())?;

    let manifest_limits = JsonLimits {
        max_bytes: usize::try_from(limits.max_plugin_json_bytes)
            .map_err(|_| PackageError::archive_quota())?,
        max_depth: limits.max_json_depth,
        max_nodes: limits.max_json_nodes,
        max_string_bytes: limits.max_json_string_bytes,
    };
    parse_exact_jcs(inspection.plugin_json(), manifest_limits)
        .map_err(|_| PackageError::package_metadata())?;
    let manifest = adapter.validate_packaged_manifest(inspection.plugin_json(), metadata.target)?;
    cross_check_metadata(inspection, &metadata, &manifest)?;

    let mut signature_message =
        Vec::with_capacity(SIGNATURE_MESSAGE_DOMAIN.len() + 1 + inspection.package_json().len());
    signature_message.extend_from_slice(SIGNATURE_MESSAGE_DOMAIN);
    signature_message.push(0);
    signature_message.extend_from_slice(inspection.package_json());
    Ok((metadata, signature, manifest, signature_message))
}

fn cross_check_metadata(
    inspection: &ArchiveInspection,
    metadata: &PackageMetadataV1,
    manifest: &ManifestV2,
) -> Result<(), PackageError> {
    let observed = inspection.payload_entries();
    if observed.len() != metadata.files.len()
        || metadata.files.first().map(|file| file.path.as_str()) != Some("plugin.json")
        || observed
            .iter()
            .zip(&metadata.files)
            .any(|(observed, expected)| {
                observed.path() != &expected.path
                    || observed.mode() != expected.mode
                    || observed.size() != expected.size
                    || observed.digest() != &expected.digest
            })
        || metadata.manifest_digest != sha256_digest(inspection.plugin_json())
        || metadata.payload_root
            != merkle_root(&metadata.files).map_err(|_| PackageError::package_metadata())?
        || metadata.plugin_id != manifest.id
        || metadata.publisher != manifest.publisher
        || metadata.version != manifest.version
        || metadata.jarvis_range != manifest.compatibility.jarvis
        || metadata.plugin_api != manifest.compatibility.plugin_api
        || metadata.state != manifest.state
    {
        return Err(PackageError::package_metadata());
    }

    let mut expected_executable = BTreeSet::new();
    match manifest.runtime.kind {
        RuntimeKind::UiOnly => {}
        RuntimeKind::VerifiedNative => {
            let bridge = manifest
                .runtime
                .bridge_entry
                .as_ref()
                .ok_or_else(PackageError::package_metadata)?;
            let service = manifest
                .runtime
                .service
                .as_ref()
                .ok_or_else(PackageError::package_metadata)?;
            expected_executable.insert(bridge.as_str());
            expected_executable.insert(service.entry.as_str());
        }
    }
    let actual_executable = metadata
        .files
        .iter()
        .filter(|file| file.mode == PackageFileMode::Executable)
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    if actual_executable != expected_executable {
        return Err(PackageError::package_metadata());
    }
    Ok(())
}

pub fn extract_verified_package(
    evidence: VerifiedPackageEvidence,
    parent: &OwnedFd,
    quarantine_name: &str,
) -> Result<ExtractedPackage, PackageError> {
    extract_verified_package_with_hook(evidence, parent, quarantine_name, &NoopExtractionHook)
}

pub(crate) trait ExtractionHook {
    fn after_root_created(&self) {}
    fn fail_write_after(&self, _path: &str, _copied: u64) -> bool {
        false
    }
    fn mutate_chunk(&self, _path: &str, _bytes: &mut [u8]) {}
    fn fail_sync(&self, _path: &str) -> bool {
        false
    }
    fn before_cleanup(&self, _path: &str) {}
}

struct NoopExtractionHook;

impl ExtractionHook for NoopExtractionHook {}

pub(crate) fn extract_verified_package_with_hook<H: ExtractionHook>(
    mut evidence: VerifiedPackageEvidence,
    parent: &OwnedFd,
    quarantine_name: &str,
    hook: &H,
) -> Result<ExtractedPackage, PackageError> {
    validate_quarantine_name(quarantine_name)?;
    if evidence.metadata.plugin_id != evidence.manifest.id || evidence.signature.validate().is_err()
    {
        return Err(PackageError::package_metadata());
    }
    evidence
        .archive
        .seek(SeekFrom::Start(0))
        .map_err(|_| PackageError::archive_changed_after_verification())?;
    let second = inspect_reader_with_limits(&mut evidence.archive, ArchiveLimits::production())
        .map_err(|_| PackageError::archive_changed_after_verification())?;
    if second != evidence.inspection
        || FileIdentity::archive(&evidence.archive)? != evidence.identity
    {
        return Err(PackageError::archive_changed_after_verification());
    }

    let mut state = ExtractionState::create(parent, quarantine_name)?;
    hook.after_root_created();
    let result = (|| {
        extract_payload(&mut evidence.archive, &second, &mut state, hook)?;
        sync_directories(&state, hook)?;
        evidence
            .archive
            .seek(SeekFrom::Start(0))
            .map_err(|_| PackageError::archive_changed_after_verification())?;
        let final_inspection =
            inspect_reader_with_limits(&mut evidence.archive, ArchiveLimits::production())
                .map_err(|_| PackageError::archive_changed_after_verification())?;
        if final_inspection != evidence.inspection
            || FileIdentity::archive(&evidence.archive)? != evidence.identity
        {
            return Err(PackageError::archive_changed_after_verification());
        }
        Ok(())
    })();
    if let Err(error) = result {
        return if cleanup_quarantine(parent, quarantine_name, &state, hook).is_ok() {
            Err(error)
        } else {
            Err(PackageError::quarantine_manual_cleanup())
        };
    }
    Ok(ExtractedPackage {
        name: quarantine_name.to_owned(),
        device: state.root_identity.device,
        inode: state.root_identity.inode,
    })
}

fn validate_quarantine_name(name: &str) -> Result<(), PackageError> {
    let path = PackagePath::new(name).map_err(|_| PackageError::extraction_failed())?;
    if path.as_str() != name || name.contains('/') {
        return Err(PackageError::extraction_failed());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreatedKind {
    Directory,
    File,
}

#[derive(Clone, Debug)]
struct CreatedRecord {
    path: String,
    kind: CreatedKind,
    identity: ObjectIdentity,
}

struct ExtractionState {
    root: OwnedFd,
    root_identity: ObjectIdentity,
    root_owner: libc::uid_t,
    directories: BTreeMap<String, ObjectIdentity>,
    created: Vec<CreatedRecord>,
}

impl ExtractionState {
    fn create(parent: &OwnedFd, name: &str) -> Result<Self, PackageError> {
        let parent_stat = fstat(parent).map_err(|_| PackageError::extraction_failed())?;
        if FileType::from_raw_mode(parent_stat.st_mode) != FileType::Directory {
            return Err(PackageError::extraction_failed());
        }
        mkdirat(parent, name, Mode::from_raw_mode(0o700))
            .map_err(|_| PackageError::extraction_failed())?;
        let root = openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| PackageError::quarantine_manual_cleanup())?;
        let stat = fstat(&root).map_err(|_| PackageError::quarantine_manual_cleanup())?;
        let identity = ObjectIdentity::from_stat(&stat);
        if identity.file_type != FileType::Directory
            || stat.st_uid != parent_stat.st_uid
            || stat.st_mode & 0o777 != 0o700
        {
            return Err(PackageError::quarantine_manual_cleanup());
        }
        Ok(Self {
            root,
            root_identity: identity,
            root_owner: stat.st_uid,
            directories: BTreeMap::new(),
            created: Vec::new(),
        })
    }
}

fn extract_payload<H: ExtractionHook>(
    archive: &mut File,
    inspection: &ArchiveInspection,
    state: &mut ExtractionState,
    hook: &H,
) -> Result<(), PackageError> {
    for directory in inspection.validated_directories() {
        create_directory(state, directory.as_str())?;
    }
    for entry in inspection.payload_entries() {
        create_file_from_entry(archive, state, entry, hook)?;
    }
    Ok(())
}

fn create_directory(state: &mut ExtractionState, path: &str) -> Result<(), PackageError> {
    let (parent_path, name) = split_parent(path)?;
    let parent = open_created_directory(&state.root, parent_path, &state.directories)?;
    mkdirat(&parent, name, Mode::from_raw_mode(0o700))
        .map_err(|_| PackageError::extraction_failed())?;
    let directory = openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| PackageError::extraction_failed())?;
    let stat = fstat(&directory).map_err(|_| PackageError::extraction_failed())?;
    let identity = ObjectIdentity::from_stat(&stat);
    if identity.file_type != FileType::Directory
        || stat.st_uid != state.root_owner
        || stat.st_mode & 0o777 != 0o700
    {
        return Err(PackageError::extraction_failed());
    }
    state.directories.insert(path.to_owned(), identity);
    state.created.push(CreatedRecord {
        path: path.to_owned(),
        kind: CreatedKind::Directory,
        identity,
    });
    Ok(())
}

fn create_file_from_entry<H: ExtractionHook>(
    archive: &mut File,
    state: &mut ExtractionState,
    entry: &ObservedArchiveEntry,
    hook: &H,
) -> Result<(), PackageError> {
    let path = entry.path().as_str();
    let (parent_path, name) = split_parent(path)?;
    let parent = open_created_directory(&state.root, parent_path, &state.directories)?;
    let descriptor = openat(
        &parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| PackageError::extraction_failed())?;
    let mut output = File::from(descriptor);
    let created_stat = fstat(&output).map_err(|_| PackageError::extraction_failed())?;
    let created_identity = ObjectIdentity::from_stat(&created_stat);
    if created_identity.file_type != FileType::RegularFile
        || created_stat.st_nlink != 1
        || created_stat.st_uid != state.root_owner
        || created_stat.st_mode & 0o777 != 0o600
    {
        return Err(PackageError::extraction_failed());
    }
    state.created.push(CreatedRecord {
        path: path.to_owned(),
        kind: CreatedKind::File,
        identity: created_identity,
    });

    let mut remaining = entry.size();
    let mut archive_offset = entry.body_offset();
    let mut copied = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let chunk = usize::try_from(
            remaining.min(u64::try_from(buffer.len()).map_err(|_| PackageError::archive_quota())?),
        )
        .map_err(|_| PackageError::archive_quota())?;
        read_exact_at(archive, &mut buffer[..chunk], archive_offset)?;
        hook.mutate_chunk(path, &mut buffer[..chunk]);
        output
            .write_all(&buffer[..chunk])
            .map_err(|_| PackageError::extraction_failed())?;
        hasher.update(&buffer[..chunk]);
        let chunk_u64 = u64::try_from(chunk).map_err(|_| PackageError::archive_quota())?;
        archive_offset = archive_offset
            .checked_add(chunk_u64)
            .ok_or_else(PackageError::archive_quota)?;
        copied = copied
            .checked_add(chunk_u64)
            .ok_or_else(PackageError::archive_quota)?;
        remaining -= chunk_u64;
        if hook.fail_write_after(path, copied) {
            return Err(PackageError::extraction_failed());
        }
    }
    let digest = digest_from_bytes(hasher.finalize().into());
    if digest != *entry.digest() {
        return Err(PackageError::package_metadata());
    }
    let final_mode = libc::mode_t::try_from(entry.mode().as_octal())
        .map_err(|_| PackageError::extraction_failed())?;
    fchmod(&output, Mode::from_raw_mode(final_mode))
        .map_err(|_| PackageError::extraction_failed())?;
    if hook.fail_sync(path) {
        return Err(PackageError::extraction_failed());
    }
    fsync(&output).map_err(|_| PackageError::extraction_failed())?;
    fcntl_fullfsync(&output).map_err(|_| PackageError::extraction_failed())?;
    let final_stat = fstat(&output).map_err(|_| PackageError::extraction_failed())?;
    if ObjectIdentity::from_stat(&final_stat) != created_identity
        || final_stat.st_nlink != 1
        || final_stat.st_size < 0
        || u64::try_from(final_stat.st_size).map_err(|_| PackageError::extraction_failed())?
            != entry.size()
        || u32::from(final_stat.st_mode) & 0o777 != entry.mode().as_octal()
    {
        return Err(PackageError::extraction_failed());
    }
    Ok(())
}

fn read_exact_at(file: &File, mut output: &mut [u8], mut offset: u64) -> Result<(), PackageError> {
    while !output.is_empty() {
        let read = file
            .read_at(output, offset)
            .map_err(|_| PackageError::archive_changed_after_verification())?;
        if read == 0 {
            return Err(PackageError::archive_changed_after_verification());
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| PackageError::archive_quota())?)
            .ok_or_else(PackageError::archive_quota)?;
        output = &mut output[read..];
    }
    Ok(())
}

fn sync_directories<H: ExtractionHook>(
    state: &ExtractionState,
    hook: &H,
) -> Result<(), PackageError> {
    let mut paths = state.directories.keys().cloned().collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        right
            .matches('/')
            .count()
            .cmp(&left.matches('/').count())
            .then_with(|| right.cmp(left))
    });
    for path in paths {
        if hook.fail_sync(&path) {
            return Err(PackageError::extraction_failed());
        }
        let directory = open_created_directory(&state.root, &path, &state.directories)?;
        fsync(&directory).map_err(|_| PackageError::extraction_failed())?;
    }
    if hook.fail_sync("") {
        return Err(PackageError::extraction_failed());
    }
    fsync(&state.root).map_err(|_| PackageError::extraction_failed())?;
    Ok(())
}

fn open_created_directory(
    root: &OwnedFd,
    path: &str,
    directories: &BTreeMap<String, ObjectIdentity>,
) -> Result<OwnedFd, PackageError> {
    let mut current =
        rustix::io::fcntl_dupfd_cloexec(root, 0).map_err(|_| PackageError::extraction_failed())?;
    if path.is_empty() {
        return Ok(current);
    }
    let mut prefix = String::new();
    for component in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let expected = directories
            .get(&prefix)
            .ok_or_else(PackageError::extraction_failed)?;
        let next = openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| PackageError::extraction_failed())?;
        if ObjectIdentity::from_stat(&fstat(&next).map_err(|_| PackageError::extraction_failed())?)
            != *expected
        {
            return Err(PackageError::extraction_failed());
        }
        current = next;
    }
    Ok(current)
}

fn cleanup_quarantine<H: ExtractionHook>(
    parent: &OwnedFd,
    quarantine_name: &str,
    state: &ExtractionState,
    hook: &H,
) -> Result<(), PackageError> {
    for record in state.created.iter().rev() {
        hook.before_cleanup(&record.path);
        let (parent_path, name) = split_parent(&record.path)?;
        let parent_fd = open_created_directory(&state.root, parent_path, &state.directories)
            .map_err(|_| PackageError::quarantine_manual_cleanup())?;
        let observed = statat(&parent_fd, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| PackageError::quarantine_manual_cleanup())?;
        if ObjectIdentity::from_stat(&observed) != record.identity {
            return Err(PackageError::quarantine_manual_cleanup());
        }
        let flags = if record.kind == CreatedKind::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        unlinkat(&parent_fd, name, flags).map_err(|_| PackageError::quarantine_manual_cleanup())?;
    }
    let observed = statat(parent, quarantine_name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| PackageError::quarantine_manual_cleanup())?;
    if ObjectIdentity::from_stat(&observed) != state.root_identity {
        return Err(PackageError::quarantine_manual_cleanup());
    }
    unlinkat(parent, quarantine_name, AtFlags::REMOVEDIR)
        .map_err(|_| PackageError::quarantine_manual_cleanup())
}

fn split_parent(path: &str) -> Result<(&str, &str), PackageError> {
    match path.rsplit_once('/') {
        Some((parent, name)) if !parent.is_empty() && !name.is_empty() => Ok((parent, name)),
        None if !path.is_empty() => Ok(("", path)),
        _ => Err(PackageError::extraction_failed()),
    }
}

fn digest_from_bytes(bytes: [u8; 32]) -> Digest {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Digest::new(value).expect("lowercase SHA-256 bytes always form a valid digest")
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{symlink, MetadataExt};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use jarvis_plugin_protocol::manifest::ManifestV2;
    use jarvis_plugin_protocol::package::PackageTarget;

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
        signature_message: Vec<u8>,
    }

    impl ExactVerifier {
        fn from_golden() -> Self {
            let mut bytes = GOLDEN;
            let inspection =
                inspect_reader_with_limits(&mut bytes, ArchiveLimits::production()).unwrap();
            Self {
                signature_message: [
                    crate::pack::SIGNATURE_MESSAGE_DOMAIN,
                    b"\0",
                    inspection.package_json(),
                ]
                .concat(),
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
                && observation.signature_message() == self.signature_message
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
            "fixture_mismatch"
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
            "catalog_digest_mismatch"
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
        struct PostPassMutation {
            archive: File,
        }

        impl ExtractionHook for PostPassMutation {
            fn after_root_created(&self) {
                self.archive
                    .write_all_at(b"q", signature_body_offset())
                    .unwrap();
            }
        }

        let file = archive_file();
        let mutation = PostPassMutation {
            archive: file.try_clone().unwrap(),
        };
        let evidence = verify_fixture(file);
        let parent = tempfile::tempdir().unwrap();
        let parent_fd = held_directory(parent.path());
        assert_eq!(
            extract_verified_package_with_hook(evidence, &parent_fd, "quarantine", &mutation,)
                .unwrap_err()
                .code(),
            "archive_changed_after_verification"
        );
        assert!(!parent.path().join("quarantine").exists());
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
        fs::remove_dir_all(root).unwrap();
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
