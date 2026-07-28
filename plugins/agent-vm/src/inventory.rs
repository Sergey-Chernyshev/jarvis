use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_RECORD_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmRecord {
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default)]
    pub resources: VmResources,
    #[serde(default)]
    #[serde(skip_serializing)]
    pub user: String,
    pub workspace: VmWorkspace,
}

impl VmRecord {
    #[cfg(test)]
    fn mount(name: &str, host_path: &str, guest_path: &str) -> Self {
        Self {
            name: name.into(),
            source: "project".into(),
            modules: vec![],
            resources: VmResources::default(),
            user: "dev".into(),
            workspace: VmWorkspace {
                mode_name: "mount".into(),
                guest_path: guest_path.into(),
                host_path: Some(host_path.into()),
                repo: None,
                git_ref: None,
            },
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmResources {
    #[serde(default)]
    pub cpus: u32,
    #[serde(default)]
    pub memory: String,
    #[serde(default)]
    pub disk: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmWorkspace {
    #[serde(rename = "mode")]
    pub mode_name: String,
    pub guest_path: String,
    #[serde(default)]
    pub host_path: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimaInstance {
    pub name: String,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryVm {
    pub name: String,
    pub management: String,
    pub state: String,
    pub record: Option<VmRecord>,
}

pub fn is_safe_guest_workspace(user: &str, guest_path: &str) -> bool {
    let workspace = Path::new(guest_path);
    if !workspace.is_absolute()
        || workspace
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return false;
    }
    let expected_home = PathBuf::from("/home").join(user);
    let expected_mount_home = PathBuf::from("/home").join(format!("{user}.guest"));
    matches!(
        workspace.parent(),
        Some(parent) if parent == expected_home || parent == expected_mount_home
    )
}

pub fn load_records(registry_root: &Path) -> Result<Vec<VmRecord>, String> {
    let vms_dir = registry_root.join("vms");
    let entries = match fs::read_dir(&vms_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "не прочитать agent-vm registry {}: {err}",
                vms_dir.display()
            ));
        }
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("yaml"))
        .collect::<Vec<PathBuf>>();
    paths.sort();

    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| format!("не проверить VM record {}: {err}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "VM record {} должен быть обычным файлом",
                path.display()
            ));
        }
        if metadata.len() > MAX_RECORD_BYTES {
            return Err(format!(
                "VM record {} превышает лимит {MAX_RECORD_BYTES} байт",
                path.display()
            ));
        }
        let data = fs::read(&path)
            .map_err(|err| format!("не прочитать VM record {}: {err}", path.display()))?;
        let record: VmRecord = serde_yaml::from_slice(&data)
            .map_err(|err| format!("не разобрать VM record {}: {err}", path.display()))?;
        if !crate::project::is_valid_vm_name(&record.name) {
            return Err(format!(
                "VM record {} содержит недопустимое name",
                path.display()
            ));
        }
        records.push(record);
    }
    Ok(records)
}

pub fn parse_lima_instances(output: &str) -> Result<Vec<LimaInstance>, String> {
    let mut instances = Vec::new();
    for (index, raw) in output.lines().enumerate() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some((name, state)) = line.split_once('\t') else {
            return Err(format!(
                "неожиданный limactl list output в строке {}",
                index + 1
            ));
        };
        if name.is_empty() || state.is_empty() || state.contains('\t') {
            return Err(format!(
                "неожиданный limactl list output в строке {}",
                index + 1
            ));
        }
        instances.push(LimaInstance {
            name: name.to_string(),
            state: state.trim().to_ascii_lowercase(),
        });
    }
    Ok(instances)
}

pub fn reconcile(records: Vec<VmRecord>, instances: Vec<LimaInstance>) -> Vec<InventoryVm> {
    let mut by_name = instances
        .into_iter()
        .map(|instance| (instance.name.clone(), instance))
        .collect::<BTreeMap<_, _>>();
    let mut out = Vec::new();
    for record in records {
        let (management, state) = match by_name.remove(&record.name) {
            Some(instance) => ("managed", instance.state),
            None => ("orphaned", "missing".into()),
        };
        out.push(InventoryVm {
            name: record.name.clone(),
            management: management.into(),
            state,
            record: Some(record),
        });
    }
    out.extend(by_name.into_values().map(|instance| InventoryVm {
        name: instance.name,
        management: "unmanaged".into(),
        state: instance.state,
        record: None,
    }));
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_root(tag: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-inventory-{tag}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("vms")).unwrap();
        root
    }

    #[test]
    fn loads_records_without_exposing_unknown_yaml_fields() {
        let root = temp_root("records");
        fs::write(
            root.join("vms/sup.yaml"),
            r#"
name: sup
source: project
createdAt: 2026-07-28T08:00:00Z
base:
  image: template:_images/ubuntu
modules: [node, claude, codex]
resources:
  cpus: 4
  memory: 4GiB
  disk: 120GiB
user: developer
workspace:
  mode: mount
  guestPath: /home/developer/sup
  hostPath: /tmp/work/sup
futureSecretField: must-not-be-retained
"#,
        )
        .unwrap();

        let records = load_records(&root).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "sup");
        assert_eq!(records[0].workspace.mode_name, "mount");
        assert_eq!(
            records[0].workspace.host_path.as_deref(),
            Some("/tmp/work/sup")
        );
        assert_eq!(records[0].workspace.guest_path, "/home/developer/sup");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_lima_format_and_reconciles_managed_orphaned_and_unmanaged() {
        let records = vec![
            VmRecord::mount("alpha", "/work/alpha", "/home/dev/alpha"),
            VmRecord::mount("orphan", "/work/orphan", "/home/dev/orphan"),
        ];
        let instances = parse_lima_instances("alpha\tRunning\nforeign\tStopped\n").unwrap();

        let vms = reconcile(records, instances);

        assert_eq!(
            vms.iter()
                .map(|vm| (vm.name.as_str(), vm.management.as_str(), vm.state.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("alpha", "managed", "running"),
                ("foreign", "unmanaged", "stopped"),
                ("orphan", "orphaned", "missing"),
            ]
        );
    }

    #[test]
    fn rejects_ambiguous_lima_lines() {
        let err = parse_lima_instances("missing-tab\n").unwrap_err();
        assert!(err.contains("limactl"), "actionable parse error: {err}");
    }
}
