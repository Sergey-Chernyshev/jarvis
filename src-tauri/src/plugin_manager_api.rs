//! Shared typed Plugin Manager boundary used by Tauri IPC and the standalone CLI.
//!
//! All callers submit the same `ManagerRequest`. Package lifecycle mutations are
//! delegated to the durable A6 manager; local developer commands are delegated
//! to the immutable A7 snapshot linker.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use jarvis_package::{
    pack_plugin, PackOptions, PackageDocumentAdapter, PackageError, PackageSignatureSource,
};
use jarvis_plugin_protocol::manifest::{
    Digest, ManifestV2, PluginId, RuntimeKind, PLUGIN_API_VERSION,
};
use jarvis_plugin_protocol::operation::{Operation, OperationState};
use jarvis_plugin_protocol::package::{
    MacOsVersion, PackageSignatureV1, PackageTarget, SignatureAlgorithm,
};
use jarvis_plugin_protocol::receipt::{GrantedPermission, InstallReceipt, InstallSource};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::plugins::developer::{
    DeveloperLink, DeveloperLinker, DeveloperPackageOptions, DeveloperTeardownPort,
    NativeDigestConsent,
};
use crate::plugins::manifest_v2::HostCompatibility;
use crate::plugins::package::HostPackageDocumentAdapter;
use crate::plugins::package_manager::consent::Approval;
use crate::plugins::package_manager::downloader::HttpDownloader;
use crate::plugins::package_manager::health::NativeHealthRunner;
use crate::plugins::package_manager::lock::ManagerLock;
use crate::plugins::package_manager::manager::{
    CatalogItem, CatalogProvider, CatalogQuery, DoctorReport, InstallPlan, InstallSourceRef,
    LifecycleHost, ManagerError, ManagerResult, PackageManagerApi, PluginDetails, PluginManager,
    StrictPackageEngine, SystemClock, TeardownStatus,
};
use crate::plugins::package_manager::migration::RefuseSchemaChanges;
use crate::plugins::package_manager::paths::PluginPaths;
use crate::plugins::package_manager::receipt::ReceiptStore;
use crate::plugins::trust::provider::{
    record_developer_snapshot_evidence, ProductionCatalogProvider,
};
use crate::settings;

const DEVELOPER_REGISTRY_SCHEMA: u32 = 1;
const DEVELOPER_PACK_KEY_ID: &str = "jarvis.developer-unverified";
const MAX_LOG_BYTES: u64 = 256 * 1024;
const MAX_LOG_LINES: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ManagerRequest {
    Catalog {
        query: Option<String>,
    },
    Info {
        plugin_id: String,
    },
    PrepareInstall {
        source: String,
    },
    CommitInstall {
        operation_id: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
        approve_irreversible_migration: bool,
    },
    Update {
        plugin_id: Option<String>,
    },
    Rollback {
        plugin_id: String,
        version: Option<String>,
    },
    Enable {
        plugin_id: String,
    },
    Disable {
        plugin_id: String,
    },
    Uninstall {
        plugin_id: String,
    },
    Purge {
        plugin_id: String,
        confirmation: String,
    },
    Doctor {
        plugin_id: Option<String>,
    },
    Validate {
        source: String,
    },
    Pack {
        source: String,
        output: Option<String>,
    },
    Link {
        source: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
    },
    Unlink {
        plugin_id: String,
    },
    Reload {
        plugin_id: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
    },
    Logs {
        plugin_id: String,
    },
    List {
        developer_only: bool,
    },
    DeveloperMode {
        enabled: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSummary {
    pub plugin_id: PluginId,
    pub version: Version,
    pub source: InstallSource,
    pub enabled: bool,
    pub package_digest: Digest,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeveloperPlan {
    pub plugin_id: PluginId,
    pub package_digest: Digest,
    pub snapshot: PathBuf,
    pub added_permissions: Vec<GrantedPermission>,
    pub removed_permissions: Vec<GrantedPermission>,
    pub native_consent_required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ManagerResponse {
    Catalog {
        items: Vec<CatalogItem>,
    },
    Info {
        details: PluginDetails,
    },
    InstallPlan {
        plan: InstallPlan,
    },
    Receipt {
        receipt: InstallReceipt,
    },
    Operation {
        operation: Operation,
    },
    Operations {
        operations: Vec<Operation>,
    },
    Doctor {
        report: DoctorReport,
    },
    Validated {
        manifest: ManifestV2,
    },
    Packed {
        plugin_id: PluginId,
        version: Version,
        target: PackageTarget,
        output: PathBuf,
        package_digest: Digest,
        trust: String,
    },
    DeveloperPlan {
        plan: DeveloperPlan,
    },
    DeveloperLinked {
        receipt: InstallReceipt,
        snapshot: PathBuf,
    },
    DeveloperUnlinked {
        plugin_id: PluginId,
        generation: u64,
    },
    Logs {
        path: PathBuf,
        lines: Vec<String>,
    },
    List {
        plugins: Vec<PluginSummary>,
    },
    DeveloperMode {
        enabled: bool,
        revoked_links: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagerApiError {
    pub code: String,
    pub message: String,
}

impl ManagerApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn requires_explicit_consent(&self) -> bool {
        self.code.contains("consent")
            || self.code.contains("permission")
            || self.code == "irreversible_migration_consent_required"
    }
}

impl std::fmt::Display for ManagerApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ManagerApiError {}

impl From<ManagerError> for ManagerApiError {
    fn from(error: ManagerError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

pub trait PluginManagementApi: Send + Sync {
    fn request(&self, request: ManagerRequest) -> Result<ManagerResponse, ManagerApiError>;
}

pub fn dispatch_manager_request(
    api: &dyn PluginManagementApi,
    request: ManagerRequest,
) -> Result<ManagerResponse, ManagerApiError> {
    api.request(request)
}

pub fn dispatch_ipc(
    request: ManagerRequest,
    api: &dyn PluginManagementApi,
) -> Result<ManagerResponse, ManagerApiError> {
    dispatch_manager_request(api, request)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeveloperRecord {
    source: PathBuf,
    snapshot: PathBuf,
    receipt: InstallReceipt,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeveloperRegistry {
    schema_version: u32,
    links: BTreeMap<String, DeveloperRecord>,
}

pub struct PluginManagerEndpoint {
    profile: PathBuf,
    settings: settings::Store,
    package: Result<Arc<dyn PackageManagerApi>, ManagerApiError>,
    lifecycle: Arc<dyn LifecycleHost>,
    developer_teardown: Arc<dyn DeveloperTeardownPort>,
    receipts: ReceiptStore,
    adapter: HostPackageDocumentAdapter,
    developer: DeveloperLinker<HostPackageDocumentAdapter>,
    active_developer_links: Mutex<BTreeMap<String, DeveloperLink>>,
    developer_registry: Mutex<DeveloperRegistry>,
}

impl PluginManagerEndpoint {
    pub fn new(settings: settings::Store) -> Result<Self, ManagerApiError> {
        Self::new_with_components(
            crate::util::jarvis_dir(),
            settings,
            Arc::new(InactiveLifecycle),
            Arc::new(InactiveDeveloperTeardown),
        )
    }

    pub fn new_with_host(
        settings: settings::Store,
        host: Arc<crate::plugins::PluginHost>,
        tokens: Arc<crate::capability::tokens::TokenStore>,
        catalog: Arc<dyn CatalogProvider>,
    ) -> Result<Self, ManagerApiError> {
        let bridge = Arc::new(HostLifecycleBridge { host, tokens });
        Self::new_with_components_and_catalog(
            crate::util::jarvis_dir(),
            settings,
            bridge.clone(),
            bridge,
            catalog,
        )
    }

    fn new_with_components(
        profile: PathBuf,
        settings: settings::Store,
        lifecycle: Arc<dyn LifecycleHost>,
        developer_teardown: Arc<dyn DeveloperTeardownPort>,
    ) -> Result<Self, ManagerApiError> {
        let catalog_compatibility = crate::plugins::trust::catalog::CatalogCompatibility::parse(
            env!("CARGO_PKG_VERSION"),
            PLUGIN_API_VERSION,
            current_target(),
            "13.0.0",
        )
        .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        let catalog = Arc::new(ProductionCatalogProvider::for_profile(
            PluginPaths::new(profile.clone()),
            catalog_compatibility,
            Arc::new(SystemClock),
        ));
        Self::new_with_components_and_catalog(
            profile,
            settings,
            lifecycle,
            developer_teardown,
            catalog,
        )
    }

    fn new_with_components_and_catalog(
        profile: PathBuf,
        settings: settings::Store,
        lifecycle: Arc<dyn LifecycleHost>,
        developer_teardown: Arc<dyn DeveloperTeardownPort>,
        catalog: Arc<dyn CatalogProvider>,
    ) -> Result<Self, ManagerApiError> {
        let compatibility = HostCompatibility::parse(env!("CARGO_PKG_VERSION"), PLUGIN_API_VERSION)
            .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        let adapter = HostPackageDocumentAdapter::new(compatibility.clone());
        let options = DeveloperPackageOptions {
            target: current_target(),
            minimum_macos: MacOsVersion::parse("13.0.0")
                .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?,
        };
        let developer = DeveloperLinker::new(
            profile.clone(),
            adapter.clone(),
            options,
            settings.bool("pluginDeveloperMode"),
        )
        .map_err(developer_error)?;
        let paths = PluginPaths::new(profile.clone());
        paths
            .prepare()
            .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        let receipts = ReceiptStore::new(paths.clone());
        let package = build_package_manager(paths, compatibility, lifecycle.clone(), catalog)
            .map(|manager| Arc::new(manager) as Arc<dyn PackageManagerApi>)
            .map_err(ManagerApiError::from);
        let developer_registry = read_developer_registry(&profile)?;
        Ok(Self {
            profile,
            settings,
            package,
            lifecycle,
            developer_teardown,
            receipts,
            adapter,
            developer,
            active_developer_links: Mutex::new(BTreeMap::new()),
            developer_registry: Mutex::new(developer_registry),
        })
    }

    fn package(&self) -> Result<&dyn PackageManagerApi, ManagerApiError> {
        self.package.as_deref().map_err(Clone::clone)
    }

    fn plugin_id(raw: &str) -> Result<PluginId, ManagerApiError> {
        PluginId::new(raw.to_owned()).map_err(|error| {
            ManagerApiError::new(error.code(), format!("invalid plugin id: {raw}"))
        })
    }

    fn digest(raw: Option<&str>) -> Result<Option<Digest>, ManagerApiError> {
        raw.map(|value| {
            Digest::new(value.to_owned()).map_err(|error| {
                ManagerApiError::new(error.code(), "invalid exact native SHA-256 digest")
            })
        })
        .transpose()
    }

    fn catalog_source(raw: &str) -> Result<InstallSourceRef, ManagerApiError> {
        if Path::new(raw).components().count() > 1 || raw.ends_with(".jarvis-plugin") {
            return Err(ManagerApiError::new(
                "package_file_install_unavailable",
                "local archives require a trusted publisher/catalog import",
            ));
        }
        let (id, version) = raw
            .rsplit_once('@')
            .map(|(id, version)| (id.to_owned(), Some(version.to_owned())))
            .unwrap_or_else(|| (raw.to_owned(), None));
        let plugin_id = Self::plugin_id(&id)?;
        if let Some(version) = &version {
            Version::parse(version)
                .map_err(|_| ManagerApiError::new("package_version", "invalid package version"))?;
        }
        Ok(InstallSourceRef::Catalog {
            id: plugin_id.as_str().to_owned(),
            version,
        })
    }

    fn prepare_install(&self, source: String) -> Result<ManagerResponse, ManagerApiError> {
        let plan = self
            .package()?
            .prepare_install(Self::catalog_source(&source)?)?;
        Ok(ManagerResponse::InstallPlan { plan })
    }

    fn commit_install(
        &self,
        operation_id: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
        approve_irreversible_migration: bool,
    ) -> Result<ManagerResponse, ManagerApiError> {
        if !accept_permissions {
            return Err(ManagerApiError::new(
                "permission_consent_required",
                "repeat with --accept-permissions after reviewing the install plan",
            ));
        }
        let package = self.package()?;
        let plan = package.prepared_install(&operation_id)?;
        let native = Self::digest(trust_native_digest.as_deref())?;
        if plan.native_trust_digest != native {
            return Err(ManagerApiError::new(
                "native_digest_consent_mismatch",
                "pass the exact digest printed by the prepare operation",
            ));
        }
        if plan.irreversible_migration && !approve_irreversible_migration {
            return Err(ManagerApiError::new(
                "irreversible_migration_consent_required",
                "repeat with --approve-irreversible-migration",
            ));
        }
        let approval = Approval {
            operation_id,
            package_digest: plan.package_digest.clone(),
            granted_permissions: plan.requested_permissions.clone(),
            native_trust_digest: native,
            approve_irreversible_migration,
        };
        let receipt = package.commit_install(approval)?;
        Ok(ManagerResponse::Receipt { receipt })
    }

    fn validate_source(&self, source: &str) -> Result<ManifestV2, ManagerApiError> {
        let source = canonical_source(source)?;
        let bytes = fs::read(source.join("plugin.json")).map_err(|error| {
            ManagerApiError::new(
                "manifest_read",
                format!("cannot read {}/plugin.json: {error}", source.display()),
            )
        })?;
        self.adapter
            .resolve_source_manifest(&bytes, current_target())
            .map_err(package_error)
    }

    fn pack_source(
        &self,
        source: String,
        output: Option<String>,
    ) -> Result<ManagerResponse, ManagerApiError> {
        let source = canonical_source(&source)?;
        let manifest = self.validate_source(source.to_string_lossy().as_ref())?;
        let output = match output {
            Some(output) => absolutize(&output)?,
            None => source
                .parent()
                .unwrap_or_else(|| Path::new("/"))
                .join(format!(
                    "{}_{}_{}.jarvis-plugin",
                    manifest.id.as_str(),
                    manifest.version,
                    current_target().as_str()
                )),
        };
        if output.starts_with(&source) {
            return Err(ManagerApiError::new(
                "package_output_inside_source",
                "package output must be outside the source tree",
            ));
        }
        let parent = output.parent().ok_or_else(|| {
            ManagerApiError::new("package_output", "package output has no parent")
        })?;
        if !parent.is_dir() {
            return Err(ManagerApiError::new(
                "package_output",
                format!("output parent does not exist: {}", parent.display()),
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&output)
            .map_err(|error| {
                ManagerApiError::new(
                    "package_output",
                    format!("cannot create {}: {error}", output.display()),
                )
            })?;
        let result = pack_plugin(
            &source,
            PackOptions {
                target: current_target(),
                minimum_macos: MacOsVersion::parse("13.0.0")
                    .expect("the bundled minimum macOS version is valid"),
            },
            &self.adapter,
            &DeveloperPackSignature,
            &mut file,
        )
        .map_err(package_error);
        let digest = match result {
            Ok(digest) => {
                file.sync_all().map_err(|error| {
                    ManagerApiError::new("package_output", format!("cannot sync package: {error}"))
                })?;
                digest
            }
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(&output);
                return Err(error);
            }
        };
        Ok(ManagerResponse::Packed {
            plugin_id: manifest.id,
            version: manifest.version,
            target: current_target(),
            output,
            package_digest: digest,
            trust: "developer-unverified".into(),
        })
    }

    fn developer_link(
        &self,
        source: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
    ) -> Result<ManagerResponse, ManagerApiError> {
        let _manager_lock = self.developer_lock()?;
        let source = canonical_source(&source)?;
        let prepared = self
            .developer
            .prepare_link(&source)
            .map_err(developer_error)?;
        let requested = grants(prepared.manifest());
        let plan = DeveloperPlan {
            plugin_id: prepared.manifest().id.clone(),
            package_digest: prepared.package_digest().clone(),
            snapshot: prepared.snapshot().to_path_buf(),
            added_permissions: requested.clone(),
            removed_permissions: Vec::new(),
            native_consent_required: prepared.manifest().runtime.kind
                == RuntimeKind::VerifiedNative,
        };
        if !requested.is_empty() && !accept_permissions {
            return Ok(ManagerResponse::DeveloperPlan { plan });
        }
        let consent = native_consent(
            prepared.manifest().runtime.kind,
            prepared.package_digest(),
            trust_native_digest.as_deref(),
        )?;
        let link = self
            .developer
            .commit_link(prepared, consent)
            .map_err(developer_error)?;
        self.persist_developer_link(source, link)
    }

    fn reload_developer(
        &self,
        plugin_id: String,
        accept_permissions: bool,
        trust_native_digest: Option<String>,
    ) -> Result<ManagerResponse, ManagerApiError> {
        let _manager_lock = self.developer_lock()?;
        let plugin_id = Self::plugin_id(&plugin_id)?;
        let record = self
            .developer_registry
            .lock()
            .unwrap()
            .links
            .get(plugin_id.as_str())
            .cloned()
            .ok_or_else(|| ManagerApiError::new("developer_link_not_found", plugin_id.as_str()))?;
        let active = self
            .active_developer_links
            .lock()
            .unwrap()
            .get(plugin_id.as_str())
            .cloned();

        if let Some(active) = active {
            let plan = self
                .developer
                .prepare_reload(&active)
                .map_err(developer_error)?;
            let added = plan.permission_diff().added().to_vec();
            let removed = plan.permission_diff().removed().to_vec();
            let response_plan = DeveloperPlan {
                plugin_id: plugin_id.clone(),
                package_digest: plan.package_digest().clone(),
                snapshot: record.snapshot,
                added_permissions: added.clone(),
                removed_permissions: removed.clone(),
                native_consent_required: active.runtime_kind() == RuntimeKind::VerifiedNative,
            };
            if (!added.is_empty() || !removed.is_empty() || plan.changed()) && !accept_permissions {
                return Ok(ManagerResponse::DeveloperPlan {
                    plan: response_plan,
                });
            }
            let consent = native_consent(
                active.runtime_kind(),
                plan.package_digest(),
                trust_native_digest.as_deref(),
            )?;
            let approval = plan.changed().then(|| plan.approval());
            self.developer_teardown
                .teardown_and_revoke(&active)
                .map_err(|error| {
                    ManagerApiError::new(
                        "developer_teardown_failed",
                        format!("cannot teardown {}: {error}", plugin_id.as_str()),
                    )
                })?;
            let link = self
                .developer
                .commit_reload(&active, plan, approval, consent)
                .map_err(developer_error)?;
            return self.persist_developer_link(record.source, link);
        }

        let prepared = self
            .developer
            .prepare_link(&record.source)
            .map_err(developer_error)?;
        let next_permissions = grants(prepared.manifest());
        let (added, removed) =
            permission_diff(&record.receipt.granted_permissions, &next_permissions);
        let changed = record.receipt.package_digest != *prepared.package_digest();
        let response_plan = DeveloperPlan {
            plugin_id: plugin_id.clone(),
            package_digest: prepared.package_digest().clone(),
            snapshot: prepared.snapshot().to_path_buf(),
            added_permissions: added.clone(),
            removed_permissions: removed.clone(),
            native_consent_required: prepared.manifest().runtime.kind
                == RuntimeKind::VerifiedNative,
        };
        if (changed || !added.is_empty() || !removed.is_empty()) && !accept_permissions {
            return Ok(ManagerResponse::DeveloperPlan {
                plan: response_plan,
            });
        }
        let consent = native_consent(
            prepared.manifest().runtime.kind,
            prepared.package_digest(),
            trust_native_digest.as_deref(),
        )?;
        if self.lifecycle.teardown(&plugin_id)? == TeardownStatus::Busy {
            return Err(ManagerApiError::new(
                "developer_teardown_busy",
                format!("plugin {} still owns live resources", plugin_id.as_str()),
            ));
        }
        let link = self
            .developer
            .commit_replacement(&record.receipt, prepared, consent)
            .map_err(developer_error)?;
        self.persist_developer_link(record.source, link)
    }

    fn persist_developer_link(
        &self,
        source: PathBuf,
        link: DeveloperLink,
    ) -> Result<ManagerResponse, ManagerApiError> {
        record_developer_snapshot_evidence(
            &PluginPaths::new(self.profile.clone()),
            link.receipt(),
            link.snapshot(),
            link.metadata(),
        )
        .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        self.receipts
            .commit(link.receipt())
            .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        let record = DeveloperRecord {
            source,
            snapshot: link.snapshot().to_path_buf(),
            receipt: link.receipt().clone(),
        };
        {
            let mut registry = self.developer_registry.lock().unwrap();
            registry
                .links
                .insert(record.receipt.plugin_id.as_str().to_owned(), record.clone());
            write_developer_registry(&self.profile, &registry)?;
        }
        self.active_developer_links
            .lock()
            .unwrap()
            .insert(record.receipt.plugin_id.as_str().to_owned(), link);
        self.lifecycle
            .resume_activation(&record.receipt.plugin_id)?;
        Ok(ManagerResponse::DeveloperLinked {
            receipt: record.receipt,
            snapshot: record.snapshot,
        })
    }

    fn unlink_developer(&self, raw: String) -> Result<ManagerResponse, ManagerApiError> {
        let _manager_lock = self.developer_lock()?;
        let plugin_id = Self::plugin_id(&raw)?;
        let active = self
            .active_developer_links
            .lock()
            .unwrap()
            .get(plugin_id.as_str())
            .cloned();
        let mut next_registry = self.developer_registry.lock().unwrap().clone();
        let record = next_registry
            .links
            .get(plugin_id.as_str())
            .cloned()
            .ok_or_else(|| ManagerApiError::new("developer_link_not_found", plugin_id.as_str()))?;
        if let Some(link) = active.as_ref() {
            self.developer_teardown
                .teardown_and_revoke(link)
                .map_err(|error| {
                    ManagerApiError::new(
                        "developer_teardown_failed",
                        format!("cannot teardown {}: {error}", plugin_id.as_str()),
                    )
                })?;
        } else if self.lifecycle.teardown(&plugin_id)? == TeardownStatus::Busy {
            return Err(ManagerApiError::new(
                "developer_teardown_busy",
                format!("plugin {} still owns live resources", plugin_id.as_str()),
            ));
        }
        let mut receipt = record.receipt;
        let previous = receipt.summary();
        receipt.enabled = false;
        receipt.generation = receipt.generation.saturating_add(1);
        receipt.previous = Some(previous);
        self.receipts
            .commit(&receipt)
            .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
        next_registry.links.remove(plugin_id.as_str());
        write_developer_registry(&self.profile, &next_registry)?;
        *self.developer_registry.lock().unwrap() = next_registry;
        self.active_developer_links
            .lock()
            .unwrap()
            .remove(plugin_id.as_str());
        self.persist_plugin_enabled(&plugin_id, false)?;
        Ok(ManagerResponse::DeveloperUnlinked {
            plugin_id,
            generation: receipt.generation,
        })
    }

    fn set_developer_mode(&self, enabled: bool) -> Result<ManagerResponse, ManagerApiError> {
        let _manager_lock = self.developer_lock()?;
        if enabled {
            self.developer.enable_mode();
            self.settings
                .set_top("pluginDeveloperMode", serde_json::Value::Bool(true));
            if !self.settings.bool("pluginDeveloperMode") {
                let _ = self.developer.disable_mode(&[], &InactiveDeveloperTeardown);
                return Err(ManagerApiError::new(
                    "settings_write",
                    "cannot persist Developer Mode",
                ));
            }
            return Ok(ManagerResponse::DeveloperMode {
                enabled: true,
                revoked_links: 0,
            });
        }

        let links = self
            .active_developer_links
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let active_ids = links
            .iter()
            .map(|link| link.receipt().plugin_id.as_str().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let inactive_enabled = self
            .developer_registry
            .lock()
            .unwrap()
            .links
            .values()
            .filter(|record| {
                record.receipt.enabled && !active_ids.contains(record.receipt.plugin_id.as_str())
            })
            .map(|record| record.receipt.plugin_id.clone())
            .collect::<Vec<_>>();
        for plugin_id in inactive_enabled {
            if self.lifecycle.teardown(&plugin_id)? == TeardownStatus::Busy {
                return Err(ManagerApiError::new(
                    "developer_teardown_busy",
                    format!("plugin {} still owns live resources", plugin_id.as_str()),
                ));
            }
        }
        let report = self
            .developer
            .disable_mode(&links, self.developer_teardown.as_ref())
            .map_err(developer_error)?;
        self.active_developer_links.lock().unwrap().clear();
        {
            let mut registry = self.developer_registry.lock().unwrap();
            for record in registry.links.values_mut() {
                if record.receipt.enabled {
                    let previous = record.receipt.summary();
                    record.receipt.enabled = false;
                    record.receipt.generation = record.receipt.generation.saturating_add(1);
                    record.receipt.previous = Some(previous);
                    self.receipts
                        .commit(&record.receipt)
                        .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?;
                }
            }
            write_developer_registry(&self.profile, &registry)?;
        }
        self.settings
            .set_top("pluginDeveloperMode", serde_json::Value::Bool(false));
        if self.settings.bool("pluginDeveloperMode") {
            return Err(ManagerApiError::new(
                "settings_write",
                "cannot persist disabled Developer Mode",
            ));
        }
        Ok(ManagerResponse::DeveloperMode {
            enabled: false,
            revoked_links: report.revoked_links,
        })
    }

    fn set_package_enabled(
        &self,
        raw: String,
        enabled: bool,
    ) -> Result<ManagerResponse, ManagerApiError> {
        let plugin_id = Self::plugin_id(&raw)?;
        let operation = self.package()?.set_enabled(&plugin_id, enabled)?;
        if operation.state == OperationState::Succeeded {
            self.persist_plugin_enabled(&plugin_id, enabled)?;
        }
        Ok(ManagerResponse::Operation { operation })
    }

    fn persist_plugin_enabled(
        &self,
        plugin_id: &PluginId,
        enabled: bool,
    ) -> Result<(), ManagerApiError> {
        let mut patch = serde_json::Map::new();
        patch.insert("enabled".into(), serde_json::Value::Bool(enabled));
        self.settings.set_plugin(plugin_id.as_str(), patch);
        if self
            .settings
            .load()
            .pointer(&format!("/plugins/{}/enabled", plugin_id.as_str()))
            .and_then(serde_json::Value::as_bool)
            != Some(enabled)
        {
            return Err(ManagerApiError::new(
                "settings_write",
                format!(
                    "cannot persist enabled={enabled} for plugin {}",
                    plugin_id.as_str()
                ),
            ));
        }
        Ok(())
    }

    fn list_plugins(&self, developer_only: bool) -> Result<ManagerResponse, ManagerApiError> {
        let mut summaries = BTreeMap::<String, PluginSummary>::new();
        for record in self.developer_registry.lock().unwrap().links.values() {
            summaries.insert(
                record.receipt.plugin_id.as_str().to_owned(),
                summary(&record.receipt),
            );
        }
        if !developer_only {
            let plugins_root = self.profile.join("plugins");
            if let Ok(entries) = fs::read_dir(plugins_root) {
                for entry in entries.flatten() {
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    let Ok(plugin_id) = PluginId::new(name.clone()) else {
                        continue;
                    };
                    if let Some(receipt) = self
                        .receipts
                        .current(&plugin_id)
                        .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))?
                    {
                        summaries.insert(name, summary(&receipt));
                    }
                }
            }
        }
        Ok(ManagerResponse::List {
            plugins: summaries.into_values().collect(),
        })
    }

    fn logs(&self, raw: String) -> Result<ManagerResponse, ManagerApiError> {
        let plugin_id = Self::plugin_id(&raw)?;
        let candidates = [
            self.profile
                .join("plugin-runtime")
                .join(plugin_id.as_str())
                .join("plugin.log"),
            self.profile.join("jarvis.log"),
        ];
        let path = candidates
            .into_iter()
            .find(|path| path.is_file())
            .unwrap_or_else(|| {
                self.profile
                    .join("plugin-runtime")
                    .join(plugin_id.as_str())
                    .join("plugin.log")
            });
        let lines = read_redacted_tail(&path)?;
        Ok(ManagerResponse::Logs { path, lines })
    }

    fn developer_lock(&self) -> Result<ManagerLock, ManagerApiError> {
        ManagerLock::acquire(&PluginPaths::new(self.profile.clone()))
            .map_err(|error| ManagerApiError::new(error.code(), error.to_string()))
    }
}

impl PluginManagementApi for PluginManagerEndpoint {
    fn request(&self, request: ManagerRequest) -> Result<ManagerResponse, ManagerApiError> {
        match request {
            ManagerRequest::Catalog { query } => Ok(ManagerResponse::Catalog {
                items: self.package()?.catalog(CatalogQuery {
                    text: query,
                    plugin_id: None,
                })?,
            }),
            ManagerRequest::Info { plugin_id } => Ok(ManagerResponse::Info {
                details: self.package()?.info(&Self::plugin_id(&plugin_id)?)?,
            }),
            ManagerRequest::PrepareInstall { source } => self.prepare_install(source),
            ManagerRequest::CommitInstall {
                operation_id,
                accept_permissions,
                trust_native_digest,
                approve_irreversible_migration,
            } => self.commit_install(
                operation_id,
                accept_permissions,
                trust_native_digest,
                approve_irreversible_migration,
            ),
            ManagerRequest::Update { plugin_id } => {
                let id = plugin_id.as_deref().map(Self::plugin_id).transpose()?;
                Ok(ManagerResponse::Operations {
                    operations: self.package()?.update(id.as_ref())?,
                })
            }
            ManagerRequest::Rollback { plugin_id, version } => {
                let plugin_id = Self::plugin_id(&plugin_id)?;
                let version = version
                    .as_deref()
                    .map(Version::parse)
                    .transpose()
                    .map_err(|_| {
                        ManagerApiError::new("package_version", "invalid rollback version")
                    })?;
                Ok(ManagerResponse::Receipt {
                    receipt: self.package()?.rollback(&plugin_id, version.as_ref())?,
                })
            }
            ManagerRequest::Enable { plugin_id } => self.set_package_enabled(plugin_id, true),
            ManagerRequest::Disable { plugin_id } => self.set_package_enabled(plugin_id, false),
            ManagerRequest::Uninstall { plugin_id } => Ok(ManagerResponse::Operation {
                operation: self.package()?.uninstall(&Self::plugin_id(&plugin_id)?)?,
            }),
            ManagerRequest::Purge {
                plugin_id,
                confirmation,
            } => Ok(ManagerResponse::Operation {
                operation: self
                    .package()?
                    .purge(&Self::plugin_id(&plugin_id)?, &confirmation)?,
            }),
            ManagerRequest::Doctor { plugin_id } => {
                let id = plugin_id.as_deref().map(Self::plugin_id).transpose()?;
                Ok(ManagerResponse::Doctor {
                    report: self.package()?.doctor(id.as_ref())?,
                })
            }
            ManagerRequest::Validate { source } => Ok(ManagerResponse::Validated {
                manifest: self.validate_source(&source)?,
            }),
            ManagerRequest::Pack { source, output } => self.pack_source(source, output),
            ManagerRequest::Link {
                source,
                accept_permissions,
                trust_native_digest,
            } => self.developer_link(source, accept_permissions, trust_native_digest),
            ManagerRequest::Unlink { plugin_id } => self.unlink_developer(plugin_id),
            ManagerRequest::Reload {
                plugin_id,
                accept_permissions,
                trust_native_digest,
            } => self.reload_developer(plugin_id, accept_permissions, trust_native_digest),
            ManagerRequest::Logs { plugin_id } => self.logs(plugin_id),
            ManagerRequest::List { developer_only } => self.list_plugins(developer_only),
            ManagerRequest::DeveloperMode { enabled } => self.set_developer_mode(enabled),
        }
    }
}

fn current_target() -> PackageTarget {
    #[cfg(target_arch = "aarch64")]
    {
        PackageTarget::DarwinArm64
    }
    #[cfg(target_arch = "x86_64")]
    {
        PackageTarget::DarwinAmd64
    }
}

fn canonical_source(raw: &str) -> Result<PathBuf, ManagerApiError> {
    let source = absolutize(raw)?;
    let source = fs::canonicalize(&source).map_err(|error| {
        ManagerApiError::new(
            "developer_source_invalid",
            format!("cannot resolve {}: {error}", source.display()),
        )
    })?;
    if !source.is_dir() {
        return Err(ManagerApiError::new(
            "developer_source_invalid",
            format!("{} is not a directory", source.display()),
        ));
    }
    Ok(source)
}

fn absolutize(raw: &str) -> Result<PathBuf, ManagerApiError> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| ManagerApiError::new("current_directory", error.to_string()))
}

fn grants(manifest: &ManifestV2) -> Vec<GrantedPermission> {
    manifest
        .permissions
        .iter()
        .map(|permission| GrantedPermission {
            id: permission.id,
            scope: permission.scope.clone(),
            modes: permission.modes.clone(),
        })
        .collect()
}

fn permission_diff(
    previous: &[GrantedPermission],
    next: &[GrantedPermission],
) -> (Vec<GrantedPermission>, Vec<GrantedPermission>) {
    (
        next.iter()
            .filter(|permission| !previous.contains(permission))
            .cloned()
            .collect(),
        previous
            .iter()
            .filter(|permission| !next.contains(permission))
            .cloned()
            .collect(),
    )
}

fn native_consent(
    runtime: RuntimeKind,
    expected: &Digest,
    supplied: Option<&str>,
) -> Result<Option<NativeDigestConsent>, ManagerApiError> {
    let supplied = supplied
        .map(|value| {
            Digest::new(value.to_owned()).map_err(|error| {
                ManagerApiError::new(error.code(), "invalid exact native SHA-256 digest")
            })
        })
        .transpose()?;
    if runtime == RuntimeKind::VerifiedNative {
        if supplied.as_ref() != Some(expected) {
            return Err(ManagerApiError::new(
                "developer_native_consent_required",
                format!("repeat with --trust-native-digest {}", expected.as_str()),
            ));
        }
        Ok(supplied.map(NativeDigestConsent::new))
    } else if supplied.is_some() {
        Err(ManagerApiError::new(
            "unexpected_native_consent",
            "UI-only plugin does not accept native digest consent",
        ))
    } else {
        Ok(None)
    }
}

fn summary(receipt: &InstallReceipt) -> PluginSummary {
    PluginSummary {
        plugin_id: receipt.plugin_id.clone(),
        version: receipt.version.clone(),
        source: receipt.source,
        enabled: receipt.enabled,
        package_digest: receipt.package_digest.clone(),
        generation: receipt.generation,
    }
}

fn package_error(error: PackageError) -> ManagerApiError {
    ManagerApiError::new(error.code(), error.to_string())
}

fn developer_error(error: crate::plugins::developer::DeveloperError) -> ManagerApiError {
    ManagerApiError::new(error.code(), error.to_string())
}

fn developer_registry_path(profile: &Path) -> PathBuf {
    profile.join("plugin-cache").join("developer-links.json")
}

fn read_developer_registry(profile: &Path) -> Result<DeveloperRegistry, ManagerApiError> {
    let path = developer_registry_path(profile);
    let Ok(bytes) = fs::read(&path) else {
        return Ok(DeveloperRegistry {
            schema_version: DEVELOPER_REGISTRY_SCHEMA,
            links: BTreeMap::new(),
        });
    };
    let registry: DeveloperRegistry = serde_json::from_slice(&bytes).map_err(|error| {
        ManagerApiError::new(
            "developer_registry_invalid",
            format!("cannot parse {}: {error}", path.display()),
        )
    })?;
    if registry.schema_version != DEVELOPER_REGISTRY_SCHEMA {
        return Err(ManagerApiError::new(
            "developer_registry_version",
            "unsupported developer link registry version",
        ));
    }
    Ok(registry)
}

fn write_developer_registry(
    profile: &Path,
    registry: &DeveloperRegistry,
) -> Result<(), ManagerApiError> {
    let path = developer_registry_path(profile);
    let parent = path.parent().expect("developer registry has a parent");
    fs::create_dir_all(parent).map_err(|error| {
        ManagerApiError::new(
            "developer_registry_write",
            format!("cannot create {}: {error}", parent.display()),
        )
    })?;
    let temp = parent.join(format!(".developer-links-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(registry)
        .map_err(|error| ManagerApiError::new("developer_registry_write", error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| {
            ManagerApiError::new(
                "developer_registry_write",
                format!("cannot create {}: {error}", temp.display()),
            )
        })?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(ManagerApiError::new(
            "developer_registry_write",
            format!("cannot commit {}: {error}", path.display()),
        ));
    }
    Ok(())
}

fn read_redacted_tail(path: &Path) -> Result<Vec<String>, ManagerApiError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(ManagerApiError::new(
                "plugin_logs",
                format!("cannot read {}: {error}", path.display()),
            ))
        }
    };
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if length > MAX_LOG_BYTES {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(-(MAX_LOG_BYTES as i64)))
            .map_err(|error| ManagerApiError::new("plugin_logs", error.to_string()))?;
    }
    let mut text = String::new();
    file.take(MAX_LOG_BYTES)
        .read_to_string(&mut text)
        .map_err(|error| ManagerApiError::new("plugin_logs", error.to_string()))?;
    let mut lines = text
        .lines()
        .rev()
        .take(MAX_LOG_LINES)
        .map(redact_log_line)
        .collect::<Vec<_>>();
    lines.reverse();
    Ok(lines)
}

fn redact_log_line(line: &str) -> String {
    let uppercase = line.to_ascii_uppercase();
    if ["TOKEN", "PASSWORD", "AUTHORIZATION", "SECRET", "API_KEY"]
        .iter()
        .any(|needle| uppercase.contains(needle))
    {
        "[redacted sensitive log line]".into()
    } else {
        line.to_owned()
    }
}

struct DeveloperPackSignature;

impl PackageSignatureSource for DeveloperPackSignature {
    fn sign(&self, _message: &[u8]) -> Result<PackageSignatureV1, PackageError> {
        PackageSignatureV1::new(
            SignatureAlgorithm::Ed25519,
            DEVELOPER_PACK_KEY_ID,
            STANDARD.encode([0_u8; 64]),
        )
        .map_err(|_| PackageError::package_metadata())
    }
}

struct InactiveLifecycle;

impl LifecycleHost for InactiveLifecycle {
    fn teardown(&self, _plugin_id: &PluginId) -> ManagerResult<TeardownStatus> {
        Err(ManagerError::new(
            "runtime_lifecycle_unavailable",
            "plugin runtime teardown is not connected",
        ))
    }

    fn uninstall_activation(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> ManagerResult<()> {
        Err(ManagerError::new(
            "activation_uninstall_unavailable",
            "verified activation wiring is not configured",
        ))
    }

    fn has_live_resources(&self, _plugin_id: &PluginId) -> ManagerResult<bool> {
        Err(ManagerError::new(
            "runtime_lifecycle_unavailable",
            "plugin runtime ownership cannot be verified",
        ))
    }

    fn purge_owned_data(&self, _paths: &PluginPaths, _plugin_id: &PluginId) -> ManagerResult<()> {
        Err(ManagerError::new(
            "plugin_purge_unavailable",
            "owned-data purge requires verified runtime ownership wiring",
        ))
    }
}

struct InactiveDeveloperTeardown;

impl DeveloperTeardownPort for InactiveDeveloperTeardown {
    fn teardown_and_revoke(&self, _link: &DeveloperLink) -> Result<(), String> {
        Err("plugin runtime teardown is not connected".into())
    }
}

struct HostLifecycleBridge {
    host: Arc<crate::plugins::PluginHost>,
    tokens: Arc<crate::capability::tokens::TokenStore>,
}

impl LifecycleHost for HostLifecycleBridge {
    fn teardown(&self, plugin_id: &PluginId) -> ManagerResult<TeardownStatus> {
        self.host
            .teardown_for_manager(plugin_id.as_str(), &self.tokens)
            .map(|()| TeardownStatus::Complete)
            .map_err(|error| ManagerError::new("runtime_teardown_failed", error))
    }

    fn resume_activation(&self, plugin_id: &PluginId) -> ManagerResult<()> {
        self.host.resume_after_manager(plugin_id.as_str());
        Ok(())
    }

    fn uninstall_activation(
        &self,
        _paths: &PluginPaths,
        _plugin_id: &PluginId,
    ) -> ManagerResult<()> {
        Err(ManagerError::new(
            "activation_uninstall_unavailable",
            "secure activation removal is not implemented",
        ))
    }

    fn has_live_resources(&self, plugin_id: &PluginId) -> ManagerResult<bool> {
        self.host
            .has_live_resources(plugin_id.as_str(), &self.tokens)
            .map_err(|error| ManagerError::new("runtime_observation_failed", error))
    }

    fn purge_owned_data(&self, _paths: &PluginPaths, _plugin_id: &PluginId) -> ManagerResult<()> {
        Err(ManagerError::new(
            "plugin_purge_unavailable",
            "secure owned-data purge is not implemented",
        ))
    }
}

impl DeveloperTeardownPort for HostLifecycleBridge {
    fn teardown_and_revoke(&self, link: &DeveloperLink) -> Result<(), String> {
        self.host
            .teardown_for_manager(link.receipt().plugin_id.as_str(), &self.tokens)
    }
}

fn build_package_manager(
    paths: PluginPaths,
    compatibility: HostCompatibility,
    lifecycle: Arc<dyn LifecycleHost>,
    catalog: Arc<dyn CatalogProvider>,
) -> ManagerResult<PluginManager> {
    PluginManager::new(
        paths,
        catalog,
        Arc::new(HttpDownloader::new(Duration::from_secs(120))?),
        Arc::new(StrictPackageEngine::new(compatibility)),
        Arc::new(RefuseSchemaChanges),
        Arc::new(NativeHealthRunner),
        lifecycle,
        Arc::new(SystemClock),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    #[test]
    fn unavailable_runtime_never_reports_successful_teardown() {
        let plugin_id = PluginId::new("dev.example.lifecycle").unwrap();
        let error = InactiveLifecycle
            .teardown(&plugin_id)
            .expect_err("an unwired runtime must fail closed");
        assert_eq!(error.code(), "runtime_lifecycle_unavailable");
    }

    #[test]
    fn unavailable_runtime_never_reports_no_live_resources() {
        let plugin_id = PluginId::new("dev.example.lifecycle").unwrap();
        let error = InactiveLifecycle
            .has_live_resources(&plugin_id)
            .expect_err("an unwired runtime cannot prove that resources are gone");
        assert_eq!(error.code(), "runtime_lifecycle_unavailable");
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "jarvis-plugin-manager-api-{label}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn production_catalog_absence_is_a_stable_fail_closed_error() {
        let root = TestDirectory::new("catalog-absent");
        let profile = root.0.join("profile");
        fs::create_dir(&profile).unwrap();
        let endpoint = PluginManagerEndpoint::new_with_components(
            profile.clone(),
            settings::Store::with_path(profile.join("settings.json")),
            Arc::new(InactiveLifecycle),
            Arc::new(InactiveDeveloperTeardown),
        )
        .unwrap();

        let error = endpoint
            .request(ManagerRequest::Catalog { query: None })
            .expect_err("an absent production catalog must fail closed");

        assert_eq!(error.code, "catalog_unconfigured");
    }

    struct InspectingDeveloperTeardown {
        profile: PathBuf,
        settings: settings::Store,
        calls: AtomicUsize,
        fail: bool,
    }

    impl DeveloperTeardownPort for InspectingDeveloperTeardown {
        fn teardown_and_revoke(&self, link: &DeveloperLink) -> Result<(), String> {
            let plugin_id = &link.receipt().plugin_id;
            let receipt = ReceiptStore::new(PluginPaths::new(self.profile.clone()))
                .current(plugin_id)
                .unwrap()
                .expect("enabled receipt is still visible during teardown");
            assert!(receipt.enabled);
            assert!(
                read_developer_registry(&self.profile)
                    .unwrap()
                    .links
                    .contains_key(plugin_id.as_str()),
                "registry mutation must happen after teardown"
            );
            assert_eq!(
                self.settings
                    .load()
                    .pointer(&format!("/plugins/{}/enabled", plugin_id.as_str()))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "settings mutation must happen after teardown"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("fixture teardown failed".into())
            } else {
                Ok(())
            }
        }
    }

    fn write_ui_plugin(source: &Path) {
        fs::create_dir_all(source.join("ui")).unwrap();
        fs::write(source.join("ui/index.html"), "developer fixture").unwrap();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "id": "dev.example.lifecycle",
            "name": "Lifecycle fixture",
            "version": "1.0.0",
            "publisher": "example",
            "compatibility": {
                "jarvis": ">=0.3.3, <0.5.0",
                "pluginApi": 2
            },
            "runtime": {
                "kind": "ui-only",
                "protocol": 2,
                "activationEvents": []
            },
            "permissions": [],
            "state": {
                "schemaVersion": 1,
                "migrations": [],
                "rollbackCompatibleThrough": 1
            },
            "contributes": {
                "pages": [],
                "commands": [],
                "actions": [],
                "hotkeys": [],
                "settings": [],
                "projectRuntimes": [],
                "dataContracts": []
            }
        });
        fs::write(
            source.join("plugin.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn linked_endpoint(
        fail_teardown: bool,
    ) -> (
        TestDirectory,
        PluginManagerEndpoint,
        Arc<InspectingDeveloperTeardown>,
        PluginId,
    ) {
        let root = TestDirectory::new(if fail_teardown {
            "unlink-failure"
        } else {
            "unlink-success"
        });
        let profile = root.0.join("profile");
        let source = root.0.join("source");
        fs::create_dir(&profile).unwrap();
        fs::create_dir(&source).unwrap();
        write_ui_plugin(&source);
        let settings = settings::Store::with_path(profile.join("settings.json"));
        let teardown = Arc::new(InspectingDeveloperTeardown {
            profile: profile.clone(),
            settings: settings.clone(),
            calls: AtomicUsize::new(0),
            fail: fail_teardown,
        });
        let endpoint = PluginManagerEndpoint::new_with_components(
            profile,
            settings,
            Arc::new(InactiveLifecycle),
            teardown.clone(),
        )
        .unwrap();
        endpoint
            .request(ManagerRequest::DeveloperMode { enabled: true })
            .unwrap();
        endpoint
            .request(ManagerRequest::Link {
                source: source.to_string_lossy().into_owned(),
                accept_permissions: true,
                trust_native_digest: None,
            })
            .unwrap();
        let plugin_id = PluginId::new("dev.example.lifecycle").unwrap();
        endpoint.persist_plugin_enabled(&plugin_id, true).unwrap();
        (root, endpoint, teardown, plugin_id)
    }

    #[test]
    fn unlink_tears_down_before_receipt_registry_and_settings_mutation() {
        let (_root, endpoint, teardown, plugin_id) = linked_endpoint(false);

        endpoint
            .request(ManagerRequest::Unlink {
                plugin_id: plugin_id.as_str().to_owned(),
            })
            .unwrap();

        assert_eq!(teardown.calls.load(Ordering::SeqCst), 1);
        assert!(
            !endpoint
                .receipts
                .current(&plugin_id)
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(!endpoint
            .developer_registry
            .lock()
            .unwrap()
            .links
            .contains_key(plugin_id.as_str()));
        assert!(!endpoint
            .active_developer_links
            .lock()
            .unwrap()
            .contains_key(plugin_id.as_str()));
        assert_eq!(
            endpoint
                .settings
                .load()
                .pointer(&format!("/plugins/{}/enabled", plugin_id.as_str()))
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn successful_developer_link_is_resolvable_from_its_immutable_snapshot() {
        let (root, _endpoint, _teardown, plugin_id) = linked_endpoint(false);
        let paths = PluginPaths::new(root.0.join("profile"));
        let compatibility = HostCompatibility::parse("0.4.0", PLUGIN_API_VERSION).unwrap();
        let catalog_compatibility = crate::plugins::trust::catalog::CatalogCompatibility::parse(
            "0.4.0",
            PLUGIN_API_VERSION,
            current_target(),
            "13.0.0",
        )
        .unwrap();
        let trust = Arc::new(ProductionCatalogProvider::for_profile(
            paths.clone(),
            catalog_compatibility,
            Arc::new(SystemClock),
        ));
        let resolver = crate::plugins::resolver::PluginResolver::new(
            paths.clone(),
            compatibility,
            current_target(),
            trust,
        );

        let resolved = resolver
            .resolve(
                &plugin_id,
                crate::plugins::resolver::ResolutionPolicy {
                    developer_mode: true,
                    legacy_agent_vm_enabled: false,
                },
            )
            .unwrap();

        assert_eq!(
            resolved.source(),
            crate::plugins::resolver::ActivationSource::DeveloperSnapshot
        );
        assert!(resolved
            .root()
            .starts_with(paths.cache(&plugin_id).join("developer")));
    }

    #[test]
    fn failed_unlink_teardown_preserves_receipt_registry_and_settings() {
        let (_root, endpoint, teardown, plugin_id) = linked_endpoint(true);

        let error = endpoint
            .request(ManagerRequest::Unlink {
                plugin_id: plugin_id.as_str().to_owned(),
            })
            .expect_err("teardown failure must abort unlink");

        assert_eq!(error.code, "developer_teardown_failed");
        assert_eq!(teardown.calls.load(Ordering::SeqCst), 1);
        assert!(
            endpoint
                .receipts
                .current(&plugin_id)
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(endpoint
            .developer_registry
            .lock()
            .unwrap()
            .links
            .contains_key(plugin_id.as_str()));
        assert!(endpoint
            .active_developer_links
            .lock()
            .unwrap()
            .contains_key(plugin_id.as_str()));
        assert_eq!(
            endpoint
                .settings
                .load()
                .pointer(&format!("/plugins/{}/enabled", plugin_id.as_str()))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn reload_tears_down_before_replacing_the_active_developer_receipt() {
        let (root, endpoint, teardown, plugin_id) = linked_endpoint(false);
        let before = endpoint.receipts.current(&plugin_id).unwrap().unwrap();
        fs::write(
            root.0.join("source/ui/index.html"),
            "developer fixture reloaded",
        )
        .unwrap();

        endpoint
            .request(ManagerRequest::Reload {
                plugin_id: plugin_id.as_str().to_owned(),
                accept_permissions: true,
                trust_native_digest: None,
            })
            .unwrap();

        let after = endpoint.receipts.current(&plugin_id).unwrap().unwrap();
        assert_eq!(teardown.calls.load(Ordering::SeqCst), 1);
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(
            after.previous.as_ref().map(|previous| previous.generation),
            Some(before.generation)
        );
    }

    #[test]
    fn failed_reload_teardown_preserves_the_old_developer_activation() {
        let (root, endpoint, teardown, plugin_id) = linked_endpoint(true);
        let before = endpoint.receipts.current(&plugin_id).unwrap().unwrap();
        fs::write(
            root.0.join("source/ui/index.html"),
            "developer fixture rejected reload",
        )
        .unwrap();

        let error = endpoint
            .request(ManagerRequest::Reload {
                plugin_id: plugin_id.as_str().to_owned(),
                accept_permissions: true,
                trust_native_digest: None,
            })
            .expect_err("teardown failure must abort developer reload");

        assert_eq!(error.code, "developer_teardown_failed");
        assert_eq!(teardown.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            endpoint.receipts.current(&plugin_id).unwrap().unwrap(),
            before
        );
        assert!(endpoint
            .active_developer_links
            .lock()
            .unwrap()
            .contains_key(plugin_id.as_str()));
        assert!(endpoint
            .developer_registry
            .lock()
            .unwrap()
            .links
            .contains_key(plugin_id.as_str()));
    }
}
