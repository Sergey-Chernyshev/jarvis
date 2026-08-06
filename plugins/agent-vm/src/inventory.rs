use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_RECORD_BYTES: u64 = 256 * 1024;
pub const MAX_ADDITIONAL_MOUNTS: usize = 16;

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
    #[serde(default)]
    pub mounts: Vec<VmMount>,
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
            mounts: Vec::new(),
        }
    }

    pub fn mount_roots(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut roots = Vec::with_capacity(self.mounts.len() + 1);
        if let Some(host_path) = self.workspace.host_path.as_deref() {
            roots.push((
                PathBuf::from(host_path),
                PathBuf::from(&self.workspace.guest_path),
            ));
        }
        roots.extend(self.mounts.iter().map(|mount| {
            (
                PathBuf::from(&mount.host_path),
                PathBuf::from(&mount.guest_path),
            )
        }));
        roots
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmMount {
    pub host_path: String,
    pub guest_path: String,
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
    let expected_lima_home = PathBuf::from("/home").join(format!("{user}.linux"));
    matches!(
        workspace.parent(),
        Some(parent)
            if parent == expected_home
                || parent == expected_mount_home
                || parent == expected_lima_home
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
    let paths = collect_record_paths(
        entries.map(|entry| entry.map(|entry| entry.path())),
        &vms_dir,
    )?;

    let mut records = Vec::with_capacity(paths.len());
    for path in paths {
        match load_record(&path) {
            Ok(record) => records.push(record),
            Err(reason) => report_and_quarantine_record(registry_root, &path, &reason),
        }
    }
    Ok(records)
}

fn collect_record_paths<I>(entries: I, vms_dir: &Path) -> Result<Vec<PathBuf>, String>
where
    I: IntoIterator<Item = Result<PathBuf, io::Error>>,
{
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| {
            format!(
                "не прочитать entry agent-vm registry {}: {error}",
                vms_dir.display()
            )
        })?;
        if path.extension().and_then(|extension| extension.to_str()) == Some("yaml") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn load_record(path: &Path) -> Result<VmRecord, String> {
    let file_name = record_file_name(path);
    let metadata = fs::symlink_metadata(path)
        .map_err(|err| format!("не проверить VM record {file_name}: {err}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("VM record {file_name} должен быть обычным файлом"));
    }
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "VM record {file_name} превышает лимит {MAX_RECORD_BYTES} байт"
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|err| format!("не открыть VM record {file_name} без symlink follow: {err}"))?;
    let opened = file
        .metadata()
        .map_err(|err| format!("не проверить открытый VM record {file_name}: {err}"))?;
    if !opened.file_type().is_file() || opened.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "VM record {file_name} изменился или имеет unsafe type/size"
        ));
    }
    let mut data = Vec::with_capacity(opened.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|err| format!("не прочитать VM record {file_name}: {err}"))?;
    if data.len() as u64 > MAX_RECORD_BYTES {
        return Err(format!(
            "VM record {file_name} превышает лимит {MAX_RECORD_BYTES} байт"
        ));
    }
    let record: VmRecord = serde_yaml::from_slice(&data)
        .map_err(|err| format!("не разобрать VM record {file_name}: {err}"))?;
    if !crate::project::is_valid_vm_name(&record.name) {
        return Err(format!("VM record {file_name} содержит недопустимое name"));
    }
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(record.name.as_str()) {
        return Err(format!(
            "VM record {file_name} содержит name, не совпадающий с именем файла"
        ));
    }
    validate_record_shape(&record)
        .map_err(|error| format!("VM record {file_name} имеет unsafe Record: {error}"))?;
    Ok(record)
}

fn report_and_quarantine_record(registry_root: &Path, path: &Path, reason: &str) {
    match quarantine_record(registry_root, path) {
        Ok(target) => eprintln!(
            "[agent-vm] {reason}; quarantined as {}",
            record_file_name(&target)
        ),
        Err(error) => eprintln!("[agent-vm] {reason}; quarantine failed: {error}"),
    }
}

fn quarantine_record(registry_root: &Path, path: &Path) -> Result<PathBuf, String> {
    let quarantine = registry_root.join("quarantine");
    match fs::symlink_metadata(&quarantine) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err("VM record quarantine имеет unsafe type".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&quarantine)
                .map_err(|err| format!("не создать VM record quarantine: {err}"))?;
        }
        Err(error) => return Err(format!("не проверить VM record quarantine: {error}")),
    }
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("не защитить VM record quarantine: {err}"))?;
    let file_name = record_file_name(path);
    let target = quarantine.join(format!(
        "{file_name}.{}.invalid",
        uuid::Uuid::new_v4().simple()
    ));
    fs::rename(path, &target)
        .map_err(|err| format!("не переместить invalid VM record {file_name}: {err}"))?;
    Ok(target)
}

fn record_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown.yaml")
        .to_string()
}

fn validate_record_shape(record: &VmRecord) -> Result<(), String> {
    if record.user.is_empty()
        || record.user == "."
        || record.user == ".."
        || record.user.len() > 128
        || record
            .user
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    {
        return Err("user имеет недопустимый формат".into());
    }
    if !is_safe_guest_workspace(&record.user, &record.workspace.guest_path) {
        return Err("workspace.guestPath находится вне guest home".into());
    }
    match record.workspace.mode_name.as_str() {
        "mount" => {
            let host = record
                .workspace
                .host_path
                .as_deref()
                .ok_or_else(|| "mount workspace не содержит hostPath".to_string())?;
            let host = Path::new(host);
            if !host.is_absolute()
                || host
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err("workspace.hostPath должен быть absolute и normalized".into());
            }
        }
        "clone" => {
            if record
                .workspace
                .repo
                .as_deref()
                .unwrap_or_default()
                .is_empty()
            {
                return Err("clone workspace не содержит repo".into());
            }
        }
        _ => return Err("workspace.mode должен быть mount или clone".into()),
    }
    validate_additional_mounts(record).map_err(|error| format!("unsafe additional mounts: {error}"))
}

fn validate_additional_mounts(record: &VmRecord) -> Result<(), String> {
    if record.mounts.len() > MAX_ADDITIONAL_MOUNTS {
        return Err(format!("больше {MAX_ADDITIONAL_MOUNTS} additional mounts"));
    }
    let primary_guest = Path::new(&record.workspace.guest_path);
    let mut host_paths = std::collections::BTreeSet::new();
    let mut guest_paths = std::collections::BTreeSet::from([primary_guest.to_path_buf()]);
    for mount in &record.mounts {
        let host = Path::new(&mount.host_path);
        let guest = Path::new(&mount.guest_path);
        if !host.is_absolute()
            || host
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("additional hostPath должен быть absolute и normalized".into());
        }
        if !is_safe_guest_workspace(&record.user, &mount.guest_path) {
            return Err("additional guestPath должен быть прямым child guest home".into());
        }
        if !host_paths.insert(host.to_path_buf()) {
            return Err("additional hostPath продублирован".into());
        }
        if !guest_paths.insert(guest.to_path_buf()) {
            return Err("additional guestPath конфликтует с другим mount".into());
        }
    }
    Ok(())
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
    use std::io;
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
    fn current_upstream_additional_mounts_are_loaded_bounded_and_exposed_as_roots() {
        let root = temp_root("additional-mounts");
        let registry = root.join("vms");
        fs::create_dir_all(&registry).unwrap();
        fs::write(
            registry.join("project-a.yaml"),
            r#"name: project-a
source: project
user: dev
workspace:
  mode: mount
  hostPath: /work/main
  guestPath: /home/dev.linux/main
mounts:
  - hostPath: /work/shared-lib
    guestPath: /home/dev.linux/shared-lib
  - hostPath: /work/tools
    guestPath: /home/dev.linux/tools
"#,
        )
        .unwrap();

        let records = load_records(&root).unwrap();

        assert_eq!(records[0].mounts.len(), 2);
        assert_eq!(
            records[0].mount_roots(),
            vec![
                (
                    PathBuf::from("/work/main"),
                    PathBuf::from("/home/dev.linux/main")
                ),
                (
                    PathBuf::from("/work/shared-lib"),
                    PathBuf::from("/home/dev.linux/shared-lib")
                ),
                (
                    PathBuf::from("/work/tools"),
                    PathBuf::from("/home/dev.linux/tools")
                ),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn additional_mounts_reject_guest_escape_and_collisions() {
        let mut record = VmRecord::mount("project-a", "/work/main", "/home/dev/main");
        record.mounts = vec![VmMount {
            host_path: "/work/shared".into(),
            guest_path: "/tmp/shared".into(),
        }];
        assert!(validate_additional_mounts(&record).is_err());

        record.mounts = vec![VmMount {
            host_path: "/work/shared".into(),
            guest_path: "/home/dev/main".into(),
        }];
        assert!(validate_additional_mounts(&record).is_err());
    }

    #[test]
    fn corrupt_record_is_reported_and_quarantined_without_hiding_good_records() {
        let root = temp_root("quarantine-corrupt");
        let vms = root.join("vms");
        let good = vms.join("good.yaml");
        let corrupt = vms.join("corrupt.yaml");
        fs::write(
            &good,
            r#"name: good
source: project
user: dev
workspace:
  mode: mount
  hostPath: /work/good
  guestPath: /home/dev/good
"#,
        )
        .unwrap();
        let corrupt_bytes = b"name: [unterminated\n";
        fs::write(&corrupt, corrupt_bytes).unwrap();

        let records = load_records(&root).unwrap();

        assert_eq!(
            records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            ["good"]
        );
        assert!(!corrupt.exists());
        let quarantine = root.join("quarantine");
        let quarantined = fs::read_dir(&quarantine)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantined.len(), 1);
        assert!(
            quarantined[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("corrupt"),
            "quarantine preserves record provenance"
        );
        assert_eq!(fs::read(&quarantined[0]).unwrap(), corrupt_bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_entry_error_fails_closed_with_directory_diagnostic() {
        let vms = PathBuf::from("/synthetic/agent-vm/vms");
        let entries = vec![
            Ok(vms.join("visible.yaml")),
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic denied entry",
            )),
        ];

        let error = collect_record_paths(entries, &vms).unwrap_err();

        assert!(error.contains("/synthetic/agent-vm/vms"), "{error}");
        assert!(error.contains("entry"), "{error}");
        assert!(error.contains("synthetic denied entry"), "{error}");
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
