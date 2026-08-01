use std::io::{Cursor, Read, Write};

use jarvis_plugin_protocol::package::PackagePath;
use tar::{Builder, Header};

use crate::PackageError;

pub(crate) const BLOCK_SIZE: usize = 512;
const GNU_LONG_LINK: &[u8] = b"././@LongLink";
const GNU_LONG_FILE: &[u8] = b"././@LongFile";

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
        assert_eq!(production.max_collision_key_bytes, 4_096);
        assert_eq!(production.max_long_name_body, 1_025);
        assert_eq!(production.max_package_json_bytes, 16 * 1024 * 1024);
        assert_eq!(production.max_signature_bytes, 4 * 1024);
        assert_eq!(production.max_plugin_json_bytes, 256 * 1024);
        assert_eq!(production.max_json_depth, 64);
        assert_eq!(production.max_json_nodes, 250_000);
        assert_eq!(production.max_json_string_bytes, 64 * 1024);

        let archive = valid_archive(&[]);
        for mutate in [
            |limits: &mut ArchiveLimits| limits.max_physical_bytes = archive.len() as u64,
            |limits: &mut ArchiveLimits| limits.max_unpacked_payload_bytes = 2,
            |limits: &mut ArchiveLimits| limits.max_single_payload_file = 2,
            |limits: &mut ArchiveLimits| limits.max_payload_files = 1,
            |limits: &mut ArchiveLimits| limits.max_logical_entries = 3,
            |limits: &mut ArchiveLimits| limits.max_raw_records = 3,
            |limits: &mut ArchiveLimits| limits.max_path_bytes = 12,
            |limits: &mut ArchiveLimits| limits.max_component_bytes = 12,
            |limits: &mut ArchiveLimits| limits.max_path_depth = 1,
            |limits: &mut ArchiveLimits| limits.max_namespace_nodes = 3,
            |limits: &mut ArchiveLimits| limits.max_collision_key_bytes = 12,
            |limits: &mut ArchiveLimits| limits.max_package_json_bytes = 2,
            |limits: &mut ArchiveLimits| limits.max_signature_bytes = 2,
            |limits: &mut ArchiveLimits| limits.max_plugin_json_bytes = 2,
        ] {
            let mut exact = production;
            mutate(&mut exact);
            assert!(inspect_bytes_with_limits(&archive, exact).is_ok());
        }

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
}
