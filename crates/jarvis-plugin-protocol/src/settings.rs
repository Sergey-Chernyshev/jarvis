use schemars::JsonSchema;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

use crate::validation::{is_canonical_dotted_id, is_safe_opaque_identifier};

const MAX_SETTING_KEY_BYTES: usize = 128;
const MAX_SETTING_VALUE_BYTES: usize = 64 * 1024;
const MAX_REFERENCE_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash, JsonSchema, Serialize)]
#[serde(transparent)]
pub struct SettingKey(
    #[schemars(schema_with = "crate::validation::namespaced_key_128_schema")] String,
);

impl SettingKey {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.len() > MAX_SETTING_KEY_BYTES || !is_canonical_dotted_id(&value, false) {
            return Err("invalid setting key");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SettingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, JsonSchema, Serialize)]
#[serde(transparent)]
pub struct ProjectId(
    #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")] String,
);

impl ProjectId {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        validate_reference(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProjectId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialReference {
    #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
    credential_id: String,
}

impl CredentialReference {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        validate_reference(&value)?;
        Ok(Self {
            credential_id: value,
        })
    }

    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }
}

impl<'de> Deserialize<'de> for CredentialReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            credential_id: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.credential_id).map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingScope {
    User,
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, JsonSchema, Serialize)]
#[serde(transparent)]
pub struct SettingString(
    #[schemars(schema_with = "crate::validation::setting_string_65536_schema")] String,
);

impl SettingString {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.len() > MAX_SETTING_VALUE_BYTES {
            return Err("setting value too large");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SettingString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingValue {
    Integer { value: i64 },
    Number { value: f64 },
    Boolean { value: bool },
    String { value: SettingString },
    CredentialReference { reference: CredentialReference },
}

impl SettingValue {
    pub fn string(value: impl Into<String>) -> Result<Self, &'static str> {
        Ok(Self::String {
            value: SettingString::new(value)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "scope",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingRecord {
    User {
        key: SettingKey,
        value: SettingValue,
        revision: u64,
    },
    Project {
        key: SettingKey,
        project_id: ProjectId,
        value: SettingValue,
        revision: u64,
    },
}

impl SettingRecord {
    pub fn scope(&self) -> SettingScope {
        match self {
            Self::User { .. } => SettingScope::User,
            Self::Project { .. } => SettingScope::Project,
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            Self::User { revision, .. } | Self::Project { revision, .. } => *revision,
        }
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        match self {
            Self::User { .. } => None,
            Self::Project { project_id, .. } => Some(project_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "scope",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingWrite {
    User {
        key: SettingKey,
        value: SettingValue,
        expected_revision: u64,
    },
    Project {
        key: SettingKey,
        project_id: ProjectId,
        value: SettingValue,
        expected_revision: u64,
    },
}

impl SettingWrite {
    pub fn scope(&self) -> SettingScope {
        match self {
            Self::User { .. } => SettingScope::User,
            Self::Project { .. } => SettingScope::Project,
        }
    }

    pub fn expected_revision(&self) -> u64 {
        match self {
            Self::User {
                expected_revision, ..
            }
            | Self::Project {
                expected_revision, ..
            } => *expected_revision,
        }
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        match self {
            Self::User { .. } => None,
            Self::Project { project_id, .. } => Some(project_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingChange {
    pub cursor: u64,
    pub setting: SettingRecord,
}

fn validate_reference(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > MAX_REFERENCE_BYTES
        || !is_safe_opaque_identifier(value)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
    {
        return Err("invalid setting reference");
    }
    Ok(())
}
