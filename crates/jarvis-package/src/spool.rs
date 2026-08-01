use std::fs::File;
use std::ops::Range;
use std::os::unix::fs::FileExt;

use jarvis_plugin_protocol::manifest::Digest;
use jarvis_plugin_protocol::package::PackagePath;
use rustix::fs::{fchmod, fstat, FileType, Mode};

use crate::PackageError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpooledFile {
    path: PackagePath,
    offset: u64,
    length: u64,
    digest: Digest,
    source_mode: u32,
}

impl SpooledFile {
    pub(crate) fn new(
        path: PackagePath,
        offset: u64,
        length: u64,
        digest: Digest,
        source_mode: u32,
    ) -> Result<Self, PackageError> {
        checked_span(offset, length)?;
        Ok(Self {
            path,
            offset,
            length,
            digest,
            source_mode,
        })
    }

    pub(crate) fn path(&self) -> &PackagePath {
        &self.path
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn digest(&self) -> &Digest {
        &self.digest
    }

    pub(crate) fn source_mode(&self) -> u32 {
        self.source_mode
    }
}

#[derive(Debug)]
pub(crate) struct SourceSnapshot {
    spool: File,
    files: Vec<SpooledFile>,
}

impl SourceSnapshot {
    pub(crate) fn create() -> Result<(File, Vec<SpooledFile>), PackageError> {
        let spool = tempfile::tempfile().map_err(|_| PackageError::source_invalid())?;
        fchmod(&spool, Mode::from_raw_mode(0o600)).map_err(|_| PackageError::source_invalid())?;
        validate_spool(&spool)?;
        Ok((spool, Vec::new()))
    }

    pub(crate) fn from_parts(
        spool: File,
        mut files: Vec<SpooledFile>,
    ) -> Result<Self, PackageError> {
        validate_spool(&spool)?;
        let mut by_offset = files.iter().collect::<Vec<_>>();
        by_offset.sort_by_key(|file| file.offset);
        let mut expected_offset = 0_u64;
        for file in by_offset {
            let span = checked_span(file.offset, file.length)?;
            if span.start != expected_offset {
                return Err(PackageError::source_invalid());
            }
            expected_offset = span.end;
        }
        let spool_size = fstat(&spool)
            .map_err(|_| PackageError::source_invalid())?
            .st_size;
        if spool_size < 0
            || u64::try_from(spool_size).map_err(|_| PackageError::source_invalid())?
                != expected_offset
        {
            return Err(PackageError::source_invalid());
        }
        files.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
        if files
            .windows(2)
            .any(|pair| pair[0].path.as_str() == pair[1].path.as_str())
        {
            return Err(PackageError::source_invalid());
        }
        Ok(Self { spool, files })
    }

    pub(crate) fn files(&self) -> &[SpooledFile] {
        &self.files
    }

    pub(crate) fn read_file(&self, path: &str) -> Result<Vec<u8>, PackageError> {
        let file = self
            .files
            .iter()
            .find(|file| file.path.as_str() == path)
            .ok_or_else(PackageError::source_invalid)?;
        let length = usize::try_from(file.length).map_err(|_| PackageError::source_invalid())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| PackageError::source_invalid())?;
        bytes.resize(length, 0);
        read_exact_at(&self.spool, &mut bytes, file.offset)?;
        Ok(bytes)
    }

    #[cfg(test)]
    fn spool_identity(&self) -> std::io::Result<SpoolIdentity> {
        let stat = fstat(&self.spool)?;
        Ok(SpoolIdentity {
            mode: stat.st_mode,
            link_count: stat.st_nlink,
        })
    }
}

pub(crate) fn checked_span(offset: u64, length: u64) -> Result<Range<u64>, PackageError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(PackageError::source_invalid)?;
    Ok(offset..end)
}

fn validate_spool(spool: &File) -> Result<(), PackageError> {
    let stat = fstat(spool).map_err(|_| PackageError::source_invalid())?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 0
        || stat.st_mode & 0o777 != 0o600
    {
        return Err(PackageError::source_invalid());
    }
    Ok(())
}

fn read_exact_at(file: &File, mut output: &mut [u8], mut offset: u64) -> Result<(), PackageError> {
    while !output.is_empty() {
        let read = file
            .read_at(output, offset)
            .map_err(|_| PackageError::source_invalid())?;
        if read == 0 {
            return Err(PackageError::source_invalid());
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| PackageError::source_invalid())?)
            .ok_or_else(PackageError::source_invalid)?;
        output = &mut output[read..];
    }
    Ok(())
}

#[cfg(test)]
struct SpoolIdentity {
    mode: libc::mode_t,
    link_count: libc::nlink_t,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::source::snapshot_source;

    use super::checked_span;

    #[test]
    fn aggregate_spool_is_owner_only_unlinked_and_spans_are_exact() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("plugin.json"), b"{}").unwrap();
        fs::write(source.path().join("payload"), b"payload").unwrap();

        let snapshot = snapshot_source(source.path()).unwrap();
        let identity = snapshot.spool_identity().unwrap();
        assert_eq!(identity.mode & 0o777, 0o600);
        assert_eq!(identity.link_count, 0);
        assert_eq!(snapshot.read_file("plugin.json").unwrap(), b"{}");
        assert_eq!(snapshot.read_file("payload").unwrap(), b"payload");
        assert_eq!(snapshot.files().len(), 2);
        assert!(snapshot
            .files()
            .iter()
            .all(|file| file.source_mode() & 0o111 == 0));
        assert!(snapshot
            .files()
            .iter()
            .all(|file| file.digest().as_str().starts_with("sha256:")));
        assert_eq!(snapshot.files()[0].path().as_str(), "payload");
        assert_eq!(snapshot.files()[0].length(), 7);
    }

    #[test]
    fn checked_spans_reject_offset_and_length_overflow() {
        assert_eq!(checked_span(10, 20).unwrap(), 10..30);
        assert!(checked_span(u64::MAX, 1).is_err());
        assert!(checked_span(u64::MAX - 1, 2).is_err());
    }
}
