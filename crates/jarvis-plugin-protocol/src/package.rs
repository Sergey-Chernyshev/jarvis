use std::fmt;

use semver::Version;
use serde::de::{self};
use serde::{Deserialize, Deserializer, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::manifest::{Digest, PluginId, PublisherId, StateDeclaration, VersionRange};

pub const PACKAGE_SCHEMA_VERSION: u32 = 1;
pub const PACKAGE_METADATA_SCHEMA_JSON: &[u8] =
    include_bytes!("../schema/plugin-package-v1.schema.json");
pub const PACKAGE_SIGNATURE_SCHEMA_JSON: &[u8] =
    include_bytes!("../schema/plugin-package-signature-v1.schema.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageContractError(&'static str);

impl PackageContractError {
    pub fn code(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for PackageContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for PackageContractError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageMetadataV1 {
    pub schema_version: u32,
    pub plugin_id: PluginId,
    pub publisher: PublisherId,
    pub version: Version,
    pub manifest_digest: Digest,
    pub target: PackageTarget,
    pub minimum_macos: MacOsVersion,
    pub jarvis_range: VersionRange,
    pub plugin_api: u32,
    pub state: StateDeclaration,
    pub files: Vec<PackageFile>,
    pub payload_root: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub path: PackagePath,
    pub kind: PackageFileKind,
    pub mode: PackageFileMode,
    pub size: u64,
    pub digest: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageTarget {
    DarwinArm64,
    DarwinAmd64,
}

impl PackageTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "darwin-arm64",
            Self::DarwinAmd64 => "darwin-amd64",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageFileKind {
    Regular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageFileMode {
    #[serde(rename = "0444")]
    ReadOnly,
    #[serde(rename = "0555")]
    Executable,
}

impl PackageFileMode {
    pub fn as_octal(self) -> u32 {
        match self {
            Self::ReadOnly => 0o444,
            Self::Executable => 0o555,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PackagePath(String);

impl PackagePath {
    pub fn new(value: impl Into<String>) -> Result<Self, PackageContractError> {
        let value = value.into();
        if !valid_package_path(&value) {
            return Err(PackageContractError("package_path"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PackagePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid package path"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MacOsVersion(String);

impl MacOsVersion {
    pub fn parse(value: impl Into<String>) -> Result<Self, PackageContractError> {
        let value = value.into();
        if !valid_macos_version(&value) {
            return Err(PackageContractError("package_macos_version"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MacOsVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(|_| de::Error::custom("invalid minimum macOS version"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignatureAlgorithm {
    Ed25519,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSignatureV1 {
    pub algorithm: SignatureAlgorithm,
    pub key_id: String,
    pub value: String,
}

impl PackageSignatureV1 {
    pub fn new(
        algorithm: SignatureAlgorithm,
        key_id: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, PackageContractError> {
        let signature = Self {
            algorithm,
            key_id: key_id.into(),
            value: value.into(),
        };
        signature.validate()?;
        Ok(signature)
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn validate(&self) -> Result<(), PackageContractError> {
        if self.key_id.is_empty()
            || self.key_id.len() > 128
            || !self.key_id.is_ascii()
            || !self.key_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            })
        {
            return Err(PackageContractError("package_signature_key_id"));
        }
        if !is_canonical_64_byte_base64(&self.value) {
            return Err(PackageContractError("package_signature_value"));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for PackageSignatureV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            algorithm: SignatureAlgorithm,
            key_id: String,
            value: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.algorithm, wire.key_id, wire.value)
            .map_err(|_| de::Error::custom("invalid package signature"))
    }
}

fn valid_package_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 1024
        || !value.nfc().eq(value.chars())
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value.contains("//")
        || value.chars().any(|character| {
            character == '\0'
                || character.is_control()
                || matches!(character, '%' | '?' | '#' | ':')
        })
    {
        return false;
    }

    let mut depth = 0usize;
    for component in value.split('/') {
        depth += 1;
        if component.is_empty() || component == "." || component == ".." || component.len() > 255 {
            return false;
        }
    }
    depth <= 64
}

fn valid_macos_version(value: &str) -> bool {
    let mut components = value.split('.');
    let first = components.next();
    let second = components.next();
    let third = components.next();
    if components.next().is_some() {
        return false;
    }
    [first, second, third].into_iter().all(|component| {
        let Some(component) = component else {
            return false;
        };
        !component.is_empty()
            && component.bytes().all(|byte| byte.is_ascii_digit())
            && (component == "0" || !component.starts_with('0'))
    })
}

fn is_canonical_64_byte_base64(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 88
        && bytes[86..] == *b"=="
        && bytes[..86]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        && matches!(bytes[85], b'A' | b'Q' | b'g' | b'w')
}
