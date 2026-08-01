use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use schemars::JsonSchema;
use semver::{Version, VersionReq};
use serde::de::{self};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::json::{parse_bounded_json_with_limits, BoundedJsonError, JsonLimits};
use crate::validation::{
    is_canonical_dotted_id as valid_dotted_id, is_canonical_segment as valid_segment,
};

pub const MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const PLUGIN_API_VERSION: u32 = 2;
pub const MANIFEST_PROCESS_PROTOCOL: u32 = 2;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_JSON_DEPTH: usize = 64;
pub const MAX_JSON_NODES: usize = 20_000;
pub const MAX_JSON_STRING_BYTES: usize = 64 * 1024;
pub const MANIFEST_SCHEMA_JSON: &[u8] = include_bytes!("../schema/plugin-manifest-v2.schema.json");

const MAX_ID_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 256;
const MAX_EXPRESSION_BYTES: usize = 4 * 1024;
const MAX_CONTRIBUTIONS_PER_KIND: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    TooLarge,
    TooDeep,
    Schema,
    Semver,
    Incompatible,
    UnresolvedTarget,
}

impl ManifestError {
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "manifest_too_large",
            Self::TooDeep => "manifest_too_deep",
            Self::Schema => "manifest_schema",
            Self::Semver => "manifest_semver",
            Self::Incompatible => "manifest_incompatible",
            Self::UnresolvedTarget => "manifest_unresolved_target",
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Error for ManifestError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if value.len() > MAX_ID_BYTES || !valid_dotted_id(&value, true) {
            return Err(ManifestError::Schema);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PluginId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid plugin id"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PublisherId(String);

impl PublisherId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if value.len() > 64 || !valid_segment(&value) {
            return Err(ManifestError::Schema);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PublisherId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid publisher id"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        let valid = value
            .strip_prefix("sha256:")
            .map(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .unwrap_or(false);
        if !valid {
            return Err(ManifestError::Schema);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid sha256 digest"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContractId(String);

impl ContractId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        let Some((name, version)) = value.rsplit_once('@') else {
            return Err(ManifestError::Schema);
        };
        let Some((namespace, contract)) = name.rsplit_once('/') else {
            return Err(ManifestError::Schema);
        };
        if value.len() > 256
            || !valid_dotted_id(namespace, false)
            || !valid_dotted_id(contract, true)
            || Version::parse(version).is_err()
        {
            return Err(ManifestError::Schema);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn namespace(&self) -> &str {
        self.0
            .split_once('/')
            .map(|(namespace, _)| namespace)
            .expect("validated contract id always contains a namespace separator")
    }
}

impl<'de> Deserialize<'de> for ContractId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid contract id"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RelativePackagePath(String);

impl RelativePackagePath {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        if !valid_relative_path(&value) {
            return Err(ManifestError::Schema);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RelativePackagePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid relative package path"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRange {
    raw: String,
    requirement: VersionReq,
}

impl VersionRange {
    pub fn parse(raw: impl Into<String>) -> Result<Self, ManifestError> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > 256 {
            return Err(ManifestError::Semver);
        }
        let requirement = VersionReq::parse(&raw).map_err(|_| ManifestError::Semver)?;
        Ok(Self { raw, requirement })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.requirement.matches(version)
    }
}

impl Serialize for VersionRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for VersionRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(|_| de::Error::custom("invalid semantic version range"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestV2 {
    pub schema_version: u32,
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub publisher: PublisherId,
    pub compatibility: Compatibility,
    pub runtime: RuntimeDeclaration,
    pub permissions: Vec<PermissionDeclaration>,
    pub state: StateDeclaration,
    pub contributes: Contributions,
}

impl ManifestV2 {
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let value = parse_bounded_json(bytes)?;
        if value_contains_template(&value) {
            return Err(ManifestError::UnresolvedTarget);
        }
        let wire: ManifestWire =
            serde_json::from_value(value).map_err(|_| ManifestError::Schema)?;
        if wire.version.len() > 128 {
            return Err(ManifestError::Semver);
        }
        let manifest = Self {
            schema_version: wire.schema_version,
            id: wire.id,
            name: wire.name,
            version: Version::parse(&wire.version).map_err(|_| ManifestError::Semver)?,
            publisher: wire.publisher,
            compatibility: Compatibility {
                jarvis: VersionRange::parse(wire.compatibility.jarvis)?,
                plugin_api: wire.compatibility.plugin_api,
            },
            runtime: wire.runtime,
            permissions: wire.permissions,
            state: wire.state,
            contributes: wire.contributes,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::Schema);
        }
        if self.compatibility.plugin_api != PLUGIN_API_VERSION
            || self.runtime.protocol != MANIFEST_PROCESS_PROTOCOL
        {
            return Err(ManifestError::Incompatible);
        }
        if !self.id.as_str().contains('.') && self.publisher.as_str() != "jarvis-owner" {
            return Err(ManifestError::Schema);
        }
        bounded_nonempty(&self.name, MAX_TITLE_BYTES)?;
        self.runtime.validate()?;
        validate_permissions(&self.permissions)?;
        self.state.validate()?;
        self.contributes
            .validate(&self.id, &self.publisher, &self.runtime.activation_events)?;
        Ok(())
    }
}

pub fn parse_bounded_json(bytes: &[u8]) -> Result<Value, ManifestError> {
    parse_bounded_json_with_limits(
        bytes,
        JsonLimits {
            max_bytes: MAX_MANIFEST_BYTES,
            max_depth: MAX_JSON_DEPTH,
            max_nodes: MAX_JSON_NODES,
            max_string_bytes: MAX_JSON_STRING_BYTES,
        },
    )
    .map_err(|error| match error {
        BoundedJsonError::TooLarge => ManifestError::TooLarge,
        BoundedJsonError::TooDeep => ManifestError::TooDeep,
        BoundedJsonError::Invalid | BoundedJsonError::Io(_) => ManifestError::Schema,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestWire {
    schema_version: u32,
    id: PluginId,
    name: String,
    version: String,
    publisher: PublisherId,
    compatibility: CompatibilityWire,
    runtime: RuntimeDeclaration,
    permissions: Vec<PermissionDeclaration>,
    state: StateDeclaration,
    contributes: Contributions,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompatibilityWire {
    jarvis: String,
    plugin_api: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compatibility {
    pub jarvis: VersionRange,
    pub plugin_api: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeDeclaration {
    pub kind: RuntimeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<RuntimeLifecycle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_entry: Option<RelativePackagePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceDeclaration>,
    pub protocol: u32,
    pub activation_events: Vec<String>,
}

impl RuntimeDeclaration {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.activation_events.len() > MAX_CONTRIBUTIONS_PER_KIND {
            return Err(ManifestError::Schema);
        }
        match self.kind {
            RuntimeKind::UiOnly => {
                if self.lifecycle.is_some() || self.bridge_entry.is_some() || self.service.is_some()
                {
                    return Err(ManifestError::Schema);
                }
            }
            RuntimeKind::VerifiedNative => {
                if self.lifecycle != Some(RuntimeLifecycle::ServiceBridge)
                    || self.bridge_entry.is_none()
                    || self.service.is_none()
                {
                    return Err(ManifestError::Schema);
                }
            }
        }
        if let Some(service) = &self.service {
            validate_contribution_id(&service.id)?;
        }
        for event in &self.activation_events {
            bounded_nonempty(event, MAX_TITLE_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    UiOnly,
    VerifiedNative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeLifecycle {
    ServiceBridge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeclaration {
    pub id: String,
    pub manager: ServiceManager,
    pub entry: RelativePackagePath,
    pub survives_core_exit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceManager {
    LaunchdUser,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionDeclaration {
    pub id: PermissionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<PermissionScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<Vec<MountMode>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PermissionId {
    #[serde(rename = "projects.read")]
    ProjectsRead,
    #[serde(rename = "filesystem.mount")]
    FilesystemMount,
    #[serde(rename = "memory.read")]
    MemoryRead,
    #[serde(rename = "memory.propose-write")]
    MemoryProposeWrite,
    #[serde(rename = "notifications.publish")]
    NotificationsPublish,
    #[serde(rename = "credentials.request")]
    CredentialsRequest,
    #[serde(rename = "process.vm-provider")]
    ProcessVmProvider,
    #[serde(rename = "chat.compose.contribute")]
    ChatComposeContribute,
    #[serde(rename = "chat.composer.text.read")]
    ChatComposerTextRead,
    #[serde(rename = "projects.contribute")]
    ProjectsContribute,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionScope {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountMode {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateDeclaration {
    pub schema_version: u32,
    pub migrations: Vec<StateMigration>,
    pub rollback_compatible_through: u32,
}

impl StateDeclaration {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version == 0
            || self.rollback_compatible_through == 0
            || self.rollback_compatible_through > self.schema_version
            || self.migrations.len() > 128
        {
            return Err(ManifestError::Schema);
        }
        let mut edges = BTreeSet::new();
        for migration in &self.migrations {
            if migration.from == 0
                || migration.to == 0
                || migration.from >= migration.to
                || !edges.insert((migration.from, migration.to))
            {
                return Err(ManifestError::Schema);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateMigration {
    pub from: u32,
    pub to: u32,
    pub entry: RelativePackagePath,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Contributions {
    pub pages: Vec<PageContribution>,
    pub commands: Vec<CommandContribution>,
    pub actions: Vec<ActionContribution>,
    pub hotkeys: Vec<HotkeyContribution>,
    pub settings: Vec<SettingContribution>,
    pub project_runtimes: Vec<ProjectRuntimeContribution>,
    pub data_contracts: Vec<DataContractContribution>,
}

impl Contributions {
    fn validate(
        &self,
        plugin_id: &PluginId,
        publisher: &PublisherId,
        activation_events: &[String],
    ) -> Result<(), ManifestError> {
        for length in [
            self.pages.len(),
            self.commands.len(),
            self.actions.len(),
            self.hotkeys.len(),
            self.settings.len(),
            self.project_runtimes.len(),
            self.data_contracts.len(),
        ] {
            if length > MAX_CONTRIBUTIONS_PER_KIND {
                return Err(ManifestError::Schema);
            }
        }

        let mut all_ids = BTreeSet::new();
        let mut pages = BTreeSet::new();
        let mut commands = BTreeSet::new();
        let mut runtimes = BTreeSet::new();
        let mut contracts = BTreeSet::new();
        let mut command_contracts = BTreeSet::new();
        let declared_contract_namespace =
            declared_contract_namespace(plugin_id, publisher).ok_or(ManifestError::Schema)?;

        for page in &self.pages {
            validate_contribution_id(&page.id)?;
            validate_title(&page.title)?;
            validate_unique_list(&page.placements, 1, 4)?;
            if !all_ids.insert(page.id.as_str()) || !pages.insert(page.id.as_str()) {
                return Err(ManifestError::Schema);
            }
        }
        for command in &self.commands {
            validate_plugin_scoped_id(plugin_id, &command.id)?;
            validate_title(&command.title)?;
            validate_unique_list(&command.placements, 1, 1)?;
            if let Some(InvocationUi::SchemaForm {
                defaults_from_context,
            }) = &command.invocation_ui
            {
                validate_unique_list(defaults_from_context, 0, 5)?;
            }
            if let CommandHandler::RuntimeCommand { command } = &command.handler {
                validate_contribution_id(command)?;
            }
            if !all_ids.insert(command.id.as_str()) || !commands.insert(command.id.as_str()) {
                return Err(ManifestError::Schema);
            }
        }
        for action in &self.actions {
            validate_plugin_scoped_id(plugin_id, &action.id)?;
            validate_title(&action.title)?;
            bounded_nonempty(&action.icon, MAX_TITLE_BYTES)?;
            bounded_nonempty(&action.when, MAX_EXPRESSION_BYTES)?;
            validate_unique_list(&action.locations, 1, 3)?;
            validate_unique_list(&action.context, 0, 5)?;
            if !all_ids.insert(action.id.as_str()) {
                return Err(ManifestError::Schema);
            }
        }
        for setting in &self.settings {
            validate_plugin_scoped_id(plugin_id, setting.id())?;
            validate_title(setting.title())?;
            if !all_ids.insert(setting.id()) {
                return Err(ManifestError::Schema);
            }
            setting.validate()?;
        }
        for runtime in &self.project_runtimes {
            validate_plugin_scoped_id(plugin_id, &runtime.id)?;
            validate_title(&runtime.title)?;
            validate_unique_list(&runtime.project_kinds, 1, 1)?;
            if !all_ids.insert(runtime.id.as_str()) || !runtimes.insert(runtime.id.as_str()) {
                return Err(ManifestError::Schema);
            }
        }
        for contract in &self.data_contracts {
            if contract.id().namespace() != declared_contract_namespace
                || !all_ids.insert(contract.id().as_str())
                || !contracts.insert(contract.id().as_str())
            {
                return Err(ManifestError::Schema);
            }
            if matches!(contract, DataContractContribution::Command { .. }) {
                command_contracts.insert(contract.id().as_str());
            }
        }

        for command in &self.commands {
            if let CommandHandler::OpenPage { page } = &command.handler {
                if !pages.contains(page.as_str()) {
                    return Err(ManifestError::Schema);
                }
            }
        }
        for action in &self.actions {
            if !commands.contains(action.command.as_str()) {
                return Err(ManifestError::Schema);
            }
        }
        let mut hotkey_bindings = BTreeSet::new();
        for hotkey in &self.hotkeys {
            bounded_nonempty(&hotkey.default, MAX_ID_BYTES)?;
            if !commands.contains(hotkey.command.as_str())
                || !hotkey_bindings.insert((hotkey.command.as_str(), hotkey.scope))
            {
                return Err(ManifestError::Schema);
            }
        }
        for runtime in &self.project_runtimes {
            if !pages.contains(runtime.page.as_str()) {
                return Err(ManifestError::Schema);
            }
            if runtime
                .lifecycle_commands
                .iter()
                .into_iter()
                .any(|command| !command_contracts.contains(command.as_str()))
                || runtime
                    .contracts
                    .extensions()
                    .into_iter()
                    .any(|contract| !contracts.contains(contract.as_str()))
            {
                return Err(ManifestError::Schema);
            }
        }
        validate_activation_events(activation_events, &pages, &commands, &runtimes, &contracts)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageContribution {
    pub id: String,
    pub title: String,
    pub entry: RelativePackagePath,
    pub placements: Vec<PagePlacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params_schema: Option<RelativePackagePath>,
    pub instance_policy: InstancePolicy,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, JsonSchema, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum PagePlacement {
    Sidebar,
    CommandPalette,
    DeepLink,
    PluginSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstancePolicy {
    Singleton,
    PerProject,
    PerSession,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandContribution {
    pub id: String,
    pub title: String,
    pub risk: Risk,
    pub placements: Vec<CommandPlacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_schema: Option<RelativePackagePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<RelativePackagePath>,
    #[serde(rename = "invocationUI", skip_serializing_if = "Option::is_none")]
    pub invocation_ui: Option<InvocationUi>,
    pub handler: CommandHandler,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, JsonSchema, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum CommandPlacement {
    GlobalPalette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Read,
    Control,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CommandHandler {
    OpenPage { page: String },
    RuntimeCommand { command: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum InvocationUi {
    SchemaForm {
        defaults_from_context: Vec<ContextField>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionContribution {
    pub id: String,
    pub title: String,
    pub icon: String,
    pub locations: Vec<ActionLocation>,
    pub command: String,
    pub when: String,
    pub context: Vec<ContextField>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, JsonSchema, Serialize, Deserialize,
)]
pub enum ActionLocation {
    #[serde(rename = "chat.composer.actions")]
    ChatComposerActions,
    #[serde(rename = "project.actions")]
    ProjectActions,
    #[serde(rename = "project.session.context")]
    ProjectSessionContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContextField {
    #[serde(rename = "project.id")]
    ProjectId,
    #[serde(rename = "chat.id")]
    ChatId,
    #[serde(rename = "composer.text")]
    ComposerText,
    #[serde(rename = "runtime.id")]
    RuntimeId,
    #[serde(rename = "session.id")]
    SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HotkeyContribution {
    pub command: String,
    pub default: String,
    pub scope: HotkeyScope,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, JsonSchema, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum HotkeyScope {
    Global,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingContribution {
    Integer {
        id: String,
        title: String,
        default: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum: Option<i64>,
    },
    Number {
        id: String,
        title: String,
        default: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
    },
    Boolean {
        id: String,
        title: String,
        default: bool,
    },
    String {
        id: String,
        title: String,
        default: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum_length: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum_length: Option<usize>,
        #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
        allowed_values: Option<Vec<String>>,
    },
}

impl SettingContribution {
    fn id(&self) -> &str {
        match self {
            Self::Integer { id, .. }
            | Self::Number { id, .. }
            | Self::Boolean { id, .. }
            | Self::String { id, .. } => id,
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::Integer { title, .. }
            | Self::Number { title, .. }
            | Self::Boolean { title, .. }
            | Self::String { title, .. } => title,
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        match self {
            Self::Integer {
                default,
                minimum,
                maximum,
                ..
            } => validate_ordered_default(*default, *minimum, *maximum),
            Self::Number {
                default,
                minimum,
                maximum,
                ..
            } => {
                if !default.is_finite()
                    || minimum.map(|value| !value.is_finite()).unwrap_or(false)
                    || maximum.map(|value| !value.is_finite()).unwrap_or(false)
                {
                    return Err(ManifestError::Schema);
                }
                validate_ordered_default(*default, *minimum, *maximum)
            }
            Self::Boolean { .. } => Ok(()),
            Self::String {
                default,
                minimum_length,
                maximum_length,
                allowed_values,
                ..
            } => {
                if default.len() > 64 * 1024 {
                    return Err(ManifestError::Schema);
                }
                if let (Some(minimum), Some(maximum)) = (minimum_length, maximum_length) {
                    if minimum > maximum {
                        return Err(ManifestError::Schema);
                    }
                }
                if minimum_length
                    .map(|minimum| minimum > 64 * 1024)
                    .unwrap_or(false)
                    || maximum_length
                        .map(|maximum| maximum > 64 * 1024)
                        .unwrap_or(false)
                {
                    return Err(ManifestError::Schema);
                }
                let invalid_allowed_values = allowed_values
                    .as_ref()
                    .map(|values| {
                        let unique: BTreeSet<_> = values.iter().collect();
                        values.is_empty()
                            || values.len() > 256
                            || unique.len() != values.len()
                            || !values.iter().any(|value| value == default)
                    })
                    .unwrap_or(false);
                if minimum_length
                    .map(|minimum| default.chars().count() < minimum)
                    .unwrap_or(false)
                    || maximum_length
                        .map(|maximum| default.chars().count() > maximum)
                        .unwrap_or(false)
                    || invalid_allowed_values
                {
                    return Err(ManifestError::Schema);
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRuntimeContribution {
    pub id: String,
    pub title: String,
    pub project_kinds: Vec<ProjectKind>,
    pub page: String,
    pub provider_schema: ContractId,
    pub lifecycle_commands: LifecycleCommands,
    pub contracts: RuntimeContracts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    LocalFolder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleCommands {
    pub provision: ContractId,
    pub start: ContractId,
    pub stop: ContractId,
    pub destroy: ContractId,
    pub session_create: ContractId,
    pub session_stop: ContractId,
}

impl LifecycleCommands {
    fn iter(&self) -> [&ContractId; 6] {
        [
            &self.provision,
            &self.start,
            &self.stop,
            &self.destroy,
            &self.session_create,
            &self.session_stop,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeContracts {
    pub runtime: ContractBinding,
    pub session: ContractBinding,
    pub turn: ContractBinding,
}

impl RuntimeContracts {
    fn extensions(&self) -> [&ContractId; 3] {
        [
            &self.runtime.extension,
            &self.session.extension,
            &self.turn.extension,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractBinding {
    pub core: ContractId,
    pub extension: ContractId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DataContractContribution {
    Entity {
        id: ContractId,
        schema: RelativePackagePath,
        visibility: ContractVisibility,
        sensitivity: Sensitivity,
    },
    Command {
        id: ContractId,
        args_schema: RelativePackagePath,
        result_schema: RelativePackagePath,
        risk: Risk,
    },
}

impl DataContractContribution {
    fn id(&self) -> &ContractId {
        match self {
            Self::Entity { id, .. } | Self::Command { id, .. } => id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContractVisibility {
    Private,
    Granted,
    Public,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sensitivity {
    Public,
    Internal,
    Sensitive,
}

fn validate_permissions(permissions: &[PermissionDeclaration]) -> Result<(), ManifestError> {
    if permissions.len() > 128 {
        return Err(ManifestError::Schema);
    }
    let mut ids = BTreeSet::new();
    for permission in permissions {
        if !ids.insert(permission.id) {
            return Err(ManifestError::Schema);
        }
        match permission.id {
            PermissionId::ProjectsRead => {
                require_single_scope(permission, "selected")?;
                require_no_modes(permission)?;
            }
            PermissionId::FilesystemMount => {
                require_single_scope(permission, "selected")?;
                let Some(modes) = &permission.modes else {
                    return Err(ManifestError::Schema);
                };
                let unique: BTreeSet<_> = modes.iter().collect();
                if modes.is_empty() || modes.len() > 2 || unique.len() != modes.len() {
                    return Err(ManifestError::Schema);
                }
            }
            PermissionId::MemoryRead | PermissionId::MemoryProposeWrite => {
                require_multiple_scope(permission, &["global", "selected-project"])?;
                require_no_modes(permission)?;
            }
            PermissionId::CredentialsRequest => {
                require_multiple_scope(permission, &["claude", "codex"])?;
                require_no_modes(permission)?;
            }
            PermissionId::ChatComposerTextRead => {
                require_single_scope(permission, "invocation")?;
                require_no_modes(permission)?;
            }
            PermissionId::NotificationsPublish
            | PermissionId::ProcessVmProvider
            | PermissionId::ChatComposeContribute
            | PermissionId::ProjectsContribute => {
                if permission.scope.is_some() || permission.modes.is_some() {
                    return Err(ManifestError::Schema);
                }
            }
        }
    }
    Ok(())
}

fn require_single_scope(
    permission: &PermissionDeclaration,
    expected: &str,
) -> Result<(), ManifestError> {
    if !matches!(&permission.scope, Some(PermissionScope::One(value)) if value == expected) {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn require_multiple_scope(
    permission: &PermissionDeclaration,
    allowed: &[&str],
) -> Result<(), ManifestError> {
    let Some(PermissionScope::Many(values)) = &permission.scope else {
        return Err(ManifestError::Schema);
    };
    let unique: BTreeSet<_> = values.iter().map(String::as_str).collect();
    if values.is_empty()
        || values.len() > allowed.len()
        || unique.len() != values.len()
        || values
            .iter()
            .any(|value| !allowed.contains(&value.as_str()))
    {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn require_no_modes(permission: &PermissionDeclaration) -> Result<(), ManifestError> {
    if permission.modes.is_some() {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn validate_activation_events<'a>(
    events: &[String],
    pages: &BTreeSet<&'a str>,
    commands: &BTreeSet<&'a str>,
    runtimes: &BTreeSet<&'a str>,
    contracts: &BTreeSet<&'a str>,
) -> Result<(), ManifestError> {
    let mut unique = BTreeSet::new();
    for event in events {
        if !unique.insert(event.as_str()) {
            return Err(ManifestError::Schema);
        }
        if event == "onStartup" || event == "manual" {
            continue;
        }
        let Some((kind, reference)) = event.split_once(':') else {
            return Err(ManifestError::Schema);
        };
        let exists = match kind {
            "onPage" => pages.contains(reference),
            "onCommand" => commands.contains(reference),
            "onProjectRuntime" => runtimes.contains(reference),
            "onDataContract" => contracts.contains(reference),
            _ => false,
        };
        if !exists {
            return Err(ManifestError::Schema);
        }
    }
    Ok(())
}

fn validate_contribution_id(value: &str) -> Result<(), ManifestError> {
    if value.len() > MAX_ID_BYTES || !valid_dotted_id(value, true) {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn validate_plugin_scoped_id(plugin_id: &PluginId, value: &str) -> Result<(), ManifestError> {
    validate_contribution_id(value)?;
    let prefix = format!("{}.", plugin_id.as_str());
    if !value.starts_with(&prefix) {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn validate_unique_list<T>(
    values: &[T],
    minimum: usize,
    maximum: usize,
) -> Result<(), ManifestError>
where
    T: Ord,
{
    let unique: BTreeSet<_> = values.iter().collect();
    if values.len() < minimum || values.len() > maximum || unique.len() != values.len() {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn validate_title(value: &str) -> Result<(), ManifestError> {
    bounded_nonempty(value, MAX_TITLE_BYTES)
}

fn bounded_nonempty(value: &str, maximum: usize) -> Result<(), ManifestError> {
    if value.trim().is_empty() || value.len() > maximum {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn validate_ordered_default<T>(
    default: T,
    minimum: Option<T>,
    maximum: Option<T>,
) -> Result<(), ManifestError>
where
    T: Copy + PartialOrd,
{
    if minimum.map(|minimum| default < minimum).unwrap_or(false)
        || maximum.map(|maximum| default > maximum).unwrap_or(false)
        || matches!((minimum, maximum), (Some(minimum), Some(maximum)) if minimum > maximum)
    {
        return Err(ManifestError::Schema);
    }
    Ok(())
}

fn valid_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 1024
        || !value.nfc().eq(value.chars())
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '%' | '?' | '#' | ':'))
        || value.contains("//")
        || value.ends_with('/')
    {
        return false;
    }
    value
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn declared_contract_namespace(plugin_id: &PluginId, publisher: &PublisherId) -> Option<String> {
    if plugin_id.as_str().contains('.') {
        return Some(plugin_id.as_str().to_owned());
    }
    (publisher.as_str() == "jarvis-owner").then(|| format!("dev.jarvis.{}", plugin_id.as_str()))
}

fn value_contains_template(value: &Value) -> bool {
    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        match current {
            Value::String(value) => {
                if value.contains("${") {
                    return true;
                }
            }
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => {
                for (key, value) in values {
                    if key.contains("${") {
                        return true;
                    }
                    stack.push(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    false
}
