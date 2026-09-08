//! Stable provider/account identity at the hook boundary. Provider SIDs remain
//! unchanged for resume; UI keys also distinguish the machine and account.
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use crate::agent_instances::Registry;

static REGISTRY: OnceLock<Mutex<Option<(i64, Registry)>>> = OnceLock::new();

pub fn registry() -> Result<Registry, String> {
    let now = crate::util::now_ms();
    let mut cached = REGISTRY.get_or_init(Default::default).lock().map_err(|e| e.to_string())?;
    if let Some((at, registry)) = cached.as_ref().filter(|(at,_)| now - at < 15_000) {
        let _ = at;
        return Ok(registry.clone());
    }
    let registry = crate::agent_instances::load_registry(&crate::util::jarvis_dir())?;
    *cached = Some((now, registry.clone()));
    Ok(registry)
}

pub fn invalidate() {
    if let Ok(mut cached) = REGISTRY.get_or_init(Default::default).lock() { *cached = None; }
}

pub fn split_codex_key(key: &str, remote: Option<&str>) -> Option<(String, String)> {
    let key = remote.and_then(|node| key.strip_prefix(&format!("{node}:"))).unwrap_or(key);
    let rest = key.strip_prefix("codex:")?;
    let (instance, sid) = rest.split_once(':')?;
    if instance.is_empty() || sid.is_empty() { return None; }
    Some((instance.to_owned(), sid.to_owned()))
}

pub fn key(instance_id: &str, home: &str, sid: &str, remote: Option<&str>) -> String {
    // Preserve the original default-home IDs, independent of the selected
    // launch default. Other profiles have stable namespaces from first sight.
    let legacy = if remote.is_none() {
        let canonical = crate::agent_instances::canonical_home(Path::new(home)).ok();
        canonical.is_some() && canonical == crate::agent_instances::canonical_home(&crate::util::home_dir().join(".codex")).ok()
    } else { false };
    let local = if legacy { sid.to_owned() } else { format!("codex:{instance_id}:{sid}") };
    remote.map_or_else(|| local.clone(), |node| format!("{node}:{local}"))
}

pub fn normalize(event: &Value) -> Option<Value> {
    let local = event["agent"] == "codex" && event["remote"].as_str().filter(|v| !v.is_empty()).is_none();
    let registry = if local { Some(registry().ok()?) } else { None };
    normalize_with_registry(event, registry.as_ref())
}

fn normalize_with_registry(event: &Value, registry: Option<&Registry>) -> Option<Value> {
    let mut out = event.clone();
    if event["agent"] != "codex" { return Some(out); }
    let original = event["payload"]["session_id"].as_str()?;
    let remote = event["remote"].as_str().filter(|s| !s.is_empty());
    let mut home = event["providerHome"].as_str().filter(|s| !s.is_empty()).map(String::from);
    let mut instance = event["instanceId"].as_str().filter(|s| !s.is_empty()).map(String::from);
    let mut label = event["instanceLabel"].as_str().map(String::from);
    if remote.is_none() {
        let registry = registry?;
        let canonical = home.as_deref().map(|home| crate::agent_instances::canonical_home(Path::new(home))).transpose().ok()?;
        let found = if let Some(id) = instance.as_deref() {
            registry.instances.iter().find(|i| i.id == id)
        } else if let Some(home) = canonical.as_ref() {
            registry.instances.iter().find(|i| &i.canonical_home == home)
        } else {
            event["payload"]["transcript_path"].as_str().and_then(|p| registry.instance_for_transcript(Path::new(p)))
        };
        if let Some(found) = found {
            if !found.enabled || canonical.as_ref().is_some_and(|home| home != &found.canonical_home) { return None; }
            instance = Some(found.id.clone()); home = Some(found.canonical_home.to_string_lossy().into_owned());
            label = Some(found.label.clone());
        } else if let Some(canonical) = canonical {
            // Discovery may lag a newly launched profile by one refresh. Its
            // explicit home still has a stable namespace; never use raw SID.
            let id = crate::agent_instances::instance_id(&registry.machine, &canonical).ok()?;
            if instance.as_ref().is_some_and(|provided| provided != &id) { return None; }
            if registry.instances.iter().any(|i| i.canonical_home == canonical && !i.enabled) { return None; }
            instance = Some(id); home = Some(canonical.to_string_lossy().into_owned());
            label.get_or_insert_with(|| canonical.file_name().unwrap_or_default().to_string_lossy().into_owned());
        } else if instance.is_some() { return None; }
    }
    if let (Some(id), Some(home)) = (instance.as_deref(), home.as_deref()) {
        let scoped = remote.and_then(|node| original.strip_prefix(&format!("{node}:"))).unwrap_or(original);
        let raw = event["providerSessionId"].as_str().unwrap_or_else(|| scoped.strip_prefix(&format!("codex:{id}:")).unwrap_or(scoped));
        if raw.is_empty() { return None; }
        out["payload"]["session_id"] = Value::String(key(id, home, raw, remote));
        out["providerSessionId"] = raw.into();
        out["instanceId"] = id.into(); out["providerHome"] = home.into();
        if let Some(label) = label { out["instanceLabel"] = label.into(); }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn registry() -> Registry { Registry { instances: vec![], default_codex_instance:String::new(), machine:"local".into() } }
    #[test]
    fn undiscovered_explicit_homes_never_merge_the_same_provider_sid() {
        let r=registry();
        let event=|home| json!({"agent":"codex","providerHome":home,"payload":{"session_id":"same-sid"}});
        let first=normalize_with_registry(&event("/tmp/qa-codex-personal"),Some(&r)).unwrap();
        let second=normalize_with_registry(&event("/tmp/qa-codex-work"),Some(&r)).unwrap();
        assert_ne!(first["payload"]["session_id"],second["payload"]["session_id"]);
        assert_eq!(first["providerSessionId"],"same-sid");
        assert_eq!(normalize_with_registry(&first,Some(&r)).unwrap(),first);
    }
    #[test]
    fn remote_identity_is_idempotent_and_keeps_machine_namespace() {
        let original=json!({"agent":"codex","remote":"vm","instanceId":"profile","providerHome":"/home/qa/.codex","payload":{"session_id":"vm:same-sid"}});
        let first=normalize_with_registry(&original,None).unwrap();
        assert_eq!(first["payload"]["session_id"],"vm:codex:profile:same-sid");
        assert_eq!(normalize_with_registry(&first,None).unwrap(),first);
    }
    #[test]
    fn conflicting_profile_identity_and_disabled_profiles_are_rejected() {
        let mut r=registry();let path=crate::agent_instances::canonical_home(Path::new("/tmp/qa-codex-work")).unwrap();
        r.instances.push(crate::agent_instances::AgentInstance { id:"work".into(),agent:"codex".into(),machine:"local".into(),home:path.clone(),canonical_home:path,
            label:"Work".into(),enabled:true,exists:true,sources:vec![],launchers:vec![],cli:None });
        let mut event=json!({"agent":"codex","instanceId":"work","providerHome":"/tmp/qa-codex-personal","payload":{"session_id":"s"}});
        assert!(normalize_with_registry(&event,Some(&r)).is_none());
        event["providerHome"]=json!("/tmp/qa-codex-work");r.instances[0].enabled=false;
        assert!(normalize_with_registry(&event,Some(&r)).is_none());
    }
}
