use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::capability::contract::RiskClass;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol_version: u32,
    pub entry: Entry,
    #[serde(default)]
    pub capabilities: Vec<RiskClass>,
    #[serde(default)]
    pub project_runtimes: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Entry {
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PluginPackage {
    pub manifest: Manifest,
    pub root: PathBuf,
    pub executable: PathBuf,
}

#[derive(Clone, Debug)]
pub struct LoadError {
    pub key: String,
    pub path: PathBuf,
    pub message: String,
    pub incompatible: bool,
}

#[derive(Default)]
pub struct Discovery {
    pub packages: Vec<PluginPackage>,
    pub errors: Vec<LoadError>,
}

fn path_key(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown-plugin")
        .to_string()
}

fn error(path: &Path, key: impl Into<String>, message: impl Into<String>) -> LoadError {
    LoadError {
        key: key.into(),
        path: path.to_path_buf(),
        message: message.into(),
        incompatible: false,
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub fn load_package(manifest_path: &Path) -> Result<PluginPackage, LoadError> {
    let key = path_key(manifest_path);
    let metadata = fs::metadata(manifest_path)
        .map_err(|err| error(manifest_path, &key, format!("manifest недоступен: {err}")))?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(error(
            manifest_path,
            key,
            format!("размер manifest превышает лимит {MAX_MANIFEST_BYTES} байт"),
        ));
    }
    let bytes = fs::read(manifest_path)
        .map_err(|err| error(manifest_path, &key, format!("manifest не читается: {err}")))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|err| {
        error(
            manifest_path,
            &key,
            format!("невалидный manifest JSON: {err}"),
        )
    })?;

    if !valid_id(&manifest.id) {
        return Err(error(
            manifest_path,
            &manifest.id,
            format!("невалидный plugin id '{}'", manifest.id),
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err(error(manifest_path, &manifest.id, "name обязателен"));
    }
    if manifest.version.trim().is_empty() {
        return Err(error(manifest_path, &manifest.id, "version обязателен"));
    }
    if manifest.protocol_version != PROTOCOL_VERSION {
        let mut err = error(
            manifest_path,
            &manifest.id,
            format!(
                "несовместимый protocolVersion {}, поддерживается {PROTOCOL_VERSION}",
                manifest.protocol_version
            ),
        );
        err.incompatible = true;
        return Err(err);
    }
    if manifest.entry.kind != "binary" {
        return Err(error(
            manifest_path,
            &manifest.id,
            format!("неподдерживаемый entry.type '{}'", manifest.entry.kind),
        ));
    }
    if manifest.capabilities.contains(&RiskClass::Admin) {
        return Err(error(
            manifest_path,
            &manifest.id,
            "capability class admin запрещён внешним плагинам",
        ));
    }

    let plugin_root = manifest_path
        .parent()
        .ok_or_else(|| error(manifest_path, &manifest.id, "manifest без каталога"))?
        .canonicalize()
        .map_err(|err| {
            error(
                manifest_path,
                &manifest.id,
                format!("каталог плагина недоступен: {err}"),
            )
        })?;
    let executable = plugin_root
        .join(&manifest.entry.path)
        .canonicalize()
        .map_err(|err| {
            error(
                manifest_path,
                &manifest.id,
                format!("entry недоступен: {err}"),
            )
        })?;
    if !executable.starts_with(&plugin_root) {
        return Err(error(
            manifest_path,
            &manifest.id,
            "entry находится вне каталога плагина",
        ));
    }
    let executable_meta = fs::metadata(&executable).map_err(|err| {
        error(
            manifest_path,
            &manifest.id,
            format!("entry недоступен: {err}"),
        )
    })?;
    if !executable_meta.is_file() {
        return Err(error(
            manifest_path,
            &manifest.id,
            "entry должен быть обычным файлом",
        ));
    }
    if executable_meta.permissions().mode() & 0o111 == 0 {
        return Err(error(
            manifest_path,
            &manifest.id,
            "entry не является исполняемым",
        ));
    }

    Ok(PluginPackage {
        manifest,
        root: plugin_root,
        executable,
    })
}

pub fn discover(roots: &[PathBuf]) -> Discovery {
    let mut packages = BTreeMap::<String, PluginPackage>::new();
    let mut errors = Vec::new();

    for root in roots {
        let mut directories = match fs::read_dir(root) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
                .map(|entry| entry.path())
                .collect::<Vec<_>>(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                errors.push(error(
                    root,
                    root.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("plugin-root"),
                    format!("каталог плагинов недоступен: {err}"),
                ));
                continue;
            }
        };
        directories.sort();

        for directory in directories {
            let manifest_path = directory.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            match load_package(&manifest_path) {
                Ok(package) => {
                    let id = package.manifest.id.clone();
                    if packages.contains_key(&id) {
                        errors.push(error(
                            &manifest_path,
                            &id,
                            format!("дубликат plugin id '{id}', используется первый root"),
                        ));
                    } else {
                        packages.insert(id, package);
                    }
                }
                Err(err) => errors.push(err),
            }
        }
    }

    Discovery {
        packages: packages.into_values().collect(),
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_root(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "jarvis-plugin-manifest-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_executable(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_plugin(
        root: &Path,
        directory: &str,
        id: &str,
        version: &str,
        protocol_version: u32,
        capabilities: Value,
        entry_path: &str,
    ) -> PathBuf {
        let plugin_root = root.join(directory);
        fs::create_dir_all(&plugin_root).unwrap();
        if !entry_path.starts_with("../") {
            write_executable(&plugin_root.join(entry_path));
        }
        let manifest_path = plugin_root.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&json!({
                "id": id,
                "name": format!("Plugin {id}"),
                "version": version,
                "protocolVersion": protocol_version,
                "entry": {
                    "type": "binary",
                    "path": entry_path,
                    "args": ["--fixture"]
                },
                "capabilities": capabilities,
                "projectRuntimes": []
            }))
            .unwrap(),
        )
        .unwrap();
        manifest_path
    }

    #[test]
    fn loads_valid_v1_manifest_and_canonical_entry() {
        let root = temp_root("valid");
        let path = write_plugin(
            &root,
            "agent-vm",
            "agent-vm",
            "0.1.0",
            PROTOCOL_VERSION,
            json!(["read", "control"]),
            "bin/plugin",
        );

        let package = load_package(&path).unwrap();

        assert_eq!(package.manifest.id, "agent-vm");
        assert_eq!(package.manifest.protocol_version, PROTOCOL_VERSION);
        assert_eq!(package.manifest.capabilities.len(), 2);
        assert_eq!(package.root, root.join("agent-vm").canonicalize().unwrap());
        assert_eq!(
            package.executable,
            root.join("agent-vm/bin/plugin").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_admin_capability() {
        let root = temp_root("admin");
        let path = write_plugin(
            &root,
            "admin",
            "admin-plugin",
            "1.0.0",
            PROTOCOL_VERSION,
            json!(["read", "admin"]),
            "plugin",
        );

        let err = load_package(&path).unwrap_err();

        assert!(
            err.message.contains("admin"),
            "ошибка называет класс: {err:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_protocol_mismatch_as_incompatible() {
        let root = temp_root("protocol");
        let path = write_plugin(
            &root,
            "future",
            "future-plugin",
            "1.0.0",
            PROTOCOL_VERSION + 1,
            json!(["read"]),
            "plugin",
        );

        let err = load_package(&path).unwrap_err();

        assert!(err.incompatible);
        assert!(err.message.contains("protocol"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_entry_outside_plugin_root() {
        let root = temp_root("escape");
        write_executable(&root.join("escape"));
        let path = write_plugin(
            &root,
            "plugin",
            "escape-plugin",
            "1.0.0",
            PROTOCOL_VERSION,
            json!(["read"]),
            "../escape",
        );

        let err = load_package(&path).unwrap_err();

        assert!(
            err.message.contains("вне каталога плагина"),
            "понятная path-traversal ошибка: {err:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_plugin_id() {
        for id in ["AgentVm", "agent/vm", "agent vm"] {
            let root = temp_root("bad-id");
            let path = write_plugin(
                &root,
                "plugin",
                id,
                "1.0.0",
                PROTOCOL_VERSION,
                json!(["read"]),
                "plugin",
            );
            let err = load_package(&path).unwrap_err();
            assert!(
                err.message.contains("id"),
                "{id:?} должен быть отклонён: {err:?}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn discovery_is_sorted_and_first_root_wins_duplicate_id() {
        let first = temp_root("first");
        let second = temp_root("second");
        write_plugin(
            &first,
            "z",
            "zed",
            "1.0.0",
            PROTOCOL_VERSION,
            json!(["read"]),
            "plugin",
        );
        write_plugin(
            &first,
            "dupe",
            "dupe",
            "1.0.0",
            PROTOCOL_VERSION,
            json!(["read"]),
            "plugin",
        );
        write_plugin(
            &second,
            "dupe",
            "dupe",
            "2.0.0",
            PROTOCOL_VERSION,
            json!(["read"]),
            "plugin",
        );

        let found = discover(&[first.clone(), second.clone()]);

        assert_eq!(
            found
                .packages
                .iter()
                .map(|package| package.manifest.id.as_str())
                .collect::<Vec<_>>(),
            ["dupe", "zed"]
        );
        assert_eq!(found.packages[0].manifest.version, "1.0.0");
        assert_eq!(found.errors.len(), 1);
        assert!(found.errors[0].message.contains("дубликат"));
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn rejects_manifest_over_size_limit() {
        let root = temp_root("large");
        let path = write_plugin(
            &root,
            "large",
            "large-plugin",
            "1.0.0",
            PROTOCOL_VERSION,
            json!(["read"]),
            "plugin",
        );
        fs::write(&path, vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();

        let err = load_package(&path).unwrap_err();

        assert!(err.message.contains("размер"), "ошибка квоты: {err:?}");
        fs::remove_dir_all(root).unwrap();
    }
}
