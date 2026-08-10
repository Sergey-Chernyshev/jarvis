use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::host::HostApi;
use crate::inventory::InventoryVm;
use crate::project::ProjectIdentity;
use crate::service::{RuntimeService, RuntimeSnapshot};

#[derive(Clone)]
pub struct VmEntityPublisher<H: HostApi> {
    host: H,
    state: Arc<Mutex<PublisherState>>,
}

#[derive(Clone, Debug, PartialEq)]
enum VmPublication {
    Upsert { state: String, attrs: Value },
    Remove,
}

#[derive(Clone)]
struct DesiredVm {
    publication: VmPublication,
    revision: u64,
    confirmed: bool,
}

#[derive(Clone)]
struct ReservedPublication {
    vm_name: String,
    publication: VmPublication,
    revision: u64,
}

#[derive(Default)]
struct PublisherState {
    revision: u64,
    desired: BTreeMap<String, DesiredVm>,
}

impl<H: HostApi> VmEntityPublisher<H> {
    pub fn new(host: H) -> Self {
        Self {
            host,
            state: Arc::new(Mutex::new(PublisherState::default())),
        }
    }

    pub fn checkpoint(&self) -> u64 {
        self.state.lock().unwrap().revision
    }

    pub fn publish_snapshot(&self, snapshot: &RuntimeSnapshot) -> Result<(), String> {
        let vm_state = snapshot
            .vm
            .as_ref()
            .map(|vm| vm.state.as_str())
            .unwrap_or("absent");
        let mut attrs = snapshot_attrs(snapshot);
        let request = {
            let mut publisher = self.state.lock().unwrap();
            if let Some(existing) = publisher.desired.get(&snapshot.vm_name).cloned() {
                preserve_running_bootstrap(&existing, vm_state, &mut attrs);
            }
            let publication = VmPublication::Upsert {
                state: vm_state.into(),
                attrs,
            };
            reserve_authoritative(&mut publisher, &snapshot.vm_name, publication)?
        };
        self.send_reserved(request)
    }

    pub fn reconcile_inventory<S: RuntimeService>(
        &self,
        checkpoint: u64,
        service: &S,
        inventory: Vec<InventoryVm>,
    ) -> Result<(), String> {
        let current = inventory
            .iter()
            .map(|vm| vm.name.clone())
            .collect::<BTreeSet<_>>();
        let host_vm_names = self
            .host
            .query_vm_entity_ids()
            .unwrap_or_default()
            .into_iter()
            .collect::<BTreeSet<_>>();

        for vm in &inventory {
            let mut attrs = inventory_attrs(service, vm);
            let request = {
                let mut publisher = self.state.lock().unwrap();
                let existing = publisher.desired.get(&vm.name).cloned();
                if existing
                    .as_ref()
                    .is_some_and(|desired| desired.revision > checkpoint)
                {
                    None
                } else {
                    if let Some(existing) = &existing {
                        preserve_running_bootstrap(existing, &vm.state, &mut attrs);
                    }
                    reserve_inventory(
                        &mut publisher,
                        &vm.name,
                        VmPublication::Upsert {
                            state: vm.state.clone(),
                            attrs,
                        },
                    )?
                }
            };
            self.send_reserved(request)?;
        }

        let cached_names = {
            let publisher = self.state.lock().unwrap();
            publisher
                .desired
                .iter()
                .filter(|(_, desired)| {
                    matches!(desired.publication, VmPublication::Upsert { .. })
                        && desired.revision <= checkpoint
                })
                .map(|(name, _)| name.clone())
                .collect::<BTreeSet<_>>()
        };
        let removed = cached_names
            .union(&host_vm_names)
            .filter(|name| !current.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        for vm_name in removed {
            let request = {
                let mut publisher = self.state.lock().unwrap();
                reserve_remove(&mut publisher, &vm_name, checkpoint)?
            };
            self.send_reserved(request)?;
        }
        Ok(())
    }

    fn send_reserved(&self, request: Option<ReservedPublication>) -> Result<(), String> {
        let Some(mut request) = request else {
            return Ok(());
        };
        loop {
            let result = self.send_publication(&request);
            let next = {
                let mut publisher = self.state.lock().unwrap();
                match publisher.desired.get_mut(&request.vm_name) {
                    Some(current) if current.revision == request.revision => {
                        current.confirmed = result.is_ok();
                        None
                    }
                    Some(current) => Some(ReservedPublication {
                        vm_name: request.vm_name.clone(),
                        publication: current.publication.clone(),
                        revision: current.revision,
                    }),
                    None => None,
                }
            };
            match next {
                Some(newer) => request = newer,
                None => return result,
            }
        }
    }

    fn send_publication(&self, request: &ReservedPublication) -> Result<(), String> {
        match &request.publication {
            VmPublication::Upsert { state, attrs } => {
                self.host
                    .publish_entity("upsert", "vm", &request.vm_name, state, attrs.clone())
            }
            VmPublication::Remove => {
                self.host
                    .publish_entity("remove", "vm", &request.vm_name, "", json!({}))
            }
        }
    }
}

fn reserve_authoritative(
    publisher: &mut PublisherState,
    vm_name: &str,
    publication: VmPublication,
) -> Result<Option<ReservedPublication>, String> {
    let existing = publisher.desired.get(vm_name).cloned();
    if existing
        .as_ref()
        .is_some_and(|desired| desired.publication == publication && desired.confirmed)
    {
        let revision = advance_revision(publisher)?;
        publisher.desired.get_mut(vm_name).unwrap().revision = revision;
        return Ok(None);
    }
    reserve_changed(publisher, vm_name, publication)
}

fn reserve_inventory(
    publisher: &mut PublisherState,
    vm_name: &str,
    publication: VmPublication,
) -> Result<Option<ReservedPublication>, String> {
    if publisher
        .desired
        .get(vm_name)
        .is_some_and(|desired| desired.publication == publication && desired.confirmed)
    {
        return Ok(None);
    }
    reserve_changed(publisher, vm_name, publication)
}

fn reserve_remove(
    publisher: &mut PublisherState,
    vm_name: &str,
    checkpoint: u64,
) -> Result<Option<ReservedPublication>, String> {
    if publisher
        .desired
        .get(vm_name)
        .is_some_and(|desired| desired.revision > checkpoint)
    {
        return Ok(None);
    }
    reserve_inventory(publisher, vm_name, VmPublication::Remove)
}

fn reserve_changed(
    publisher: &mut PublisherState,
    vm_name: &str,
    publication: VmPublication,
) -> Result<Option<ReservedPublication>, String> {
    let revision = advance_revision(publisher)?;
    publisher.desired.insert(
        vm_name.into(),
        DesiredVm {
            publication: publication.clone(),
            revision,
            confirmed: false,
        },
    );
    Ok(Some(ReservedPublication {
        vm_name: vm_name.into(),
        publication,
        revision,
    }))
}

fn advance_revision(publisher: &mut PublisherState) -> Result<u64, String> {
    let revision = publisher
        .revision
        .checked_add(1)
        .ok_or_else(|| "VM entity revision overflow".to_string())?;
    publisher.revision = revision;
    Ok(revision)
}

fn snapshot_attrs(snapshot: &RuntimeSnapshot) -> Value {
    let vm = snapshot.vm.as_ref();
    let record = vm.and_then(|vm| vm.record.as_ref());
    json!({
        "projectId":snapshot.project_id,
        "project":snapshot.display_name,
        "cwd":snapshot.cwd,
        "management":vm.map(|vm| vm.management.as_str()).unwrap_or("missing"),
        "guestWorkspace":record.map(|record| record.workspace.guest_path.as_str()).unwrap_or(""),
        "modules":record.map(|record| record.modules.as_slice()).unwrap_or(&[]),
        "mounts":record.map(|record| record.mounts.as_slice()).unwrap_or(&[]),
        "resources":record.map(|record| &record.resources),
        "shellCommand":snapshot.shell_command,
        "createdSpec":snapshot.created_spec,
        "environment":snapshot.environment
    })
}

fn inventory_attrs<S: RuntimeService>(service: &S, vm: &InventoryVm) -> Value {
    match &vm.record {
        Some(record) => {
            let project = record
                .workspace
                .host_path
                .as_deref()
                .and_then(|path| ProjectIdentity::from_path(Path::new(path)).ok());
            json!({
                "projectId":project.as_ref().map(|item| item.project_id.as_str()),
                "project":project.as_ref().map(|item| item.display_name.as_str()),
                "cwd":project.as_ref().map(|item| item.canonical_path.to_string_lossy().into_owned()),
                "management":vm.management,
                "guestWorkspace":record.workspace.guest_path,
                "modules":record.modules,
                "mounts":record.mounts,
                "resources":record.resources,
                "shellCommand":service.shell_command(&vm.name, true),
                "createdSpec":false,
                "environment":Value::Null
            })
        }
        None => json!({
            "projectId":Value::Null,
            "project":Value::Null,
            "cwd":Value::Null,
            "management":vm.management,
            "guestWorkspace":"",
            "modules":[],
            "mounts":[],
            "resources":Value::Null,
            "shellCommand":service.shell_command(&vm.name, false),
            "createdSpec":false,
            "environment":Value::Null
        }),
    }
}

fn preserve_running_bootstrap(existing: &DesiredVm, state: &str, attrs: &mut Value) {
    let VmPublication::Upsert {
        state: existing_state,
        attrs: existing_attrs,
    } = &existing.publication
    else {
        return;
    };
    if state != "running"
        || existing_state != "running"
        || !same_runtime_binding(existing_attrs, attrs)
        || attrs
            .get("environment")
            .is_some_and(|environment| !environment.is_null())
    {
        return;
    }
    let Some(fields) = attrs.as_object_mut() else {
        return;
    };
    for key in ["environment", "createdSpec"] {
        if let Some(value) = existing_attrs.get(key) {
            fields.insert(key.into(), value.clone());
        }
    }
}

fn same_runtime_binding(left: &Value, right: &Value) -> bool {
    const BINDING_FIELDS: [&str; 7] = [
        "projectId",
        "cwd",
        "management",
        "guestWorkspace",
        "modules",
        "mounts",
        "resources",
    ];
    left.get("projectId").and_then(Value::as_str).is_some()
        && right.get("projectId").and_then(Value::as_str).is_some()
        && BINDING_FIELDS
            .into_iter()
            .all(|field| left.get(field) == right.get(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_runtime_binding_never_inherits_a_ready_bootstrap_environment() {
        let existing = DesiredVm {
            publication: VmPublication::Upsert {
                state: "running".into(),
                attrs: json!({
                    "projectId":"project-a",
                    "cwd":"/work/a",
                    "management":"managed",
                    "guestWorkspace":"/home/dev/a",
                    "modules":["claude"],
                    "mounts":[{"hostPath":"/work/a/one"}],
                    "resources":{"cpus":2},
                    "environment":{"claude":"ready"},
                    "createdSpec":true
                }),
            },
            revision: 1,
            confirmed: true,
        };
        let mut changed = json!({
            "projectId":"project-a",
            "cwd":"/work/a",
            "management":"managed",
            "guestWorkspace":"/home/dev/a",
            "modules":["claude"],
            "mounts":[{"hostPath":"/work/a/two"}],
            "resources":{"cpus":2},
            "environment":Value::Null,
            "createdSpec":false
        });

        preserve_running_bootstrap(&existing, "running", &mut changed);

        assert!(changed["environment"].is_null());
        assert_eq!(changed["createdSpec"], false);
    }
}
