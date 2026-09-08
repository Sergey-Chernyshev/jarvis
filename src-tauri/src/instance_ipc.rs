use serde_json::{json, Value};
use tauri::AppHandle;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
static RUNTIME: OnceLock<Mutex<HashMap<String, Result<Value, String>>>> = OnceLock::new();

#[tauri::command]
pub async fn agent_instances_list(app: AppHandle) -> Result<Value, String> {
    let d = crate::daemon::Daemon::get(&app);
    let sessions = d.snapshot();
    tokio::task::spawn_blocking(move || {
        let config = crate::agent_instances::load(&crate::util::jarvis_dir())?;
        let registry = crate::session_identity::registry()?;
        let mut health = crate::install::instance_health()?;
        if let Ok(runtime) = RUNTIME.get_or_init(Default::default).lock() {
            for item in &mut health {
                match runtime.get(&item.instance_id) {
                    Some(Ok(response)) => crate::install::apply_runtime_hook_health(item, response),
                    Some(Err(error)) => item.errors.push(error.clone()), _ => {},
                }
            }
        }
        let mut result = serde_json::to_value(registry).map_err(|e| e.to_string())?;
        result["config"] = serde_json::to_value(config).map_err(|e|e.to_string())?;
        result["health"] = serde_json::to_value(health).map_err(|e|e.to_string())?;
        for row in result["instances"].as_array_mut().into_iter().flatten() {
            let id = row["id"].as_str().unwrap_or("");
            let owned: Vec<_> = sessions.iter().filter(|s| s.instance_id.as_deref() == Some(id)).collect();
            let last_hook = owned.iter().filter_map(|s| s.hook_last_at).max();
            row["observedChats"] = json!(owned.len()); row["lastHookAt"] = json!(last_hook);
            row["monitoring"] = json!("rollout+hooks");
            // Suggestions belong to this exact account, never the process default.
            let home = row["canonicalHome"].as_str().map(std::path::PathBuf::from);
            if let Some(home) = home {
                row["models"] = std::fs::read(home.join("models_cache.json")).ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .and_then(|cache| cache.get("models").cloned())
                    .map(|models| Value::Array(models.as_array().into_iter().flatten().filter_map(|model| {
                        let slug = model["slug"].as_str()?;
                        if slug.is_empty() || model["visibility"].as_str() == Some("hide") || slug.contains("review") { return None; }
                        Some(json!({"value":slug,"label":model["display_name"].as_str().filter(|label| !label.is_empty()).unwrap_or(slug)}))
                    }).take(100).collect())).unwrap_or_else(|| json!([]));
            }
        }
        Ok(result)
    }).await.map_err(|e|e.to_string())?
}

#[tauri::command]
pub async fn remote_source_repair(app: AppHandle, name: String, source_id: String) -> Result<Value, String> {
    let d = crate::daemon::Daemon::get(&app);
    let node = d.remotes.node(&name).ok_or("Узел не найден")?;
    node.client()?.repair_source(&source_id).await?;
    Ok(json!({"ok":true}))
}

#[tauri::command]
pub async fn agent_instances_save(app: AppHandle, config: crate::agent_instances::InstanceConfig) -> Result<Value, String> {
    let registry = tokio::task::spawn_blocking(move || {
        let registry = crate::agent_instances::save(&crate::util::jarvis_dir(), &config)?;
        crate::session_identity::invalidate();
        Ok::<_, String>(registry)
    }).await.map_err(|e|e.to_string())??;
    let d = crate::daemon::Daemon::get(&app);
    d.sessions.lock().unwrap_or_else(|e|e.into_inner()).retain(|_,s| s.remote.is_some()
        || s.instance_id.as_deref().map_or(true, |id| registry.resolve(Some(id)).is_ok()));
    d.push();
    serde_json::to_value(registry).map_err(|e|e.to_string())
}

#[tauri::command]
pub async fn agent_instances_repair(instance_ids: Option<Vec<String>>) -> Result<Value, String> {
    let selected = instance_ids.clone();
    let mut health = tokio::task::spawn_blocking(move || {
        let health = crate::install::repair_hooks_for_instances(instance_ids.as_deref(), &|_| {})?;
        crate::session_identity::invalidate();
        Ok::<_, String>(health)
    }).await.map_err(|e|e.to_string())??;
    let registry = crate::session_identity::registry()?;
    for item in health.iter_mut().filter(|h| h.enabled && h.rules_installed && selected.as_ref().map_or(true, |ids| ids.contains(&h.instance_id))) {
        let response = match registry.launch_spec(Some(&item.instance_id)) {
            Ok(spec) => crate::codex_hooks::reconcile(&spec.program, &spec.codex_home, std::path::Path::new(&item.hook_bin), true).await,
            Err(error) => Err(error),
        };
        match &response {
            Ok(response) => crate::install::apply_runtime_hook_health(item, response),
            Err(error) => item.errors.push(error.clone()),
        }
        if let Ok(mut runtime) = RUNTIME.get_or_init(Default::default).lock() { runtime.insert(item.instance_id.clone(), response); }
    }
    serde_json::to_value(health).map_err(|e|e.to_string())
}
