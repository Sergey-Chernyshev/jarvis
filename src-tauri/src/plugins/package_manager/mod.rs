use std::fmt;
use std::fmt::Write as _;

pub mod consent;
pub mod downloader;
pub mod health;
pub mod lock;
pub mod manager;
pub mod migration;
pub mod operation;
pub mod paths;
pub mod quarantine;
pub mod receipt;
pub mod recovery;
mod secure_fs;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurableObservation<T> {
    Confirmed(T),
    DurabilityUnknown(T),
}

#[derive(Debug)]
pub struct StorageError {
    code: &'static str,
    message: String,
}

impl StorageError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StorageError {}

pub(crate) fn random_storage_id() -> Result<String, StorageError> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        StorageError::new(
            "storage_random",
            format!("cannot generate storage identifier: {error}"),
        )
    })?;
    Ok(format_storage_id(bytes))
}

fn format_storage_id(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut output = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod storage_tests {
    use super::format_storage_id;

    #[test]
    fn storage_ids_use_canonical_uuid_v4_shape_without_uuid_dependency() {
        assert_eq!(
            format_storage_id([0; 16]),
            "00000000-0000-4000-8000-000000000000"
        );
        assert_eq!(
            format_storage_id([0xff; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
    }
}

#[cfg(test)]
mod tests;
