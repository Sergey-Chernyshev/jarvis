use std::collections::HashSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::entities::Entity;

const MAX_PROJECT_PROFILES: usize = 128;
const MAX_PROJECT_FOLDERS: usize = 512;
const MAX_PATH_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfile {
    pub project_id: String,
    pub project: String,
    pub cwd: String,
    pub start_with_jarvis: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_id: String,
    pub project: String,
    pub canonical_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFolder {
    pub project_id: String,
    pub project: String,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectManagerView {
    #[default]
    List,
    Cards,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectManagerState {
    pub folders: Vec<ProjectFolder>,
    pub favorite_project_ids: Vec<String>,
    pub view: ProjectManagerView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavoriteMove {
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentVmFocus {
    pub project_id: String,
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentVmDeepLink {
    pub kind: String,
    pub project_id: String,
    pub project: String,
    pub cwd: String,
    pub run_id: Option<String>,
}

#[derive(Default)]
pub struct Coordinator {
    focus: Mutex<Option<AgentVmFocus>>,
}

impl Coordinator {
    pub fn set_focus(&self, focus: Option<AgentVmFocus>) {
        *self.focus.lock().unwrap() = focus;
    }

    pub fn clear_focus(&self) {
        self.set_focus(None);
    }

    pub fn focus(&self) -> Option<AgentVmFocus> {
        self.focus.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeNotification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub speak: String,
    pub target: Value,
}

pub fn identity_for_path(path: &Path) -> Result<ProjectIdentity, String> {
    let canonical_path = fs::canonicalize(path)
        .map_err(|_| "project path недоступен для Agent VM profile".to_string())?;
    if !canonical_path.is_dir() {
        return Err("Agent VM profile требует project directory".into());
    }
    Ok(identity_for_canonical(&canonical_path))
}

fn identity_for_canonical(canonical_path: &Path) -> ProjectIdentity {
    let project = canonical_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("project")
        .to_string();
    let hash = fnv1a64(canonical_path.as_os_str().as_bytes());
    ProjectIdentity {
        project_id: format!("project-{hash:016x}"),
        project,
        canonical_path: canonical_path.to_path_buf(),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn profiles_from_settings(settings: &Value) -> Vec<ProjectProfile> {
    let mut seen = HashSet::new();
    settings
        .pointer("/agentVm/projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<ProjectProfile>(value.clone()).ok())
        .filter(|profile| {
            profile.start_with_jarvis
                && valid_object_id(&profile.project_id)
                && !profile.project.is_empty()
                && profile.project.len() <= 512
                && Path::new(&profile.cwd).is_absolute()
                && profile.cwd.len() <= MAX_PATH_BYTES
                && seen.insert(profile.project_id.clone())
        })
        .take(MAX_PROJECT_PROFILES)
        .collect()
}

pub fn update_profile_block(
    settings: &Value,
    cwd: &Path,
    start_with_jarvis: bool,
) -> Result<(Value, ProjectProfile), String> {
    let identity = identity_for_path(cwd)?;
    let profile = ProjectProfile {
        project_id: identity.project_id,
        project: identity.project,
        cwd: identity.canonical_path.to_string_lossy().into_owned(),
        start_with_jarvis,
    };
    let mut block = settings
        .get("agentVm")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut profiles = profiles_from_settings(&json!({"agentVm":block}));
    profiles.retain(|item| item.project_id != profile.project_id);
    if start_with_jarvis {
        if profiles.len() >= MAX_PROJECT_PROFILES {
            return Err("слишком много Agent VM project profiles".into());
        }
        profiles.push(profile.clone());
        profiles.sort_by(|left, right| left.project.cmp(&right.project));
    }
    block.insert(
        "projects".into(),
        serde_json::to_value(profiles)
            .map_err(|_| "не сериализовать Agent VM profiles".to_string())?,
    );
    Ok((Value::Object(block), profile))
}

pub fn project_manager_state_from_settings(settings: &Value) -> ProjectManagerState {
    let mut seen_ids = HashSet::new();
    let mut seen_paths = HashSet::new();
    let folders = settings
        .pointer("/projectManager/folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<ProjectFolder>(value.clone()).ok())
        .filter(|folder| {
            valid_object_id(&folder.project_id)
                && !folder.project.trim().is_empty()
                && folder.project.len() <= 512
                && Path::new(&folder.cwd).is_absolute()
                && folder.cwd.len() <= MAX_PATH_BYTES
                && seen_ids.insert(folder.project_id.clone())
                && seen_paths.insert(folder.cwd.clone())
        })
        .take(MAX_PROJECT_FOLDERS)
        .collect::<Vec<_>>();
    let folder_ids = folders
        .iter()
        .map(|folder| folder.project_id.as_str())
        .collect::<HashSet<_>>();
    let mut seen_favorites = HashSet::new();
    let favorite_project_ids = settings
        .pointer("/projectManager/favoriteProjectIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|project_id| {
            folder_ids.contains(*project_id)
                && valid_object_id(project_id)
                && seen_favorites.insert((*project_id).to_string())
        })
        .map(str::to_string)
        .take(MAX_PROJECT_FOLDERS)
        .collect();
    let view = settings
        .pointer("/projectManager/view")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

    ProjectManagerState {
        folders,
        favorite_project_ids,
        view,
    }
}

fn project_manager_block(settings: &Value) -> Map<String, Value> {
    settings
        .get("projectManager")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn serialize_project_manager_block(
    settings: &Value,
    state: &ProjectManagerState,
) -> Result<Value, String> {
    let mut block = project_manager_block(settings);
    block.insert(
        "folders".into(),
        serde_json::to_value(&state.folders)
            .map_err(|_| "не сериализовать каталог проектов".to_string())?,
    );
    block.insert(
        "favoriteProjectIds".into(),
        serde_json::to_value(&state.favorite_project_ids)
            .map_err(|_| "не сериализовать избранные проекты".to_string())?,
    );
    block.insert(
        "view".into(),
        serde_json::to_value(state.view)
            .map_err(|_| "не сериализовать вид проектов".to_string())?,
    );
    Ok(Value::Object(block))
}

fn canonical_project_folder(path: &Path) -> Result<ProjectFolder, String> {
    let identity = identity_for_path(path)?;
    Ok(ProjectFolder {
        project_id: identity.project_id,
        project: identity.project,
        cwd: identity.canonical_path.to_string_lossy().into_owned(),
    })
}

fn upsert_project_folder(
    state: &mut ProjectManagerState,
    path: &Path,
) -> Result<ProjectFolder, String> {
    let folder = canonical_project_folder(path)?;
    state
        .folders
        .retain(|item| item.project_id != folder.project_id && item.cwd != folder.cwd);
    if state.folders.len() >= MAX_PROJECT_FOLDERS {
        return Err("слишком много папок в менеджере проектов".into());
    }
    state.folders.push(folder.clone());
    state.folders.sort_by(|left, right| {
        left.project
            .to_lowercase()
            .cmp(&right.project.to_lowercase())
            .then_with(|| left.cwd.cmp(&right.cwd))
    });
    Ok(folder)
}

pub fn update_project_manager_folder(
    settings: &Value,
    cwd: &Path,
) -> Result<(Value, ProjectManagerState, ProjectFolder), String> {
    let mut state = project_manager_state_from_settings(settings);
    let folder = upsert_project_folder(&mut state, cwd)?;
    let block = serialize_project_manager_block(settings, &state)?;
    Ok((block, state, folder))
}

pub fn update_project_manager_favorite(
    settings: &Value,
    cwd: &Path,
    favorite: bool,
) -> Result<(Value, ProjectManagerState, ProjectFolder), String> {
    let mut state = project_manager_state_from_settings(settings);
    let folder = upsert_project_folder(&mut state, cwd)?;
    state
        .favorite_project_ids
        .retain(|project_id| project_id != &folder.project_id);
    if favorite {
        state.favorite_project_ids.push(folder.project_id.clone());
    }
    let block = serialize_project_manager_block(settings, &state)?;
    Ok((block, state, folder))
}

pub fn move_project_manager_favorite(
    settings: &Value,
    project_id: &str,
    direction: FavoriteMove,
) -> Result<(Value, ProjectManagerState), String> {
    if !valid_object_id(project_id) {
        return Err("Некорректный идентификатор проекта".into());
    }
    let mut state = project_manager_state_from_settings(settings);
    let Some(index) = state
        .favorite_project_ids
        .iter()
        .position(|value| value == project_id)
    else {
        return Err("Проект не находится в избранном".into());
    };
    let destination = match direction {
        FavoriteMove::Up => index.saturating_sub(1),
        FavoriteMove::Down => (index + 1).min(state.favorite_project_ids.len() - 1),
    };
    state.favorite_project_ids.swap(index, destination);
    let block = serialize_project_manager_block(settings, &state)?;
    Ok((block, state))
}

pub fn update_project_manager_view(
    settings: &Value,
    view: ProjectManagerView,
) -> Result<(Value, ProjectManagerState), String> {
    let mut state = project_manager_state_from_settings(settings);
    state.view = view;
    let block = serialize_project_manager_block(settings, &state)?;
    Ok((block, state))
}

pub fn valid_object_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn parse_deep_link(value: &Value) -> Option<AgentVmDeepLink> {
    let link = serde_json::from_value::<AgentVmDeepLink>(value.clone()).ok()?;
    if link.kind != "agent-vm"
        || !valid_object_id(&link.project_id)
        || link.project.is_empty()
        || link.project.len() > 512
        || link.cwd.is_empty()
        || link.cwd.len() > MAX_PATH_BYTES
        || !Path::new(&link.cwd).is_absolute()
        || link
            .run_id
            .as_deref()
            .is_some_and(|run_id| !valid_object_id(run_id))
    {
        return None;
    }
    Some(link)
}

fn attr<'a>(entity: &'a Entity, key: &str) -> &'a str {
    entity.attrs.get(key).and_then(Value::as_str).unwrap_or("")
}

fn target_for(entity: &Entity) -> Option<Value> {
    let project_id = attr(entity, "projectId");
    let cwd = attr(entity, "cwd");
    if !valid_object_id(project_id)
        || cwd.is_empty()
        || cwd.len() > MAX_PATH_BYTES
        || !Path::new(cwd).is_absolute()
    {
        return None;
    }
    let run_id = attr(entity, "runId");
    Some(json!({
        "kind":"agent-vm",
        "projectId":project_id,
        "project":attr(entity, "project"),
        "cwd":cwd,
        "runId":if valid_object_id(run_id) { Value::String(run_id.into()) } else { Value::Null }
    }))
}

fn project_title(entity: &Entity) -> String {
    let title = attr(entity, "project").trim();
    if title.is_empty() {
        "Проект".into()
    } else {
        title.chars().take(120).collect()
    }
}

fn is_focused(focus: Option<&AgentVmFocus>, entity: &Entity) -> bool {
    focus.is_some_and(|focus| focus.project_id == attr(entity, "projectId"))
}

pub fn notification_for(
    previous: Option<&Entity>,
    current: &Entity,
    focus: Option<&AgentVmFocus>,
) -> Option<RuntimeNotification> {
    if current.owner != "plugin:agent-vm"
        || previous.is_some_and(|before| before.state == current.state)
    {
        return None;
    }
    let target = target_for(current)?;
    let project_id = attr(current, "projectId");
    let project = project_title(current);
    let command = attr(current, "command");
    let recovered = current
        .attrs
        .get("recovered")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (scope, transition, title, body, kind, speak) = match current.kind.as_str() {
        "vm" if matches!(current.state.as_str(), "running" | "ready")
            && previous.is_some()
            && previous.is_none_or(|before| {
                !matches!(before.state.as_str(), "running" | "ready" | "working")
            })
            && !is_focused(focus, current) =>
        {
            (
                "runtime",
                "ready",
                format!("{project} — VM готова"),
                "Среда готова к работе".into(),
                "done",
                format!("{project}: виртуальная машина готова"),
            )
        }
        "vm" if current.state == "error" => (
            "runtime",
            "error",
            format!("{project} — ошибка VM"),
            "Открой проект, чтобы повторить запуск".into(),
            "error",
            format!("{project}: ошибка виртуальной машины"),
        ),
        "agent_run" if current.state == "waiting" && (!recovered || previous.is_some()) => (
            "run",
            "waiting",
            format!("{project} — нужен ответ"),
            "Агент ждёт твоего решения".into(),
            "waiting",
            format!("{project}: агент ждёт ответа"),
        ),
        "agent_run" if current.state == "completed" && !is_focused(focus, current) => (
            "run",
            "completed",
            format!("{project} — готово"),
            "Задача завершена".into(),
            "done",
            format!("{project}: задача завершена"),
        ),
        "agent_run"
            if matches!(current.state.as_str(), "failed" | "error" | "interrupted")
                && (previous.is_some() || recovered) =>
        {
            (
                "run",
                current.state.as_str(),
                format!("{project} — запуск прерван"),
                "Открой проект, чтобы посмотреть состояние и продолжить".into(),
                "error",
                format!("{project}: запуск агента прерван"),
            )
        }
        "operation"
            if current.state == "error"
                && previous.is_some()
                && matches!(
                    command,
                    "runtime.ensure" | "runtime.restart" | "runtime.stop" | "runtime.send"
                ) =>
        {
            (
                "operation",
                "error",
                format!("{project} — ошибка Agent VM"),
                "Открой проект, чтобы посмотреть состояние и повторить действие".into(),
                "error",
                format!("{project}: ошибка Agent VM"),
            )
        }
        _ => return None,
    };
    let object_id = match scope {
        "run" => attr(current, "runId"),
        "operation" => attr(current, "requestId"),
        _ => "",
    };
    Some(RuntimeNotification {
        id: if object_id.is_empty() {
            format!("agent-vm:{project_id}:{scope}:{transition}")
        } else {
            format!("agent-vm:{project_id}:{scope}:{object_id}:{transition}")
        },
        title,
        body,
        kind: kind.into(),
        speak,
        target,
    })
}

pub fn route_transition(
    daemon: &Arc<crate::daemon::Daemon>,
    previous: Option<&Entity>,
    current: &Entity,
) {
    let focus = daemon.agent_vm.focus();
    let Some(notification) = notification_for(previous, current, focus.as_ref()) else {
        return;
    };
    daemon.notify_id_voiced_target(
        &notification.id,
        &notification.title,
        &notification.body,
        None,
        &notification.kind,
        Some(&notification.speak),
        Some(&notification.target),
    );
}

pub async fn autostart_profiles(daemon: Arc<crate::daemon::Daemon>) {
    let profiles = profiles_from_settings(&daemon.settings.load());
    for profile in profiles {
        let identity = match identity_for_path(Path::new(&profile.cwd)) {
            Ok(identity) if identity.project_id == profile.project_id => identity,
            _ => {
                crate::log::line(&format!(
                    "[agent-vm] autostart skipped for unavailable project {}",
                    profile.project
                ));
                continue;
            }
        };
        let args = json!({
            "projectId":identity.project_id,
            "cwd":identity.canonical_path
        });
        let mut accepted = None;
        for _ in 0..120 {
            match daemon
                .plugins
                .enqueue_command("agent-vm", "runtime.ensure", args.clone())
            {
                Ok(value) => {
                    accepted = value
                        .get("requestId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
        let Some(request_id) = accepted else {
            notify_autostart_error(&daemon, &profile);
            break;
        };
        let operation_id = format!("operation.{request_id}");
        let deadline = Instant::now() + Duration::from_secs(30 * 60);
        loop {
            let operation = daemon.entities.get(&operation_id);
            let terminal = operation
                .as_ref()
                .filter(|entity| matches!(entity.state.as_str(), "done" | "error"));
            if terminal.is_some() {
                let _ = daemon.entities.remove("plugin:agent-vm", &operation_id);
                crate::windows::emit_to_panel(&daemon.app, "entities", &daemon.entities.snapshot());
                break;
            }
            if operation.as_ref().is_some_and(|entity| entity.stale) || Instant::now() >= deadline {
                notify_autostart_error(&daemon, &profile);
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn notify_autostart_error(daemon: &Arc<crate::daemon::Daemon>, profile: &ProjectProfile) {
    let target = json!({
        "kind":"agent-vm",
        "projectId":profile.project_id,
        "project":profile.project,
        "cwd":profile.cwd,
        "runId":Value::Null
    });
    daemon.notify_id_voiced_target(
        &format!("agent-vm:{}:runtime:autostart-error", profile.project_id),
        &format!("{} — VM не запустилась", profile.project),
        "Открой проект, чтобы повторить запуск",
        None,
        "error",
        Some(&format!(
            "{}: виртуальная машина не запустилась",
            profile.project
        )),
        Some(&target),
    );
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;
    use crate::entities::Entity;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn entity(kind: &str, id: &str, state: &str, attrs: serde_json::Value) -> Entity {
        Entity {
            id: format!("{kind}.{id}"),
            kind: kind.into(),
            owner: "plugin:agent-vm".into(),
            state: state.into(),
            attrs,
            updated_at: 1,
            stale: false,
        }
    }

    #[test]
    fn profile_patch_uses_canonical_identity_and_preserves_agent_vm_settings() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-profile-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let settings = json!({
            "agentVm": {
                "futureFlag": true,
                "projects": []
            }
        });

        let (block, profile) = update_profile_block(&settings, &root, true).unwrap();

        assert_eq!(block["futureFlag"], true);
        assert_eq!(block["projects"][0]["projectId"], profile.project_id);
        assert_eq!(
            block["projects"][0]["cwd"],
            fs::canonicalize(&root).unwrap().to_string_lossy().as_ref()
        );
        assert!(profile.start_with_jarvis);

        let (block, _) = update_profile_block(&json!({"agentVm":block}), &root, false).unwrap();
        assert_eq!(block["projects"], json!([]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn autostart_profiles_are_deduplicated_bounded_and_only_explicitly_enabled() {
        let settings = json!({
            "agentVm": {
                "projects": [
                    {"projectId":"project-a","project":"a","cwd":"/tmp/a","startWithJarvis":true},
                    {"projectId":"project-a","project":"a","cwd":"/tmp/a","startWithJarvis":true},
                    {"projectId":"project-b","project":"b","cwd":"/tmp/b","startWithJarvis":false},
                    {"projectId":"../bad","project":"bad","cwd":"relative","startWithJarvis":true}
                ]
            }
        });

        let profiles = profiles_from_settings(&settings);

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].project_id, "project-a");
    }

    #[test]
    fn project_manager_state_sanitizes_folders_favorites_and_view() {
        let settings = json!({
            "projectManager": {
                "folders": [
                    {"projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"},
                    {"projectId":"project-a","project":"duplicate","cwd":"/tmp/duplicate"},
                    {"projectId":"../bad","project":"bad","cwd":"relative"},
                    {"projectId":"project-b","project":"beta","cwd":"/tmp/beta"}
                ],
                "favoriteProjectIds": ["project-b", "missing", "project-b", "project-a"],
                "view": "cards"
            }
        });

        let state = project_manager_state_from_settings(&settings);

        assert_eq!(
            state
                .folders
                .iter()
                .map(|folder| folder.project_id.as_str())
                .collect::<Vec<_>>(),
            vec!["project-a", "project-b"]
        );
        assert_eq!(
            state.favorite_project_ids,
            vec!["project-b".to_string(), "project-a".to_string()]
        );
        assert_eq!(state.view, ProjectManagerView::Cards);
    }

    #[test]
    fn project_manager_adds_canonical_folder_without_clobbering_future_settings() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-project-manager-folder-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let settings = json!({
            "projectManager": {
                "futureFlag": true,
                "folders": []
            }
        });

        let (block, state, folder) = update_project_manager_folder(&settings, &root).unwrap();

        assert_eq!(block["futureFlag"], true);
        assert_eq!(state.folders, vec![folder.clone()]);
        assert_eq!(
            folder.cwd,
            fs::canonicalize(&root).unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(
            folder.project_id,
            identity_for_path(&root).unwrap().project_id
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_manager_favorites_move_only_inside_explicit_order() {
        let alpha = std::env::temp_dir().join(format!(
            "jarvis-project-manager-alpha-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let beta = std::env::temp_dir().join(format!(
            "jarvis-project-manager-beta-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&alpha).unwrap();
        fs::create_dir_all(&beta).unwrap();

        let (block, _, alpha_folder) =
            update_project_manager_favorite(&json!({}), &alpha, true).unwrap();
        let (block, state, beta_folder) =
            update_project_manager_favorite(&json!({"projectManager":block}), &beta, true).unwrap();
        assert_eq!(
            state.favorite_project_ids,
            vec![
                alpha_folder.project_id.clone(),
                beta_folder.project_id.clone()
            ]
        );

        let (block, moved) = move_project_manager_favorite(
            &json!({"projectManager":block}),
            &beta_folder.project_id,
            FavoriteMove::Up,
        )
        .unwrap();
        assert_eq!(
            moved.favorite_project_ids,
            vec![
                beta_folder.project_id.clone(),
                alpha_folder.project_id.clone()
            ]
        );

        let (_, unstarred, _) =
            update_project_manager_favorite(&json!({"projectManager":block}), &beta, false)
                .unwrap();
        assert_eq!(
            unstarred.favorite_project_ids,
            vec![alpha_folder.project_id]
        );
        assert!(unstarred
            .folders
            .iter()
            .any(|folder| folder.project_id == beta_folder.project_id));

        fs::remove_dir_all(alpha).unwrap();
        fs::remove_dir_all(beta).unwrap();
    }

    #[test]
    fn project_manager_view_update_is_typed_and_preserves_catalog() {
        let settings = json!({
            "projectManager": {
                "folders": [
                    {"projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"}
                ],
                "favoriteProjectIds": ["project-a"],
                "view": "list",
                "futureFlag": 42
            }
        });

        let (block, state) =
            update_project_manager_view(&settings, ProjectManagerView::Cards).unwrap();

        assert_eq!(block["futureFlag"], 42);
        assert_eq!(state.view, ProjectManagerView::Cards);
        assert_eq!(state.favorite_project_ids, vec!["project-a"]);
        assert_eq!(state.folders.len(), 1);
    }

    #[test]
    fn runtime_ready_is_transition_deduped_and_suppressed_for_open_project() {
        let previous = entity(
            "vm",
            "vm-a",
            "stopped",
            json!({"projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"}),
        );
        let ready = entity(
            "vm",
            "vm-a",
            "running",
            json!({"projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"}),
        );

        let notification = notification_for(Some(&previous), &ready, None).unwrap();
        assert_eq!(notification.id, "agent-vm:project-a:runtime:ready");
        assert_eq!(notification.kind, "done");
        assert!(notification_for(Some(&ready), &ready, None).is_none());
        assert!(notification_for(
            Some(&previous),
            &ready,
            Some(&AgentVmFocus {
                project_id: "project-a".into(),
                run_id: None,
            }),
        )
        .is_none());
    }

    #[test]
    fn waiting_done_and_recovered_interrupted_route_without_raw_agent_text() {
        let working = entity(
            "agent_run",
            "run-a",
            "working",
            json!({
                "runId":"run-a","projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"
            }),
        );
        let waiting = entity(
            "agent_run",
            "run-a",
            "waiting",
            json!({
                "runId":"run-a","projectId":"project-a","project":"alpha","cwd":"/tmp/alpha",
                "latestEvent":{"payload":{"text":"synthetic private-looking text"}}
            }),
        );
        let completed = entity(
            "agent_run",
            "run-a",
            "completed",
            json!({
                "runId":"run-a","projectId":"project-a","project":"alpha","cwd":"/tmp/alpha",
                "latestEvent":{"payload":{"text":"synthetic private-looking text"}}
            }),
        );
        let interrupted = entity(
            "agent_run",
            "run-a",
            "interrupted",
            json!({
                "runId":"run-a","projectId":"project-a","project":"alpha","cwd":"/tmp/alpha",
                "recovered":true
            }),
        );

        let wait = notification_for(Some(&working), &waiting, None).unwrap();
        assert_eq!(wait.kind, "waiting");
        assert!(!wait.body.contains("private-looking"));

        let done = notification_for(Some(&working), &completed, None).unwrap();
        assert_eq!(done.kind, "done");
        assert!(!done.body.contains("private-looking"));

        let crash = notification_for(None, &interrupted, None).unwrap();
        assert_eq!(crash.kind, "error");
        assert_eq!(crash.target["runId"], "run-a");
    }

    #[test]
    fn failed_lifecycle_operation_routes_to_project_without_sensitive_error() {
        let started = entity(
            "operation",
            "agent-vm-42",
            "started",
            json!({
                "requestId":"agent-vm-42","command":"runtime.ensure",
                "projectId":"project-a","project":"alpha","cwd":"/tmp/alpha"
            }),
        );
        let failed = entity(
            "operation",
            "agent-vm-42",
            "error",
            json!({
                "requestId":"agent-vm-42","command":"runtime.ensure",
                "projectId":"project-a","project":"alpha","cwd":"/tmp/alpha",
                "error":"synthetic private-looking detail"
            }),
        );

        let notification = notification_for(Some(&started), &failed, None).unwrap();

        assert_eq!(
            notification.id,
            "agent-vm:project-a:operation:agent-vm-42:error"
        );
        assert_eq!(notification.kind, "error");
        assert_eq!(notification.target["runId"], Value::Null);
        assert!(!notification.body.contains("private-looking"));
    }

    #[test]
    fn project_identity_matches_agent_vm_fnv_contract() {
        let path = PathBuf::from("/tmp/synthetic-agent-vm-contract");
        let identity = identity_for_canonical(&path);

        assert!(identity.project_id.starts_with("project-"));
        assert_eq!(identity.project_id.len(), "project-".len() + 16);
        assert_eq!(identity.project, "synthetic-agent-vm-contract");
    }
}
