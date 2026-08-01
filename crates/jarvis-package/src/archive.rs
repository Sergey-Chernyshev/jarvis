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
