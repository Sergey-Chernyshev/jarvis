use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use caseless::Caseless;
use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::package::PackageFileMode;
use jarvis_plugin_protocol::package::PackagePath;
use sha2::{Digest as _, Sha256};
use tar::{Builder, Header};
use unicode_normalization::UnicodeNormalization;

use crate::PackageError;

pub(crate) const BLOCK_SIZE: usize = 512;
const GNU_LONG_LINK: &[u8] = b"././@LongLink";
const GNU_LONG_FILE: &[u8] = b"././@LongFile";
const NAMESPACE_NODE_ALLOCATION_CHARGE: u64 = 256;

const REGULAR_TYPE: u8 = b'0';
const LONG_NAME_TYPE: u8 = b'L';

pub(crate) fn append_profile_entry<W: Write, R: Read>(
    builder: &mut Builder<W>,
    path: &PackagePath,
    mode: u32,
    size: u64,
    reader: R,
) -> Result<(), PackageError> {
    let path_bytes = path.as_str().as_bytes();
    if path_bytes.len() > 100 {
        let long_size = u64::try_from(path_bytes.len())
            .map_err(|_| PackageError::package_metadata())?
            .checked_add(1)
            .ok_or_else(PackageError::package_metadata)?;
        let long_header = build_header(GNU_LONG_LINK, 0o644, long_size, LONG_NAME_TYPE)?;
        let mut long_body = Vec::new();
        long_body
            .try_reserve_exact(
                usize::try_from(long_size).map_err(|_| PackageError::package_metadata())?,
            )
            .map_err(|_| PackageError::package_metadata())?;
        long_body.extend_from_slice(path_bytes);
        long_body.push(0);
        builder
            .append(&long_header, Cursor::new(long_body))
            .map_err(|_| PackageError::archive_write())?;
        let regular_header = build_header(GNU_LONG_FILE, mode, size, REGULAR_TYPE)?;
        builder
            .append(&regular_header, reader)
            .map_err(|_| PackageError::archive_write())?;
    } else {
        let regular_header = build_header(path_bytes, mode, size, REGULAR_TYPE)?;
        builder
            .append(&regular_header, reader)
            .map_err(|_| PackageError::archive_write())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn projected_entry_bytes(path: &PackagePath, size: u64) -> Result<u64, PackageError> {
    let regular = (BLOCK_SIZE as u64)
        .checked_add(padded_body_bytes(size)?)
        .ok_or_else(PackageError::archive_quota)?;
    if path.as_str().len() <= 100 {
        return Ok(regular);
    }
    let long_body = u64::try_from(path.as_str().len())
        .map_err(|_| PackageError::archive_quota())?
        .checked_add(1)
        .ok_or_else(PackageError::archive_quota)?;
    (BLOCK_SIZE as u64)
        .checked_add(padded_body_bytes(long_body)?)
        .and_then(|long| long.checked_add(regular))
        .ok_or_else(PackageError::archive_quota)
}

#[cfg(target_os = "macos")]
fn padded_body_bytes(size: u64) -> Result<u64, PackageError> {
    let block = BLOCK_SIZE as u64;
    let remainder = size % block;
    if remainder == 0 {
        return Ok(size);
    }
    size.checked_add(block - remainder)
        .ok_or_else(PackageError::archive_quota)
}

fn build_header(name: &[u8], mode: u32, size: u64, entry_type: u8) -> Result<Header, PackageError> {
    if name.is_empty()
        || name.len() > 100
        || !matches!(
            (entry_type, mode),
            (REGULAR_TYPE, 0o444 | 0o555) | (LONG_NAME_TYPE, 0o644)
        )
    {
        return Err(PackageError::package_metadata());
    }

    let mut header = Header::new_gnu();
    debug_assert_eq!(header.as_bytes().len(), BLOCK_SIZE);
    let bytes = header.as_mut_bytes();
    bytes.fill(0);
    bytes[..name.len()].copy_from_slice(name);
    encode_number(&mut bytes[100..108], u64::from(mode))?;
    encode_number(&mut bytes[108..116], 0)?;
    encode_number(&mut bytes[116..124], 0)?;
    encode_number(&mut bytes[124..136], size)?;
    encode_number(&mut bytes[136..148], 0)?;
    bytes[148..156].fill(b' ');
    bytes[156] = entry_type;
    bytes[257..263].copy_from_slice(b"ustar ");
    bytes[263..265].copy_from_slice(b" \0");
    encode_number(&mut bytes[329..337], 0)?;
    encode_number(&mut bytes[337..345], 0)?;
    let checksum = bytes
        .iter()
        .try_fold(0_u64, |sum, byte| sum.checked_add(u64::from(*byte)))
        .ok_or_else(PackageError::package_metadata)?;
    encode_checksum(&mut bytes[148..156], checksum)?;
    Ok(header)
}

fn encode_number(field: &mut [u8], value: u64) -> Result<(), PackageError> {
    let (digits, maximum) = match field.len() {
        8 => (7, 0o7_777_777),
        12 => (11, 0o77_777_777_777),
        _ => return Err(PackageError::package_metadata()),
    };
    if value > maximum {
        return Err(PackageError::package_metadata());
    }
    let encoded = format!("{value:0digits$o}");
    if encoded.len() != digits {
        return Err(PackageError::package_metadata());
    }
    field[..digits].copy_from_slice(encoded.as_bytes());
    field[digits] = 0;
    Ok(())
}

fn encode_checksum(field: &mut [u8], value: u64) -> Result<(), PackageError> {
    if field.len() != 8 || value > 0o777_777 {
        return Err(PackageError::package_metadata());
    }
    let encoded = format!("{value:06o}");
    if encoded.len() != 6 {
        return Err(PackageError::package_metadata());
    }
    field[..6].copy_from_slice(encoded.as_bytes());
    field[6] = 0;
    field[7] = b' ';
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveLimits {
    pub(crate) max_physical_bytes: u64,
    pub(crate) max_unpacked_payload_bytes: u64,
    pub(crate) max_single_payload_file: u64,
    pub(crate) max_payload_files: u64,
    pub(crate) max_logical_entries: u64,
    pub(crate) max_raw_records: u64,
    pub(crate) max_path_bytes: u64,
    pub(crate) max_component_bytes: u64,
    pub(crate) max_path_depth: u64,
    pub(crate) max_namespace_nodes: u64,
    pub(crate) max_namespace_stored_bytes: u64,
    pub(crate) max_collision_key_bytes: u64,
    pub(crate) max_long_name_body: u64,
    pub(crate) max_package_json_bytes: u64,
    pub(crate) max_signature_bytes: u64,
    pub(crate) max_plugin_json_bytes: u64,
    pub(crate) max_json_depth: usize,
    pub(crate) max_json_nodes: usize,
    pub(crate) max_json_string_bytes: usize,
}

impl ArchiveLimits {
    pub(crate) const fn production() -> Self {
        Self {
            max_physical_bytes: 2 * 1024 * 1024 * 1024,
            max_unpacked_payload_bytes: 2 * 1024 * 1024 * 1024,
            max_single_payload_file: 512 * 1024 * 1024,
            max_payload_files: 20_000,
            max_logical_entries: 20_002,
            max_raw_records: 40_002,
            max_path_bytes: 1_024,
            max_component_bytes: 255,
            max_path_depth: 64,
            max_namespace_nodes: 100_000,
            max_namespace_stored_bytes: 24 * 1024 * 1024,
            max_collision_key_bytes: 4_096,
            max_long_name_body: 1_025,
            max_package_json_bytes: 16 * 1024 * 1024,
            max_signature_bytes: 4 * 1024,
            max_plugin_json_bytes: 256 * 1024,
            max_json_depth: 64,
            max_json_nodes: 250_000,
            max_json_string_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedArchiveEntry {
    path: PackagePath,
    mode: PackageFileMode,
    size: u64,
    digest: Digest,
    body_offset: u64,
}

impl ObservedArchiveEntry {
    pub(crate) fn path(&self) -> &PackagePath {
        &self.path
    }

    pub(crate) fn mode(&self) -> PackageFileMode {
        self.mode
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn digest(&self) -> &Digest {
        &self.digest
    }

    pub(crate) fn body_offset(&self) -> u64 {
        self.body_offset
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveInspection {
    entries: Vec<ObservedArchiveEntry>,
    validated_directories: Vec<PackagePath>,
    plugin_json: Vec<u8>,
    package_json: Vec<u8>,
    signature: Vec<u8>,
    physical_digest: Digest,
    physical_bytes: u64,
}

impl ArchiveInspection {
    pub(crate) fn entries(&self) -> &[ObservedArchiveEntry] {
        &self.entries
    }

    pub(crate) fn payload_entries(&self) -> &[ObservedArchiveEntry] {
        let end = self.entries.len().saturating_sub(2);
        &self.entries[..end]
    }

    pub(crate) fn validated_directories(&self) -> &[PackagePath] {
        &self.validated_directories
    }

    pub(crate) fn plugin_json(&self) -> &[u8] {
        &self.plugin_json
    }

    pub(crate) fn package_json(&self) -> &[u8] {
        &self.package_json
    }

    pub(crate) fn signature(&self) -> &[u8] {
        &self.signature
    }

    pub(crate) fn physical_digest(&self) -> &Digest {
        &self.physical_digest
    }

    pub(crate) fn physical_bytes(&self) -> u64 {
        self.physical_bytes
    }

    #[cfg(test)]
    fn retained_body_bytes(&self) -> usize {
        self.plugin_json.len() + self.package_json.len() + self.signature.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchivePhase {
    Start,
    Payload,
    Package,
    Signature,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NamespaceKind {
    Directory,
    File,
}

#[derive(Clone, Debug)]
struct NamespaceNode {
    spelling: String,
    kind: NamespaceKind,
}

struct ParserContext {
    limits: ArchiveLimits,
    phase: ArchivePhase,
    previous_payload: Option<String>,
    exact_paths: BTreeSet<String>,
    namespace: BTreeMap<String, NamespaceNode>,
    namespace_nodes: u64,
    namespace_stored_bytes: u64,
    validated_directories: BTreeSet<String>,
    logical_entries: u64,
    payload_files: u64,
    unpacked_payload_bytes: u64,
    entries: Vec<ObservedArchiveEntry>,
    plugin_json: Vec<u8>,
    package_json: Vec<u8>,
    signature: Vec<u8>,
}

impl ParserContext {
    fn new(limits: ArchiveLimits) -> Result<Self, PackageError> {
        let mut context = Self {
            limits,
            phase: ArchivePhase::Start,
            previous_payload: None,
            exact_paths: BTreeSet::new(),
            namespace: BTreeMap::new(),
            namespace_nodes: 0,
            namespace_stored_bytes: 0,
            validated_directories: BTreeSet::new(),
            logical_entries: 0,
            payload_files: 0,
            unpacked_payload_bytes: 0,
            entries: Vec::new(),
            plugin_json: Vec::new(),
            package_json: Vec::new(),
            signature: Vec::new(),
        };
        context.insert_namespace_path("package.json", NamespaceKind::File)?;
        context.insert_namespace_path("SIGNATURE", NamespaceKind::File)?;
        Ok(context)
    }

    fn accept_entry(&mut self, path: &PackagePath, size: u64) -> Result<EntryClass, PackageError> {
        let path_text = path.as_str();
        validate_path_limits(path_text, self.limits)?;
        if !self.exact_paths.insert(path_text.to_owned()) {
            return Err(PackageError::archive_duplicate());
        }
        self.logical_entries = checked_increment(self.logical_entries)?;
        if self.logical_entries > self.limits.max_logical_entries {
            return Err(PackageError::archive_quota());
        }

        let entry_class = match self.phase {
            ArchivePhase::Start => {
                if path_text != "plugin.json" {
                    return Err(PackageError::archive_order());
                }
                self.insert_namespace_path(path_text, NamespaceKind::File)?;
                self.phase = ArchivePhase::Payload;
                EntryClass::Plugin
            }
            ArchivePhase::Payload if path_text == "package.json" => {
                self.phase = ArchivePhase::Package;
                EntryClass::Package
            }
            ArchivePhase::Payload if path_text == "SIGNATURE" => {
                return Err(PackageError::archive_order());
            }
            ArchivePhase::Payload => {
                if self
                    .previous_payload
                    .as_deref()
                    .map(|previous| path_text <= previous)
                    .unwrap_or(false)
                {
                    return Err(PackageError::archive_order());
                }
                self.insert_namespace_path(path_text, NamespaceKind::File)?;
                self.previous_payload = Some(path_text.to_owned());
                EntryClass::Payload
            }
            ArchivePhase::Package => {
                if path_text != "SIGNATURE" {
                    return Err(PackageError::archive_order());
                }
                self.phase = ArchivePhase::Signature;
                EntryClass::Signature
            }
            ArchivePhase::Signature => return Err(PackageError::archive_order()),
        };

        match entry_class {
            EntryClass::Plugin | EntryClass::Payload => {
                self.payload_files = checked_increment(self.payload_files)?;
                if self.payload_files > self.limits.max_payload_files
                    || size > self.limits.max_single_payload_file
                {
                    return Err(PackageError::archive_quota());
                }
                self.unpacked_payload_bytes = self
                    .unpacked_payload_bytes
                    .checked_add(size)
                    .ok_or_else(PackageError::archive_quota)?;
                if self.unpacked_payload_bytes > self.limits.max_unpacked_payload_bytes {
                    return Err(PackageError::archive_quota());
                }
                if entry_class == EntryClass::Plugin && size > self.limits.max_plugin_json_bytes {
                    return Err(PackageError::archive_quota());
                }
            }
            EntryClass::Package if size > self.limits.max_package_json_bytes => {
                return Err(PackageError::archive_quota());
            }
            EntryClass::Signature if size > self.limits.max_signature_bytes => {
                return Err(PackageError::archive_quota());
            }
            _ => {}
        }
        Ok(entry_class)
    }

    fn insert_namespace_path(
        &mut self,
        path: &str,
        final_kind: NamespaceKind,
    ) -> Result<(), PackageError> {
        let mut prefix = String::new();
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let kind = if components.peek().is_some() {
                NamespaceKind::Directory
            } else {
                final_kind
            };
            let key = collision_key(&prefix, self.limits.max_collision_key_bytes)?;
            if let Some(existing) = self.namespace.get(&key) {
                if existing.spelling != prefix || existing.kind != kind {
                    return Err(PackageError::archive_case_collision());
                }
                continue;
            }
            self.namespace_nodes = checked_increment(self.namespace_nodes)?;
            if self.namespace_nodes > self.limits.max_namespace_nodes {
                return Err(PackageError::archive_quota());
            }
            let directory_plan_bytes = if kind == NamespaceKind::Directory {
                u64::try_from(prefix.len()).map_err(|_| PackageError::archive_quota())?
            } else {
                0
            };
            let allocation_charge = u64::try_from(key.len())
                .map_err(|_| PackageError::archive_quota())?
                .checked_add(
                    u64::try_from(prefix.len()).map_err(|_| PackageError::archive_quota())?,
                )
                .and_then(|value| value.checked_add(directory_plan_bytes))
                .and_then(|value| value.checked_add(NAMESPACE_NODE_ALLOCATION_CHARGE))
                .ok_or_else(PackageError::archive_quota)?;
            self.namespace_stored_bytes = self
                .namespace_stored_bytes
                .checked_add(allocation_charge)
                .ok_or_else(PackageError::archive_quota)?;
            if self.namespace_stored_bytes > self.limits.max_namespace_stored_bytes {
                return Err(PackageError::archive_quota());
            }
            if kind == NamespaceKind::Directory {
                self.validated_directories.insert(prefix.clone());
            }
            self.namespace.insert(
                key,
                NamespaceNode {
                    spelling: prefix.clone(),
                    kind,
                },
            );
        }
        Ok(())
    }

    fn store_entry(
        &mut self,
        plan: RegularPlan,
        digest: Digest,
        retained: Vec<u8>,
    ) -> Result<(), PackageError> {
        match plan.entry_class {
            EntryClass::Plugin => self.plugin_json = retained,
            EntryClass::Package => self.package_json = retained,
            EntryClass::Signature => self.signature = retained,
            EntryClass::Payload => {
                if !retained.is_empty() {
                    return Err(PackageError::archive_header());
                }
            }
        }
        self.entries.push(ObservedArchiveEntry {
            path: plan.path,
            mode: plan.mode,
            size: plan.size,
            digest,
            body_offset: plan.body_offset,
        });
        Ok(())
    }

    fn finish(
        self,
        physical_digest: Digest,
        physical_bytes: u64,
    ) -> Result<ArchiveInspection, PackageError> {
        if self.phase != ArchivePhase::Signature {
            return Err(PackageError::archive_order());
        }
        let mut validated_directories = self
            .validated_directories
            .into_iter()
            .map(|path| PackagePath::new(path).map_err(|_| PackageError::archive_path()))
            .collect::<Result<Vec<_>, _>>()?;
        validated_directories.sort_by(|left, right| {
            left.as_str()
                .matches('/')
                .count()
                .cmp(&right.as_str().matches('/').count())
                .then_with(|| left.as_str().cmp(right.as_str()))
        });
        Ok(ArchiveInspection {
            entries: self.entries,
            validated_directories,
            plugin_json: self.plugin_json,
            package_json: self.package_json,
            signature: self.signature,
            physical_digest,
            physical_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryClass {
    Plugin,
    Payload,
    Package,
    Signature,
}

#[derive(Debug)]
struct RawHeader {
    name: [u8; 100],
    mode: u64,
    size: u64,
    entry_type: u8,
}

#[derive(Debug)]
struct RegularPlan {
    path: PackagePath,
    mode: PackageFileMode,
    size: u64,
    body_offset: u64,
    entry_class: EntryClass,
}

enum ParserState {
    ExpectHeader,
    ExpectLongNameBody { size: u64 },
    ExpectLongNameTarget { path: PackagePath },
    ReadRegularBody(RegularPlan),
    ExpectSecondZeroBlock,
    ExpectEof,
}

struct RawStream<'a, R> {
    reader: &'a mut R,
    hasher: Sha256,
    position: u64,
    maximum: u64,
}

impl<'a, R: Read> RawStream<'a, R> {
    fn new(reader: &'a mut R, maximum: u64) -> Self {
        Self {
            reader,
            hasher: Sha256::new(),
            position: 0,
            maximum,
        }
    }

    fn read_exact(&mut self, mut output: &mut [u8]) -> Result<(), PackageError> {
        let requested = u64::try_from(output.len()).map_err(|_| PackageError::archive_quota())?;
        let end = self
            .position
            .checked_add(requested)
            .ok_or_else(PackageError::archive_quota)?;
        if end > self.maximum {
            return Err(PackageError::archive_quota());
        }
        while !output.is_empty() {
            let read = self
                .reader
                .read(output)
                .map_err(|_| PackageError::archive_truncated())?;
            if read == 0 {
                return Err(PackageError::archive_truncated());
            }
            self.hasher.update(&output[..read]);
            self.position = self
                .position
                .checked_add(u64::try_from(read).map_err(|_| PackageError::archive_quota())?)
                .ok_or_else(PackageError::archive_quota)?;
            output = &mut output[read..];
        }
        Ok(())
    }

    fn prove_eof(&mut self) -> Result<(), PackageError> {
        let mut byte = [0_u8; 1];
        match self.reader.read(&mut byte) {
            Ok(0) => Ok(()),
            Ok(_) => {
                self.position = self
                    .position
                    .checked_add(1)
                    .ok_or_else(PackageError::archive_quota)?;
                if self.position > self.maximum {
                    Err(PackageError::archive_quota())
                } else {
                    Err(PackageError::archive_trailing())
                }
            }
            Err(_) => Err(PackageError::archive_trailing()),
        }
    }

    fn digest(self) -> Digest {
        digest_from_bytes(self.hasher.finalize().into())
    }
}

pub(crate) fn inspect_reader_with_limits<R: Read>(
    reader: &mut R,
    limits: ArchiveLimits,
) -> Result<ArchiveInspection, PackageError> {
    let mut raw = RawStream::new(reader, limits.max_physical_bytes);
    let mut context = ParserContext::new(limits)?;
    let mut raw_records = 0_u64;
    let mut state = ParserState::ExpectHeader;

    loop {
        state = match state {
            ParserState::ExpectHeader => {
                let mut block = [0_u8; BLOCK_SIZE];
                raw.read_exact(&mut block)?;
                if is_zero_block(&block) {
                    ParserState::ExpectSecondZeroBlock
                } else {
                    raw_records = increment_raw_record(raw_records, limits)?;
                    let header = parse_header(&block)?;
                    match header.entry_type {
                        LONG_NAME_TYPE => {
                            validate_long_header(&header)?;
                            ParserState::ExpectLongNameBody { size: header.size }
                        }
                        REGULAR_TYPE => {
                            let path = parse_short_path(&header.name, limits)?;
                            let plan = regular_plan(header, path, raw.position, &mut context)?;
                            ParserState::ReadRegularBody(plan)
                        }
                        _ => return Err(PackageError::archive_entry_type()),
                    }
                }
            }
            ParserState::ExpectLongNameBody { size } => {
                let path = read_long_name(&mut raw, size, limits)?;
                ParserState::ExpectLongNameTarget { path }
            }
            ParserState::ExpectLongNameTarget { path } => {
                let mut block = [0_u8; BLOCK_SIZE];
                raw.read_exact(&mut block)?;
                if is_zero_block(&block) {
                    return Err(PackageError::archive_entry_type());
                }
                raw_records = increment_raw_record(raw_records, limits)?;
                let header = parse_header(&block)?;
                if header.entry_type != REGULAR_TYPE
                    || parse_header_name(&header.name)? != GNU_LONG_FILE
                    || path.as_str() == "package.json"
                    || path.as_str() == "SIGNATURE"
                {
                    return Err(PackageError::archive_entry_type());
                }
                let plan = regular_plan(header, path, raw.position, &mut context)?;
                ParserState::ReadRegularBody(plan)
            }
            ParserState::ReadRegularBody(plan) => {
                let retain = plan.entry_class != EntryClass::Payload;
                let (digest, retained) = read_regular_body(&mut raw, plan.size, retain)?;
                context.store_entry(plan, digest, retained)?;
                ParserState::ExpectHeader
            }
            ParserState::ExpectSecondZeroBlock => {
                let mut block = [0_u8; BLOCK_SIZE];
                raw.read_exact(&mut block)?;
                if !is_zero_block(&block) {
                    return Err(PackageError::archive_truncated());
                }
                ParserState::ExpectEof
            }
            ParserState::ExpectEof => {
                raw.prove_eof()?;
                let physical_bytes = raw.position;
                let physical_digest = raw.digest();
                return context.finish(physical_digest, physical_bytes);
            }
        };
    }
}

#[cfg(test)]
fn inspect_bytes(bytes: &[u8]) -> Result<ArchiveInspection, PackageError> {
    inspect_bytes_with_limits(bytes, ArchiveLimits::production())
}

#[cfg(test)]
fn inspect_bytes_with_limits(
    bytes: &[u8],
    limits: ArchiveLimits,
) -> Result<ArchiveInspection, PackageError> {
    inspect_reader_with_limits(&mut Cursor::new(bytes), limits)
}

fn regular_plan(
    header: RawHeader,
    path: PackagePath,
    body_offset: u64,
    context: &mut ParserContext,
) -> Result<RegularPlan, PackageError> {
    let mode = match header.mode {
        0o444 => PackageFileMode::ReadOnly,
        0o555 => PackageFileMode::Executable,
        _ => return Err(PackageError::archive_header()),
    };
    let entry_class = context.accept_entry(&path, header.size)?;
    if matches!(entry_class, EntryClass::Package | EntryClass::Signature)
        && mode != PackageFileMode::ReadOnly
    {
        return Err(PackageError::archive_header());
    }
    Ok(RegularPlan {
        path,
        mode,
        size: header.size,
        body_offset,
        entry_class,
    })
}

fn parse_header(block: &[u8; BLOCK_SIZE]) -> Result<RawHeader, PackageError> {
    let mode = decode_number(&block[100..108])?;
    let uid = decode_number(&block[108..116])?;
    let gid = decode_number(&block[116..124])?;
    let size = decode_number(&block[124..136])?;
    let mtime = decode_number(&block[136..148])?;
    let checksum = decode_checksum(&block[148..156])?;
    let devmajor = decode_number(&block[329..337])?;
    let devminor = decode_number(&block[337..345])?;
    let calculated = block
        .iter()
        .enumerate()
        .try_fold(0_u64, |sum, (index, byte)| {
            let value = if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            };
            sum.checked_add(value)
        })
        .ok_or_else(PackageError::archive_header)?;
    if checksum != calculated
        || uid != 0
        || gid != 0
        || mtime != 0
        || devmajor != 0
        || devminor != 0
        || &block[257..263] != b"ustar "
        || &block[263..265] != b" \0"
        || block[157..257].iter().any(|byte| *byte != 0)
        || block[265..329].iter().any(|byte| *byte != 0)
        || block[345..].iter().any(|byte| *byte != 0)
    {
        return Err(PackageError::archive_header());
    }

    let entry_type = block[156];
    match entry_type {
        REGULAR_TYPE if matches!(mode, 0o444 | 0o555) => {}
        LONG_NAME_TYPE if mode == 0o644 => {}
        REGULAR_TYPE | LONG_NAME_TYPE => return Err(PackageError::archive_header()),
        _ => return Err(PackageError::archive_entry_type()),
    }
    let mut name = [0_u8; 100];
    name.copy_from_slice(&block[..100]);
    Ok(RawHeader {
        name,
        mode,
        size,
        entry_type,
    })
}

fn decode_number(field: &[u8]) -> Result<u64, PackageError> {
    if !matches!(field.len(), 8 | 12)
        || field[0] & 0x80 != 0
        || field.last() != Some(&0)
        || field[..field.len() - 1]
            .iter()
            .any(|byte| !(b'0'..=b'7').contains(byte))
    {
        return Err(PackageError::archive_header());
    }
    let digits = std::str::from_utf8(&field[..field.len() - 1])
        .map_err(|_| PackageError::archive_header())?;
    let value = u64::from_str_radix(digits, 8).map_err(|_| PackageError::archive_header())?;
    let canonical = format!("{value:0width$o}", width = field.len() - 1);
    if canonical.as_bytes() != &field[..field.len() - 1] {
        return Err(PackageError::archive_header());
    }
    Ok(value)
}

fn decode_checksum(field: &[u8]) -> Result<u64, PackageError> {
    if field.len() != 8
        || field[0] & 0x80 != 0
        || field[6] != 0
        || field[7] != b' '
        || field[..6].iter().any(|byte| !(b'0'..=b'7').contains(byte))
    {
        return Err(PackageError::archive_header());
    }
    let digits = std::str::from_utf8(&field[..6]).map_err(|_| PackageError::archive_header())?;
    let value = u64::from_str_radix(digits, 8).map_err(|_| PackageError::archive_header())?;
    if format!("{value:06o}").as_bytes() != &field[..6] {
        return Err(PackageError::archive_header());
    }
    Ok(value)
}

fn validate_long_header(header: &RawHeader) -> Result<(), PackageError> {
    if parse_header_name(&header.name)? != GNU_LONG_LINK || header.mode != 0o644 {
        return Err(PackageError::archive_entry_type());
    }
    Ok(())
}

fn parse_short_path(name: &[u8; 100], limits: ArchiveLimits) -> Result<PackagePath, PackageError> {
    let bytes = parse_header_name(name)?;
    if bytes.is_empty() {
        return Err(PackageError::archive_path());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PackageError::archive_path())?;
    validate_path_limits(text, limits)?;
    PackagePath::new(text).map_err(|_| PackageError::archive_path())
}

fn parse_header_name(name: &[u8; 100]) -> Result<&[u8], PackageError> {
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    if name[end..].iter().any(|byte| *byte != 0) {
        return Err(PackageError::archive_path());
    }
    Ok(&name[..end])
}

fn read_long_name<R: Read>(
    raw: &mut RawStream<'_, R>,
    size: u64,
    limits: ArchiveLimits,
) -> Result<PackagePath, PackageError> {
    if size > limits.max_long_name_body || size < 102 {
        return Err(if size > limits.max_long_name_body {
            PackageError::archive_quota()
        } else {
            PackageError::archive_path()
        });
    }
    let length = usize::try_from(size).map_err(|_| PackageError::archive_quota())?;
    let mut body = vec![0_u8; length];
    raw.read_exact(&mut body)?;
    read_padding(raw, size)?;
    if body.last() != Some(&0) || body[..body.len() - 1].contains(&0) {
        return Err(PackageError::archive_path());
    }
    let path_bytes = &body[..body.len() - 1];
    if path_bytes.len() <= 100 {
        return Err(PackageError::archive_path());
    }
    let path = std::str::from_utf8(path_bytes).map_err(|_| PackageError::archive_path())?;
    validate_path_limits(path, limits)?;
    PackagePath::new(path).map_err(|_| PackageError::archive_path())
}

fn read_regular_body<R: Read>(
    raw: &mut RawStream<'_, R>,
    size: u64,
    retain: bool,
) -> Result<(Digest, Vec<u8>), PackageError> {
    let mut retained = Vec::new();
    if retain {
        retained
            .try_reserve_exact(usize::try_from(size).map_err(|_| PackageError::archive_quota())?)
            .map_err(|_| PackageError::archive_quota())?;
    }
    let mut remaining = size;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let chunk = usize::try_from(
            remaining.min(u64::try_from(buffer.len()).map_err(|_| PackageError::archive_quota())?),
        )
        .map_err(|_| PackageError::archive_quota())?;
        raw.read_exact(&mut buffer[..chunk])?;
        hasher.update(&buffer[..chunk]);
        if retain {
            retained.extend_from_slice(&buffer[..chunk]);
        }
        remaining -= u64::try_from(chunk).map_err(|_| PackageError::archive_quota())?;
    }
    read_padding(raw, size)?;
    Ok((digest_from_bytes(hasher.finalize().into()), retained))
}

fn read_padding<R: Read>(raw: &mut RawStream<'_, R>, size: u64) -> Result<(), PackageError> {
    let remainder = size % BLOCK_SIZE as u64;
    let padding = if remainder == 0 {
        0
    } else {
        BLOCK_SIZE as u64 - remainder
    };
    let mut bytes = [0_u8; BLOCK_SIZE];
    let length = usize::try_from(padding).map_err(|_| PackageError::archive_quota())?;
    raw.read_exact(&mut bytes[..length])?;
    if bytes[..length].iter().any(|byte| *byte != 0) {
        return Err(PackageError::archive_header());
    }
    Ok(())
}

fn validate_path_limits(path: &str, limits: ArchiveLimits) -> Result<(), PackageError> {
    let path_bytes = u64::try_from(path.len()).map_err(|_| PackageError::archive_quota())?;
    if path_bytes > limits.max_path_bytes {
        return Err(PackageError::archive_quota());
    }
    let mut depth = 0_u64;
    for component in path.split('/') {
        depth = checked_increment(depth)?;
        if u64::try_from(component.len()).map_err(|_| PackageError::archive_quota())?
            > limits.max_component_bytes
            || depth > limits.max_path_depth
        {
            return Err(PackageError::archive_quota());
        }
    }
    Ok(())
}

fn collision_key(path: &str, maximum_bytes: u64) -> Result<String, PackageError> {
    // Both pinned crates expose Unicode 16.0 tables. This is exactly:
    // NFD -> full default non-Turkic case fold -> NFD.
    let key = path
        .chars()
        .nfd()
        .default_case_fold()
        .nfd()
        .collect::<String>();
    if u64::try_from(key.len()).map_err(|_| PackageError::archive_quota())? > maximum_bytes {
        return Err(PackageError::archive_quota());
    }
    Ok(key)
}

#[cfg(test)]
fn collision_key_for_test(path: &str) -> Result<String, PackageError> {
    collision_key(path, ArchiveLimits::production().max_collision_key_bytes)
}

fn increment_raw_record(current: u64, limits: ArchiveLimits) -> Result<u64, PackageError> {
    let next = checked_increment(current)?;
    if next > limits.max_raw_records {
        return Err(PackageError::archive_quota());
    }
    Ok(next)
}

fn checked_increment(value: u64) -> Result<u64, PackageError> {
    value.checked_add(1).ok_or_else(PackageError::archive_quota)
}

fn is_zero_block(block: &[u8; BLOCK_SIZE]) -> bool {
    block.iter().all(|byte| *byte == 0)
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
pub(crate) fn entry_bytes_for_test(
    path: &str,
    mode: u32,
    body: &[u8],
) -> Result<Vec<u8>, PackageError> {
    let path = PackagePath::new(path).map_err(|_| PackageError::package_metadata())?;
    let mut builder = Builder::new(Vec::new());
    append_profile_entry(
        &mut builder,
        &path,
        mode,
        u64::try_from(body.len()).map_err(|_| PackageError::package_metadata())?,
        Cursor::new(body),
    )?;
    builder
        .into_inner()
        .map_err(|_| PackageError::archive_write())
}

#[cfg(test)]
pub(crate) fn encode_number_for_test(width: usize, value: u64) -> Result<Vec<u8>, PackageError> {
    let mut field = vec![0_u8; width];
    encode_number(&mut field, value)?;
    Ok(field)
}

#[cfg(test)]
pub(crate) fn encode_checksum_for_test(value: u64) -> Result<Vec<u8>, PackageError> {
    let mut field = vec![0_u8; 8];
    encode_checksum(&mut field, value)?;
    Ok(field)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use jarvis_plugin_protocol::package::PackagePath;

    use super::{
        collision_key_for_test, inspect_bytes, inspect_bytes_with_limits, ArchiveLimits, BLOCK_SIZE,
    };

    fn write_octal(field: &mut [u8], value: u64) {
        let digits = field.len() - 1;
        let text = format!("{value:0digits$o}");
        field[..digits].copy_from_slice(text.as_bytes());
        field[digits] = 0;
    }

    fn raw_header(name: &[u8], mode: u64, size: u64, entry_type: u8) -> [u8; BLOCK_SIZE] {
        let mut header = [0_u8; BLOCK_SIZE];
        header[..name.len()].copy_from_slice(name);
        write_octal(&mut header[100..108], mode);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], size);
        write_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = entry_type;
        header[257..263].copy_from_slice(b"ustar ");
        header[263..265].copy_from_slice(b" \0");
        write_octal(&mut header[329..337], 0);
        write_octal(&mut header[337..345], 0);
        rewrite_checksum(&mut header);
        header
    }

    fn rewrite_checksum(header: &mut [u8; BLOCK_SIZE]) {
        header[148..156].fill(b' ');
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
        let encoded = format!("{checksum:06o}");
        header[148..154].copy_from_slice(encoded.as_bytes());
        header[154] = 0;
        header[155] = b' ';
    }

    fn append_raw_entry(archive: &mut Vec<u8>, name: &[u8], mode: u64, body: &[u8], kind: u8) {
        archive.extend_from_slice(&raw_header(
            name,
            mode,
            u64::try_from(body.len()).unwrap(),
            kind,
        ));
        archive.extend_from_slice(body);
        let padding = (BLOCK_SIZE - body.len() % BLOCK_SIZE) % BLOCK_SIZE;
        archive.resize(archive.len() + padding, 0);
    }

    fn finish(archive: &mut Vec<u8>) {
        archive.resize(archive.len() + BLOCK_SIZE * 2, 0);
    }

    fn valid_archive(payload: &[(&[u8], u64, &[u8])]) -> Vec<u8> {
        let mut archive = Vec::new();
        append_raw_entry(&mut archive, b"plugin.json", 0o444, b"{}", b'0');
        for (path, mode, body) in payload {
            append_raw_entry(&mut archive, path, *mode, body, b'0');
        }
        append_raw_entry(&mut archive, b"package.json", 0o444, b"{}", b'0');
        append_raw_entry(&mut archive, b"SIGNATURE", 0o444, b"{}", b'0');
        finish(&mut archive);
        archive
    }

    fn valid_long_archive(path: &[u8]) -> Vec<u8> {
        let mut archive = Vec::new();
        append_raw_entry(&mut archive, b"plugin.json", 0o444, b"{}", b'0');
        archive.extend_from_slice(&long_name_record(path));
        archive.extend_from_slice(&raw_header(b"././@LongFile", 0o444, 0, b'0'));
        append_raw_entry(&mut archive, b"package.json", 0o444, b"{}", b'0');
        append_raw_entry(&mut archive, b"SIGNATURE", 0o444, b"{}", b'0');
        finish(&mut archive);
        archive
    }

    fn long_name_record(path: &[u8]) -> Vec<u8> {
        let mut archive = Vec::new();
        let mut body = path.to_vec();
        body.push(0);
        append_raw_entry(&mut archive, b"././@LongLink", 0o644, &body, b'L');
        archive
    }

    fn assert_code(bytes: &[u8], expected: &str) {
        assert_eq!(inspect_bytes(bytes).unwrap_err().code(), expected);
    }

    #[test]
    fn archive_rejects_base256_size_before_decode() {
        let mut archive = valid_archive(&[]);
        archive[124] = 0x80;
        rewrite_checksum((&mut archive[..BLOCK_SIZE]).try_into().unwrap());
        assert_code(&archive, "archive_header");
    }

    #[test]
    fn archive_rejects_noncanonical_octal_and_checksum() {
        let mut noncanonical = valid_archive(&[]);
        noncanonical[100..108].copy_from_slice(b"0000444 ");
        rewrite_checksum((&mut noncanonical[..BLOCK_SIZE]).try_into().unwrap());
        assert_code(&noncanonical, "archive_header");

        let mut bad_checksum = valid_archive(&[]);
        bad_checksum[148] ^= 1;
        assert_code(&bad_checksum, "archive_header");
    }

    #[test]
    fn archive_rejects_pax_global_local_and_sparse_extensions() {
        for kind in [b'x', b'g', b'K', b'S'] {
            let mut archive = Vec::new();
            append_raw_entry(&mut archive, b"plugin.json", 0o444, b"{}", kind);
            finish(&mut archive);
            assert_code(&archive, "archive_entry_type");
        }
    }

    #[test]
    fn archive_rejects_repeated_orphan_and_short_gnu_longname() {
        let long = "a".repeat(101);
        let mut repeated = long_name_record(long.as_bytes());
        repeated.extend_from_slice(&long_name_record(long.as_bytes()));
        finish(&mut repeated);
        assert_code(&repeated, "archive_entry_type");

        let mut orphan = long_name_record(long.as_bytes());
        finish(&mut orphan);
        assert_code(&orphan, "archive_entry_type");

        let mut short = long_name_record(&[b'a'; 100]);
        short.extend_from_slice(&raw_header(b"././@LongFile", 0o444, 0, b'0'));
        finish(&mut short);
        assert_code(&short, "archive_path");
    }

    #[test]
    fn archive_rejects_truncated_header_body_padding_and_terminator() {
        let archive = valid_archive(&[]);
        for truncated in [
            &archive[..100],
            &archive[..BLOCK_SIZE + 1],
            &archive[..BLOCK_SIZE + 100],
            &archive[..archive.len() - BLOCK_SIZE],
        ] {
            assert_code(truncated, "archive_truncated");
        }
    }

    #[test]
    fn archive_rejects_nonzero_padding_and_trailing_concatenated_archive() {
        let mut padding = valid_archive(&[]);
        padding[BLOCK_SIZE + 2] = 1;
        assert_code(&padding, "archive_header");

        let mut trailing = valid_archive(&[]);
        trailing.push(1);
        assert_code(&trailing, "archive_trailing");

        let mut concatenated = valid_archive(&[]);
        concatenated.extend_from_slice(&valid_archive(&[]));
        assert_code(&concatenated, "archive_trailing");
    }

    #[test]
    fn archive_rejects_links_directories_devices_fifo_socket_and_sparse() {
        for kind in [b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'D', b'F', b'S'] {
            let mut archive = Vec::new();
            append_raw_entry(&mut archive, b"plugin.json", 0o444, b"", kind);
            finish(&mut archive);
            assert_code(&archive, "archive_entry_type");
        }
    }

    #[test]
    fn archive_rejects_absolute_dot_empty_backslash_nul_and_non_nfc_paths() {
        for path in [
            b"/absolute".as_slice(),
            b".".as_slice(),
            b"a/../b".as_slice(),
            b"a\\b".as_slice(),
            b"a\0b".as_slice(),
            "e\u{301}".as_bytes(),
        ] {
            let mut archive = Vec::new();
            append_raw_entry(&mut archive, path, 0o444, b"", b'0');
            finish(&mut archive);
            assert_code(&archive, "archive_path");
        }
        let mut empty = Vec::new();
        append_raw_entry(&mut empty, b"", 0o444, b"", b'0');
        finish(&mut empty);
        assert_code(&empty, "archive_path");
    }

    #[test]
    fn archive_rejects_duplicate_normalized_names() {
        let archive = valid_archive(&[(b"a", 0o444, b""), (b"a", 0o444, b"")]);
        assert_code(&archive, "archive_duplicate");
    }

    #[test]
    fn archive_accepts_exact_raw_and_logical_record_limits() {
        let archive = valid_archive(&[]);
        let mut exact = ArchiveLimits::production();
        exact.max_raw_records = 3;
        exact.max_logical_entries = 3;
        assert!(inspect_bytes_with_limits(&archive, exact).is_ok());

        let mut raw_plus_one = exact;
        raw_plus_one.max_raw_records = 2;
        assert_code_with_limits(&archive, raw_plus_one, "archive_quota");
        let mut logical_plus_one = exact;
        logical_plus_one.max_logical_entries = 2;
        assert_code_with_limits(&archive, logical_plus_one, "archive_quota");
    }

    #[test]
    fn archive_rejects_raw_and_logical_record_limits_plus_one() {
        archive_accepts_exact_raw_and_logical_record_limits();
    }

    #[test]
    fn archive_rejects_namespace_shape_that_exceeds_the_memory_budget() {
        let mut archive = Vec::new();
        append_raw_entry(&mut archive, b"plugin.json", 0o444, b"{}", b'0');
        for index in 0..5_000 {
            let components = (0..5)
                .map(|level| format!("{index:04}-{level}-{}", "x".repeat(193)))
                .collect::<Vec<_>>();
            let path = components.join("/");
            assert_eq!(path.len(), 1_004);
            archive.extend_from_slice(&long_name_record(path.as_bytes()));
            archive.extend_from_slice(&raw_header(b"././@LongFile", 0o444, 0, b'0'));
        }
        append_raw_entry(&mut archive, b"package.json", 0o444, b"{}", b'0');
        append_raw_entry(&mut archive, b"SIGNATURE", 0o444, b"{}", b'0');
        finish(&mut archive);

        assert_code(&archive, "archive_quota");
    }

    #[test]
    fn archive_rejects_executable_generated_metadata_entries() {
        for header_offset in [BLOCK_SIZE * 2, BLOCK_SIZE * 4] {
            let mut archive = valid_archive(&[]);
            let header: &mut [u8; BLOCK_SIZE] = (&mut archive
                [header_offset..header_offset + BLOCK_SIZE])
                .try_into()
                .unwrap();
            write_octal(&mut header[100..108], 0o555);
            rewrite_checksum(header);
            assert_code(&archive, "archive_header");
        }
    }

    #[test]
    fn unicode_collision_vectors() {
        assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0));
        assert_eq!(caseless::UNICODE_VERSION, (16, 0, 0));
        assert_eq!(
            collision_key_for_test("Straße").unwrap(),
            collision_key_for_test("STRASSE").unwrap()
        );
        assert_eq!(
            collision_key_for_test("σςΣ").unwrap(),
            collision_key_for_test("ΣΣΣ").unwrap()
        );
        assert_eq!(
            collision_key_for_test("I").unwrap(),
            collision_key_for_test("i").unwrap()
        );
        assert_ne!(
            collision_key_for_test("İ").unwrap(),
            collision_key_for_test("i").unwrap()
        );
        assert!(PackagePath::new("K").is_err());
        assert!(PackagePath::new("e\u{301}").is_err());

        let collision =
            valid_archive(&[(b"STRASSE", 0o444, b""), ("Straße".as_bytes(), 0o444, b"")]);
        assert_code(&collision, "archive_case_collision");
        assert_code(
            &valid_archive(&[(b"K", 0o444, b""), (b"k", 0o444, b"")]),
            "archive_case_collision",
        );
        assert_code(
            &valid_archive(&[("Σ".as_bytes(), 0o444, b""), ("ς".as_bytes(), 0o444, b"")]),
            "archive_case_collision",
        );
        assert!(inspect_bytes(&valid_archive(&[
            (b"i", 0o444, b""),
            ("İ".as_bytes(), 0o444, b""),
        ]))
        .is_ok());
        assert_code(
            &valid_archive(&[(b"A/b", 0o444, b""), (b"a", 0o444, b"")]),
            "archive_case_collision",
        );
        assert_code(
            &valid_archive(&[(b"PACKAGE.json", 0o444, b"")]),
            "archive_case_collision",
        );
        let reserved = valid_archive(&[(b"signature/payload", 0o444, b"")]);
        assert_code(&reserved, "archive_case_collision");
    }

    #[test]
    fn all_limits_accept_exact_and_reject_plus_one() {
        let production = ArchiveLimits::production();
        assert_eq!(production.max_physical_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(
            production.max_unpacked_payload_bytes,
            2 * 1024 * 1024 * 1024
        );
        assert_eq!(production.max_single_payload_file, 512 * 1024 * 1024);
        assert_eq!(production.max_payload_files, 20_000);
        assert_eq!(production.max_logical_entries, 20_002);
        assert_eq!(production.max_raw_records, 40_002);
        assert_eq!(production.max_path_bytes, 1_024);
        assert_eq!(production.max_component_bytes, 255);
        assert_eq!(production.max_path_depth, 64);
        assert_eq!(production.max_namespace_nodes, 100_000);
        assert_eq!(production.max_namespace_stored_bytes, 24 * 1024 * 1024);
        assert_eq!(production.max_collision_key_bytes, 4_096);
        assert_eq!(production.max_long_name_body, 1_025);
        assert_eq!(production.max_package_json_bytes, 16 * 1024 * 1024);
        assert_eq!(production.max_signature_bytes, 4 * 1024);
        assert_eq!(production.max_plugin_json_bytes, 256 * 1024);
        assert_eq!(production.max_json_depth, 64);
        assert_eq!(production.max_json_nodes, 250_000);
        assert_eq!(production.max_json_string_bytes, 64 * 1024);

        let archive = valid_archive(&[]);
        macro_rules! assert_exact {
            ($field:ident, $value:expr) => {{
                let mut exact = production;
                exact.$field = $value;
                assert!(
                    inspect_bytes_with_limits(&archive, exact).is_ok(),
                    stringify!($field)
                );
            }};
        }
        assert_exact!(max_physical_bytes, archive.len() as u64);
        assert_exact!(max_unpacked_payload_bytes, 2);
        assert_exact!(max_single_payload_file, 2);
        assert_exact!(max_payload_files, 1);
        assert_exact!(max_logical_entries, 3);
        assert_exact!(max_raw_records, 3);
        assert_exact!(max_path_bytes, 12);
        assert_exact!(max_component_bytes, 12);
        assert_exact!(max_path_depth, 1);
        assert_exact!(max_namespace_nodes, 3);
        assert_exact!(max_namespace_stored_bytes, 832);
        assert_exact!(max_collision_key_bytes, 12);
        assert_exact!(max_package_json_bytes, 2);
        assert_exact!(max_signature_bytes, 2);
        assert_exact!(max_plugin_json_bytes, 2);

        let mut physical = production;
        physical.max_physical_bytes = archive.len() as u64 - 1;
        assert_code_with_limits(&archive, physical, "archive_quota");
        let mut unpacked = production;
        unpacked.max_unpacked_payload_bytes = 1;
        assert_code_with_limits(&archive, unpacked, "archive_quota");
        let mut single = production;
        single.max_single_payload_file = 1;
        assert_code_with_limits(&archive, single, "archive_quota");
        let mut payload_count = production;
        payload_count.max_payload_files = 0;
        assert_code_with_limits(&archive, payload_count, "archive_quota");
        let mut package = production;
        package.max_package_json_bytes = 1;
        assert_code_with_limits(&archive, package, "archive_quota");
        let mut signature = production;
        signature.max_signature_bytes = 1;
        assert_code_with_limits(&archive, signature, "archive_quota");
        let mut plugin = production;
        plugin.max_plugin_json_bytes = 1;
        assert_code_with_limits(&archive, plugin, "archive_quota");

        let path_archive = valid_archive(&[(b"payload-pathx", 0o444, b"")]);
        let mut path_exact = production;
        path_exact.max_path_bytes = 13;
        assert!(inspect_bytes_with_limits(&path_archive, path_exact).is_ok());
        path_exact.max_path_bytes = 12;
        assert_code_with_limits(&path_archive, path_exact, "archive_quota");

        let component_archive = valid_archive(&[(b"abcdefghijklmn", 0o444, b"")]);
        let mut component_exact = production;
        component_exact.max_component_bytes = 14;
        assert!(inspect_bytes_with_limits(&component_archive, component_exact).is_ok());
        component_exact.max_component_bytes = 13;
        assert_code_with_limits(&component_archive, component_exact, "archive_quota");

        let depth_archive = valid_archive(&[(b"a/b", 0o444, b"")]);
        let mut depth_exact = production;
        depth_exact.max_path_depth = 2;
        assert!(inspect_bytes_with_limits(&depth_archive, depth_exact).is_ok());
        depth_exact.max_path_depth = 1;
        assert_code_with_limits(&depth_archive, depth_exact, "archive_quota");

        let mut namespace = production;
        namespace.max_namespace_nodes = 2;
        assert_code_with_limits(&archive, namespace, "archive_quota");
        let mut namespace_stored = production;
        namespace_stored.max_namespace_stored_bytes = 831;
        assert_code_with_limits(&archive, namespace_stored, "archive_quota");
        let mut collision_key = production;
        collision_key.max_collision_key_bytes = 11;
        assert_code_with_limits(&archive, collision_key, "archive_quota");

        let long_archive = valid_long_archive(&[b'l'; 101]);
        let mut long_exact = production;
        long_exact.max_long_name_body = 102;
        assert!(inspect_bytes_with_limits(&long_archive, long_exact).is_ok());
        long_exact.max_long_name_body = 101;
        assert_code_with_limits(&long_archive, long_exact, "archive_quota");

        let mut huge_size = valid_archive(&[]);
        huge_size[124..136].copy_from_slice(b"77777777777\0");
        rewrite_checksum((&mut huge_size[..BLOCK_SIZE]).try_into().unwrap());
        assert_code(&huge_size, "archive_quota");
    }

    #[test]
    fn inspection_memory_is_bounded() {
        let mut payload = Vec::new();
        for index in 0..256 {
            payload.push((
                format!("payload/{index:04}").into_bytes(),
                vec![index as u8],
            ));
        }
        let refs = payload
            .iter()
            .map(|(path, body)| (path.as_slice(), 0o444, body.as_slice()))
            .collect::<Vec<_>>();
        let archive = valid_archive(&refs);
        let inspection = inspect_bytes(&archive).unwrap();
        assert_eq!(inspection.payload_entries().len(), 257);
        assert_eq!(inspection.retained_body_bytes(), 6);
        assert_eq!(
            inspection.physical_digest(),
            &crate::hash::sha256_digest(&archive)
        );
        assert_eq!(inspection.physical_bytes(), archive.len() as u64);
        assert!(inspection
            .entries()
            .windows(2)
            .all(|entries| entries[0].body_offset() < entries[1].body_offset()));

        let mut chunked = ChunkedReader {
            bytes: &archive,
            offset: 0,
            chunk: 3,
        };
        assert_eq!(
            super::inspect_reader_with_limits(&mut chunked, ArchiveLimits::production())
                .unwrap()
                .payload_entries()
                .len(),
            257
        );

        let first_terminator = archive.len() - BLOCK_SIZE * 2;
        let mut body_error = ErrorReader {
            bytes: &archive,
            offset: 0,
            fail_at: BLOCK_SIZE + 1,
        };
        assert_eq!(
            super::inspect_reader_with_limits(&mut body_error, ArchiveLimits::production())
                .unwrap_err()
                .code(),
            "archive_truncated"
        );
        let mut eof_error = ErrorReader {
            bytes: &archive,
            offset: 0,
            fail_at: first_terminator + BLOCK_SIZE * 2,
        };
        assert_eq!(
            super::inspect_reader_with_limits(&mut eof_error, ArchiveLimits::production())
                .unwrap_err()
                .code(),
            "archive_trailing"
        );
    }

    #[test]
    fn inspection_retains_validated_directories_in_parent_first_order() {
        let archive = valid_archive(&[
            (b"a/b/c", 0o444, b"one"),
            (b"a/d", 0o444, b"two"),
            (b"z/file", 0o444, b"three"),
        ]);
        let inspection = inspect_bytes(&archive).unwrap();
        assert_eq!(
            inspection
                .validated_directories()
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z", "a/b"]
        );
    }

    #[test]
    #[ignore = "streams a synthetic near-2-GiB archive and checks process RSS"]
    fn inspection_near_two_gib_stays_below_rss_budget() {
        let mut segments = Vec::new();
        append_sparse_entry(&mut segments, b"plugin.json", 2);
        for index in 0..4 {
            append_sparse_entry(
                &mut segments,
                format!("payload/{index}").as_bytes(),
                500 * 1024 * 1024,
            );
        }
        append_sparse_entry(&mut segments, b"package.json", 2);
        append_sparse_entry(&mut segments, b"SIGNATURE", 2);
        segments.push(SparseSegment::Zeros((BLOCK_SIZE * 2) as u64));

        let mut reader = SparseReader {
            segments,
            segment: 0,
            offset: 0,
        };
        let inspection =
            super::inspect_reader_with_limits(&mut reader, ArchiveLimits::production()).unwrap();
        assert!(inspection.physical_bytes() > 1_900 * 1024 * 1024);
        let rss_output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(rss_output.status.success());
        let rss_kib = std::str::from_utf8(&rss_output.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap();
        assert!(rss_kib < 128 * 1024, "RSS was {rss_kib} KiB");
    }

    fn assert_code_with_limits(bytes: &[u8], limits: ArchiveLimits, expected: &str) {
        assert_eq!(
            inspect_bytes_with_limits(bytes, limits).unwrap_err().code(),
            expected
        );
    }

    struct ChunkedReader<'a> {
        bytes: &'a [u8],
        offset: usize,
        chunk: usize,
    }

    impl Read for ChunkedReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.bytes.len() {
                return Ok(0);
            }
            let count = output
                .len()
                .min(self.chunk)
                .min(self.bytes.len() - self.offset);
            output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }

    struct ErrorReader<'a> {
        bytes: &'a [u8],
        offset: usize,
        fail_at: usize,
    }

    impl Read for ErrorReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.offset >= self.fail_at {
                return Err(io::Error::other("injected read error"));
            }
            if self.offset == self.bytes.len() {
                return Ok(0);
            }
            let count = output
                .len()
                .min(self.bytes.len() - self.offset)
                .min(self.fail_at - self.offset);
            output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }

    enum SparseSegment {
        Bytes(Vec<u8>),
        Zeros(u64),
    }

    fn append_sparse_entry(segments: &mut Vec<SparseSegment>, name: &[u8], size: u64) {
        segments.push(SparseSegment::Bytes(
            raw_header(name, 0o444, size, b'0').to_vec(),
        ));
        if size == 2 {
            segments.push(SparseSegment::Bytes(b"{}".to_vec()));
        } else {
            segments.push(SparseSegment::Zeros(size));
        }
        let remainder = size % BLOCK_SIZE as u64;
        if remainder != 0 {
            segments.push(SparseSegment::Zeros(BLOCK_SIZE as u64 - remainder));
        }
    }

    struct SparseReader {
        segments: Vec<SparseSegment>,
        segment: usize,
        offset: u64,
    }

    impl Read for SparseReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            while let Some(segment) = self.segments.get(self.segment) {
                let length = match segment {
                    SparseSegment::Bytes(bytes) => bytes.len() as u64,
                    SparseSegment::Zeros(length) => *length,
                };
                if self.offset == length {
                    self.segment += 1;
                    self.offset = 0;
                    continue;
                }
                let count =
                    usize::try_from((length - self.offset).min(output.len() as u64)).unwrap();
                match segment {
                    SparseSegment::Bytes(bytes) => {
                        let start = usize::try_from(self.offset).unwrap();
                        output[..count].copy_from_slice(&bytes[start..start + count]);
                    }
                    SparseSegment::Zeros(_) => output[..count].fill(0),
                }
                self.offset += count as u64;
                return Ok(count);
            }
            Ok(0)
        }
    }
}
