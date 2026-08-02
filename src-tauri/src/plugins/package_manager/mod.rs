use std::fmt;

pub mod lock;
pub mod operation;
pub mod paths;
pub mod receipt;

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
