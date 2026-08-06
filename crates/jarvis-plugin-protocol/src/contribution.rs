use std::fmt;
use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::de::{self, SeqAccess, Visitor};
use serde::ser;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::manifest::{
    ActionLocation, CommandPlacement, HotkeyScope, InstancePolicy, PagePlacement, Risk,
};
use crate::validation::{is_canonical_dotted_id, is_safe_opaque_identifier};

const MAX_CONTRIBUTION_ID_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 256;
const MAX_CONTEXT_REFERENCES: usize = 16;
const MAX_PLACEMENTS: usize = 16;
const MAX_RESOLVED_CONTRIBUTIONS_PER_KIND: usize = 512;

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
        #[serde(
            deserialize_with = "deserialize_context_id",
            serialize_with = "serialize_context_id"
        )]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Chat {
        #[serde(
            deserialize_with = "deserialize_context_id",
            serialize_with = "serialize_context_id"
        )]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Runtime {
        #[serde(
            deserialize_with = "deserialize_context_id",
            serialize_with = "serialize_context_id"
        )]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
    Session {
        #[serde(
            deserialize_with = "deserialize_context_id",
            serialize_with = "serialize_context_id"
        )]
        #[schemars(schema_with = "crate::validation::opaque_id_128_no_at_schema")]
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedPageContribution {
    pub id: ContributionId,
    #[serde(
        deserialize_with = "deserialize_title",
        serialize_with = "serialize_title"
    )]
    #[schemars(schema_with = "crate::validation::contribution_title_256_schema")]
    pub title: String,
    #[serde(
        deserialize_with = "deserialize_page_placements",
        serialize_with = "serialize_placements"
    )]
    #[schemars(length(min = 1, max = 16))]
    pub placements: Vec<PagePlacement>,
    pub instance_policy: InstancePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCommandContribution {
    pub id: ContributionId,
    #[serde(
        deserialize_with = "deserialize_title",
        serialize_with = "serialize_title"
    )]
    #[schemars(schema_with = "crate::validation::contribution_title_256_schema")]
    pub title: String,
    #[serde(
        deserialize_with = "deserialize_command_placements",
        serialize_with = "serialize_placements"
    )]
    #[schemars(length(min = 1, max = 16))]
    pub placements: Vec<CommandPlacement>,
    pub risk_floor: Risk,
    #[serde(
        default,
        deserialize_with = "deserialize_context",
        serialize_with = "serialize_context"
    )]
    #[schemars(length(max = 16))]
    pub context: Vec<ContextReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedActionContribution {
    pub id: ContributionId,
    #[serde(
        deserialize_with = "deserialize_title",
        serialize_with = "serialize_title"
    )]
    #[schemars(schema_with = "crate::validation::contribution_title_256_schema")]
    pub title: String,
    #[serde(
        deserialize_with = "deserialize_action_locations",
        serialize_with = "serialize_placements"
    )]
    #[schemars(length(min = 1, max = 16))]
    pub locations: Vec<ActionLocation>,
    pub command: ContributionId,
    pub risk_floor: Risk,
    #[serde(
        default,
        deserialize_with = "deserialize_context",
        serialize_with = "serialize_context"
    )]
    #[schemars(length(max = 16))]
    pub context: Vec<ContextReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedHotkeyContribution {
    pub command: ContributionId,
    #[serde(
        deserialize_with = "deserialize_shortcut",
        serialize_with = "serialize_shortcut"
    )]
    #[schemars(schema_with = "crate::validation::contribution_shortcut_128_schema")]
    pub shortcut: String,
    pub scope: HotkeyScope,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedContributions {
    #[serde(
        default,
        deserialize_with = "deserialize_resolved_vec",
        serialize_with = "serialize_resolved_vec"
    )]
    #[schemars(length(max = 512))]
    pub pages: Vec<ResolvedPageContribution>,
    #[serde(
        default,
        deserialize_with = "deserialize_resolved_vec",
        serialize_with = "serialize_resolved_vec"
    )]
    #[schemars(length(max = 512))]
    pub commands: Vec<ResolvedCommandContribution>,
    #[serde(
        default,
        deserialize_with = "deserialize_resolved_vec",
        serialize_with = "serialize_resolved_vec"
    )]
    #[schemars(length(max = 512))]
    pub actions: Vec<ResolvedActionContribution>,
    #[serde(
        default,
        deserialize_with = "deserialize_resolved_vec",
        serialize_with = "serialize_resolved_vec"
    )]
    #[schemars(length(max = 512))]
    pub hotkeys: Vec<ResolvedHotkeyContribution>,
}

fn deserialize_title<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_title(&value).map_err(de::Error::custom)?;
    Ok(value)
}

fn deserialize_shortcut<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_shortcut(&value).map_err(de::Error::custom)?;
    Ok(value)
}

fn deserialize_context_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_context_id(&value).map_err(de::Error::custom)?;
    Ok(value)
}

fn deserialize_context<'de, D>(deserializer: D) -> Result<Vec<ContextReference>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        0,
        MAX_CONTEXT_REFERENCES,
        "context references",
    )
}

fn deserialize_page_placements<'de, D>(deserializer: D) -> Result<Vec<PagePlacement>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, 1, MAX_PLACEMENTS, "page placements")
}

fn deserialize_command_placements<'de, D>(
    deserializer: D,
) -> Result<Vec<CommandPlacement>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, 1, MAX_PLACEMENTS, "command placements")
}

fn deserialize_action_locations<'de, D>(deserializer: D) -> Result<Vec<ActionLocation>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, 1, MAX_PLACEMENTS, "action locations")
}

fn deserialize_resolved_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserialize_bounded_vec(
        deserializer,
        0,
        MAX_RESOLVED_CONTRIBUTIONS_PER_KIND,
        "resolved contributions of one kind",
    )
}

fn deserialize_bounded_vec<'de, D, T>(
    deserializer: D,
    minimum: usize,
    maximum: usize,
    expected: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_seq(BoundedVecVisitor {
        minimum,
        maximum,
        expected,
        marker: PhantomData,
    })
}

struct BoundedVecVisitor<T> {
    minimum: usize,
    maximum: usize,
    expected: &'static str,
    marker: PhantomData<T>,
}

impl<'de, T> Visitor<'de> for BoundedVecVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} with {}..={} items",
            self.expected, self.minimum, self.maximum
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if sequence
            .size_hint()
            .map(|size| size > self.maximum)
            .unwrap_or(false)
        {
            return Err(de::Error::custom("too many contribution items"));
        }

        let mut values = Vec::with_capacity(
            sequence
                .size_hint()
                .unwrap_or(self.minimum)
                .min(self.maximum),
        );
        while let Some(value) = sequence.next_element()? {
            if values.len() == self.maximum {
                return Err(de::Error::custom("too many contribution items"));
            }
            values.push(value);
        }
        if values.len() < self.minimum {
            return Err(de::Error::custom("too few contribution items"));
        }
        Ok(values)
    }
}

fn serialize_title<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_title(value).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_shortcut<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_shortcut(value).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_context_id<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_context_id(value).map_err(ser::Error::custom)?;
    value.serialize(serializer)
}

fn serialize_context<S>(values: &Vec<ContextReference>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serialize_bounded_vec(values, serializer, 0, MAX_CONTEXT_REFERENCES)
}

fn serialize_placements<S, T>(values: &Vec<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    serialize_bounded_vec(values, serializer, 1, MAX_PLACEMENTS)
}

fn serialize_resolved_vec<S, T>(values: &Vec<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    serialize_bounded_vec(values, serializer, 0, MAX_RESOLVED_CONTRIBUTIONS_PER_KIND)
}

fn serialize_bounded_vec<S, T>(
    values: &Vec<T>,
    serializer: S,
    minimum: usize,
    maximum: usize,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    if values.len() < minimum || values.len() > maximum {
        return Err(ser::Error::custom("invalid contribution item count"));
    }
    values.serialize(serializer)
}

fn validate_title(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_TITLE_BYTES || value.chars().any(char::is_control) {
        Err("invalid contribution title")
    } else {
        Ok(())
    }
}

fn validate_shortcut(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > MAX_CONTRIBUTION_ID_BYTES
        || value.chars().any(char::is_control)
    {
        Err("invalid contribution shortcut")
    } else {
        Ok(())
    }
}

fn validate_context_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > MAX_CONTRIBUTION_ID_BYTES
        || !is_safe_opaque_identifier(value)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
    {
        Err("invalid context reference")
    } else {
        Ok(())
    }
}
