use schemars::JsonSchema;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

use crate::validation::is_safe_opaque_identifier;

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
        if value.is_empty()
            || value.len() > MAX_SETTING_KEY_BYTES
            || !value.contains('.')
            || !value.split('.').all(valid_segment)
        {
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

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialReference {
    #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
    pub credential_id: String,
}

impl CredentialReference {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_REFERENCE_BYTES
            || !is_safe_opaque_identifier(&value)
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
            })
        {
            return Err("invalid credential reference");
        }
        Ok(Self {
            credential_id: value,
        })
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

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingValue {
    Integer {
        value: i64,
    },
    Number {
        value: f64,
    },
    Boolean {
        value: bool,
    },
    String {
        #[serde(deserialize_with = "deserialize_setting_string")]
        value: String,
    },
    CredentialReference {
        reference: CredentialReference,
    },
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingRecord {
    pub key: SettingKey,
    pub scope: SettingScope,
    #[serde(default)]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_128_no_at_schema")]
    pub project_id: Option<String>,
    pub value: SettingValue,
    pub revision: u64,
}

impl<'de> Deserialize<'de> for SettingRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            key: SettingKey,
            scope: SettingScope,
            project_id: Option<String>,
            value: SettingValue,
            revision: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let project_id = validate_scope(wire.scope, wire.project_id).map_err(de::Error::custom)?;
        Ok(Self {
            key: wire.key,
            scope: wire.scope,
            project_id,
            value: wire.value,
            revision: wire.revision,
        })
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingWrite {
    pub key: SettingKey,
    pub scope: SettingScope,
    #[serde(default)]
    #[schemars(schema_with = "crate::validation::optional_opaque_id_128_no_at_schema")]
    pub project_id: Option<String>,
    pub value: SettingValue,
    pub expected_revision: u64,
}

impl<'de> Deserialize<'de> for SettingWrite {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            key: SettingKey,
            scope: SettingScope,
            project_id: Option<String>,
            value: SettingValue,
            expected_revision: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let project_id = validate_scope(wire.scope, wire.project_id).map_err(de::Error::custom)?;
        Ok(Self {
            key: wire.key,
            scope: wire.scope,
            project_id,
            value: wire.value,
            expected_revision: wire.expected_revision,
        })
    }
}

#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingChange {
    pub cursor: u64,
    pub setting: SettingRecord,
}

fn deserialize_setting_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.len() > MAX_SETTING_VALUE_BYTES {
        return Err(de::Error::custom("setting value too large"));
    }
    Ok(value)
}

fn validate_scope(
    scope: SettingScope,
    project_id: Option<String>,
) -> Result<Option<String>, &'static str> {
    match (scope, project_id) {
        (SettingScope::User, None) => Ok(None),
        (SettingScope::Project, Some(project_id))
            if !project_id.is_empty()
                && project_id.len() <= MAX_REFERENCE_BYTES
                && is_safe_opaque_identifier(&project_id)
                && project_id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
                }) =>
        {
            Ok(Some(project_id))
        }
        _ => Err("setting scope does not match project reference"),
    }
}

fn valid_segment(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
