use schemars::JsonSchema;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

use crate::manifest::{
    ActionLocation, CommandPlacement, HotkeyScope, InstancePolicy, PagePlacement, Risk,
};
use crate::validation::{is_canonical_dotted_id, is_safe_opaque_identifier};

const MAX_CONTRIBUTION_ID_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 256;
const MAX_CONTEXT_REFERENCES: usize = 16;
const MAX_PLACEMENTS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash, JsonSchema, Serialize)]
#[serde(transparent)]
pub struct ContributionId(
    #[schemars(schema_with = "crate::validation::namespaced_key_128_schema")] String,
);

impl ContributionId {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_CONTRIBUTION_ID_BYTES
            || !is_canonical_dotted_id(&value, false)
        {
            return Err("invalid contribution id");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContributionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContextReference {
    Project {
        #[serde(deserialize_with = "deserialize_context_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Chat {
        #[serde(deserialize_with = "deserialize_context_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Runtime {
        #[serde(deserialize_with = "deserialize_context_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Session {
        #[serde(deserialize_with = "deserialize_context_id")]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedPageContribution {
    pub id: ContributionId,
    #[serde(deserialize_with = "deserialize_title")]
    pub title: String,
    #[serde(deserialize_with = "deserialize_page_placements")]
    pub placements: Vec<PagePlacement>,
    pub instance_policy: InstancePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCommandContribution {
    pub id: ContributionId,
    #[serde(deserialize_with = "deserialize_title")]
    pub title: String,
    #[serde(deserialize_with = "deserialize_command_placements")]
    pub placements: Vec<CommandPlacement>,
    pub risk_floor: Risk,
    #[serde(default, deserialize_with = "deserialize_context")]
    pub context: Vec<ContextReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedActionContribution {
    pub id: ContributionId,
    #[serde(deserialize_with = "deserialize_title")]
    pub title: String,
    #[serde(deserialize_with = "deserialize_action_locations")]
    pub locations: Vec<ActionLocation>,
    pub command: ContributionId,
    pub risk_floor: Risk,
    #[serde(default, deserialize_with = "deserialize_context")]
    pub context: Vec<ContextReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedHotkeyContribution {
    pub command: ContributionId,
    #[serde(deserialize_with = "deserialize_shortcut")]
    pub shortcut: String,
    pub scope: HotkeyScope,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedContributions {
    #[serde(default)]
    pub pages: Vec<ResolvedPageContribution>,
    #[serde(default)]
    pub commands: Vec<ResolvedCommandContribution>,
    #[serde(default)]
    pub actions: Vec<ResolvedActionContribution>,
    #[serde(default)]
    pub hotkeys: Vec<ResolvedHotkeyContribution>,
}

fn deserialize_title<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() || value.len() > MAX_TITLE_BYTES || value.chars().any(char::is_control) {
        return Err(de::Error::custom("invalid contribution title"));
    }
    Ok(value)
}

fn deserialize_shortcut<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty()
        || value.len() > MAX_CONTRIBUTION_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(de::Error::custom("invalid contribution shortcut"));
    }
    Ok(value)
}

fn deserialize_context_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty()
        || value.len() > MAX_CONTRIBUTION_ID_BYTES
        || !is_safe_opaque_identifier(&value)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
    {
        return Err(de::Error::custom("invalid context reference"));
    }
    Ok(value)
}

fn deserialize_context<'de, D>(deserializer: D) -> Result<Vec<ContextReference>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<ContextReference>::deserialize(deserializer)?;
    if values.len() > MAX_CONTEXT_REFERENCES {
        return Err(de::Error::custom("too many context references"));
    }
    Ok(values)
}

fn deserialize_page_placements<'de, D>(deserializer: D) -> Result<Vec<PagePlacement>, D::Error>
where
    D: Deserializer<'de>,
{
    bounded_nonempty_vec(deserializer)
}

fn deserialize_command_placements<'de, D>(
    deserializer: D,
) -> Result<Vec<CommandPlacement>, D::Error>
where
    D: Deserializer<'de>,
{
    bounded_nonempty_vec(deserializer)
}

fn deserialize_action_locations<'de, D>(deserializer: D) -> Result<Vec<ActionLocation>, D::Error>
where
    D: Deserializer<'de>,
{
    bounded_nonempty_vec(deserializer)
}

fn bounded_nonempty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let values = Vec::<T>::deserialize(deserializer)?;
    if values.is_empty() || values.len() > MAX_PLACEMENTS {
        return Err(de::Error::custom("invalid contribution placements"));
    }
    Ok(values)
}
