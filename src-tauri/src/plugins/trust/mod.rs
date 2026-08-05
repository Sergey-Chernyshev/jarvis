use std::fmt;

pub mod catalog;
pub mod package;
pub mod provider;
pub mod signature;

pub(crate) const fn package_signature_domain() -> &'static [u8] {
    b"jarvis-plugin-package-v1"
}

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
