use std::fmt;

pub mod catalog;
pub mod package;
pub mod signature;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustError {
    code: &'static str,
}

impl TrustError {
    pub const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for TrustError {}
