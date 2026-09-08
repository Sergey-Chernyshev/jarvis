//! Saved project metadata, merged with discovered history and live sessions.
//! Saving/removing a project never creates, moves, or deletes its directory.

use crate::{daemon::Daemon, model::{Session, Status}, remote::RemoteStatus, settings::Store};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

fn machine_name(machine: &str) -> Result<String, String> {
    let machine = machine.trim();
    if machine.is_empty() || machine == "local" { return Ok("local".into()); }
    if machine.len() > 80 || !machine.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
        return Err("Некорректное имя машины".into());
    }
    Ok(machine.into())
}

fn clean_path(path: &str) -> Result<String, String> {
    if path.chars().any(char::is_control) { return Err("Путь проекта содержит управляющие символы".into()); }
    let path = path.trim();
    if !path.starts_with('/') || path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("Укажи абсолютный путь к каталогу проекта".into());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part { "" | "." => {}, ".." => { parts.pop(); }, _ => parts.push(part) }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn local_path(path: &str) -> Result<String, String> {
    if path.chars().any(char::is_control) { return Err("Путь проекта содержит управляющие символы".into()); }
    let path = path.trim();
    let expanded = if path == "~" { crate::util::home_dir().to_string_lossy().into_owned() }
        else if let Some(tail) = path.strip_prefix("~/") { crate::util::home_dir().join(tail).to_string_lossy().into_owned() }
        else { path.to_string() };
    // Resolve an existing path before lexically normalizing '..': a symlink's
    // parent belongs to its target, not to the spelling entered in the UI.
    if !expanded.starts_with('/') || expanded.chars().any(char::is_control) || expanded.len() > 4096 {
        return Err("Укажи абсолютный путь к каталогу проекта".into());
    }
    let directory = std::path::Path::new(&expanded);
    if directory.exists() {
        if !directory.is_dir() { return Err("Путь проекта должен указывать на каталог".into()); }
        return directory.canonicalize().map(|path| path.to_string_lossy().into_owned()).map_err(|e| e.to_string());
    }
    clean_path(&expanded)
}

fn identity(value: &Value) -> Option<(String, String)> {
    let machine = machine_name(value.get("machine").and_then(Value::as_str).unwrap_or("local")).ok()?;
    let cwd = clean_path(value.get("cwd")?.as_str()?).ok()?;
    Some((machine, cwd))
}

fn label(cwd: &str) -> String {
    cwd.rsplit('/').find(|part| !part.is_empty()).unwrap_or("/").to_string()
}

pub fn save(store: &Store, project: Value) -> Result<Value, String> {
    let Some(fields) = project.as_object() else { return Err("Ожидаю параметры проекта".into()); };
    let raw_machine = fields.get("machine").map(|value| value.as_str().ok_or("Машина должна быть строкой")).transpose()?.unwrap_or("local");
    let machine = machine_name(raw_machine)?;
    let raw_cwd = fields.get("cwd").and_then(Value::as_str).ok_or("Не указан каталог проекта")?;
    let cwd = if machine == "local" { local_path(raw_cwd)? } else { clean_path(raw_cwd)? };
    let name = fields.get("name").map(|value| value.as_str().ok_or("Название проекта должно быть строкой"))
        .transpose()?.map(str::trim).map(str::to_string);
    if name.as_ref().is_some_and(|name| name.chars().count() > 160 || name.chars().any(char::is_control)) {
        return Err("Название проекта должно быть короче 160 символов, без переводов строк".into());
    }
    let pinned = fields.get("pinned").map(|value| value.as_bool().ok_or("pinned должен быть true или false")).transpose()?;
    // Omission preserves an existing choice (for example when pinning); null
    // explicitly restores automatic artwork discovery.
    let avatar = fields.get("avatar").map(crate::project_icons::validate_avatar).transpose()?;
    let key = (machine.clone(), cwd.clone());
    let now = crate::util::now_ms();
    let next = store.try_update(|root| {
        let projects = root.entry("projects").or_insert_with(|| json!([])).as_array_mut()
            .ok_or("Реестр проектов повреждён: ожидался массив")?;
        let index = projects.iter().position(|value| identity(value).as_ref() == Some(&key));
        let mut saved = index.map(|index| projects[index].clone()).unwrap_or_else(|| json!({"createdAt":now}));
        saved["id"] = json!(format!("{}:{}", machine, cwd));
        saved["machine"] = json!(machine);
        saved["cwd"] = json!(cwd);
        saved["updatedAt"] = json!(now);
        saved["saved"] = json!(true);
        if let Some(name) = &name { saved["name"] = json!(if name.is_empty() { label(&cwd) } else { name.clone() }); }
        else if saved.get("name").and_then(Value::as_str).is_none() { saved["name"] = json!(label(&cwd)); }
        if let Some(pinned) = pinned { saved["pinned"] = json!(pinned); }
        else if saved.get("pinned").and_then(Value::as_bool).is_none() { saved["pinned"] = json!(false); }
        if let Some(avatar) = &avatar { saved["avatar"] = avatar.clone(); }
        match index { Some(index) => projects[index] = saved, None => projects.push(saved) }
        Ok(())
    })?;
    next["projects"].as_array().and_then(|items| items.iter().find(|value| identity(value).as_ref() == Some(&key)))
        .cloned().ok_or_else(|| "Проект не сохранился".into())
}

pub fn remove(store: &Store, machine: &str, cwd: &str) -> Result<Value, String> {
    let machine = machine_name(machine)?;
    let cwd = clean_path(cwd)?;
    let key = (machine.clone(), cwd.clone());
    store.try_update(|root| {
        if let Some(projects) = root.get_mut("projects") {
            let projects = projects.as_array_mut().ok_or("Реестр проектов повреждён: ожидался массив")?;
            projects.retain(|value| identity(value).as_ref() != Some(&key));
        }
        Ok(())
    })?;
    Ok(json!({"machine":machine,"cwd":cwd}))
}

struct Group {
    machine: String,
    cwd: String,
    name: String,
    pinned: bool,
    saved: bool,
    avatar: Value,
    last_at: i64,
    history_counts: HashMap<String, usize>,
    sessions: HashMap<String, Value>,
}

fn group<'a>(groups: &'a mut HashMap<(String, String), Group>, machine: &str, cwd: &str) -> &'a mut Group {
    groups.entry((machine.into(), cwd.into())).or_insert_with(|| Group {
        machine: machine.into(), cwd: cwd.into(), name: label(cwd), pinned: false, saved: false,
        avatar: Value::Null, last_at: 0, history_counts: HashMap::new(), sessions: HashMap::new(),
    })
}

fn connection(machine: &str, remotes: &[RemoteStatus]) -> Value {
    if machine == "local" { return json!({"kind":"local","name":"Эта машина","online":true,"error":""}); }
    match remotes.iter().find(|remote| remote.name == machine) {
        Some(remote) => json!({"kind":"remote","name":machine,"online":remote.connected,"error":remote.error,"sshHost":remote.ssh_host,"outdated":remote.outdated}),
        None => json!({"kind":"remote","name":machine,"online":false,"error":"Машина не настроена"}),
    }
}

fn merge(saved: &[Value], history: &[(String, Value)], live: &[Session], remotes: &[RemoteStatus], scope: Option<&str>) -> Vec<Value> {
    let accepts = |machine: &str| scope.map_or(true, |scope| scope == machine);
    let mut groups = HashMap::new();
    for saved in saved {
        let Some((machine, cwd)) = identity(saved) else { continue };
        if !accepts(&machine) { continue; }
        let target = group(&mut groups, &machine, &cwd);
        target.saved = true;
        target.pinned = saved.get("pinned").and_then(Value::as_bool).unwrap_or(false);
        target.avatar = saved.get("avatar").and_then(|avatar| crate::project_icons::validate_avatar(avatar).ok()).unwrap_or(Value::Null);
        if let Some(name) = saved.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()) { target.name = name.into(); }
        target.last_at = saved.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
    }
    for (machine, history) in history {
        if !accepts(machine) { continue; }
        let Some(projects) = history.as_array() else { continue };
        for project in projects {
            let Some(cwd) = project.get("cwd").and_then(Value::as_str).and_then(|cwd| clean_path(cwd).ok()) else { continue };
            let target = group(&mut groups, machine, &cwd);
            target.last_at = target.last_at.max(project.get("lastAt").and_then(Value::as_i64).unwrap_or(0));
            let provider = project.get("agent").and_then(Value::as_str).unwrap_or("all");
            let count = target.history_counts.entry(provider.into()).or_default();
            *count = (*count).max(project.get("count").and_then(Value::as_u64).unwrap_or(0) as usize);
            if let Some(sessions) = project.get("sessions").and_then(Value::as_array) {
                for session in sessions {
                    let Some(id) = session.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else { continue };
                    let mut session = session.clone();
                    session["live"] = json!(false);
                    target.sessions.insert(id.into(), session);
                }
            }
        }
    }
    for session in live {
        let machine = session.remote.as_deref().unwrap_or("local");
        if !accepts(machine) { continue; }
        let Some(cwd) = session.cwd.as_deref().and_then(|cwd| clean_path(cwd).ok()) else { continue };
        let target = group(&mut groups, machine, &cwd);
        target.last_at = target.last_at.max(session.updated_at);
        let entry = target.sessions.entry(session.id.clone()).or_insert_with(|| json!({"id":session.id}));
        let current = serde_json::to_value(session).unwrap_or_else(|_| json!({}));
        entry.as_object_mut().unwrap().extend(current.as_object().unwrap().clone());
        entry["live"] = json!(true);
        entry["lastAt"] = json!(session.updated_at);
        entry["attention"] = json!(matches!(session.status, Status::Waiting | Status::Limit));
    }
    let mut projects: Vec<_> = groups.into_values().map(|target| {
        let mut sessions: Vec<_> = target.sessions.into_values().collect();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.get("lastAt").or_else(|| session.get("at")).and_then(Value::as_i64).unwrap_or(0)));
        let live_count = sessions.iter().filter(|session| session["live"] == true).count();
        let attention_count = sessions.iter().filter(|session| session["attention"] == true).count();
        let agents: BTreeSet<_> = sessions.iter().filter_map(|session| session.get("agent").and_then(Value::as_str)).collect();
        json!({"id":format!("{}:{}",target.machine,target.cwd),"machine":target.machine,"cwd":target.cwd,
            "name":target.name,"project":target.name,"pinned":target.pinned,"saved":target.saved,"avatar":target.avatar,"lastAt":target.last_at,
            "count":target.history_counts.values().sum::<usize>().max(sessions.len()),"sessions":sessions,"liveCount":live_count,
            "attentionCount":attention_count,"agents":agents,"connection":connection(&target.machine,remotes)})
    }).collect();
    projects.sort_by(|a, b| b["pinned"].as_bool().cmp(&a["pinned"].as_bool())
        .then_with(|| b["lastAt"].as_i64().cmp(&a["lastAt"].as_i64()))
        .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        .then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
    projects
}

pub async fn list(d: Arc<Daemon>, machine: Option<String>) -> Value {
    let scope = match machine.filter(|machine| !machine.trim().is_empty()) {
        Some(machine) => match machine_name(&machine) { Ok(machine) => Some(machine), Err(error) => return json!({"ok":false,"error":error}) },
        None => None,
    };
    let settings = d.settings.load();
    let saved = settings.get("projects").and_then(Value::as_array).cloned().unwrap_or_default();
    let remotes = d.remotes.list();
    let mut history = Vec::new();
    let mut warnings = Vec::new();
    if settings.get("projects").is_some_and(|projects| !projects.is_array()) {
        warnings.push(json!({"machine":"local","error":"Не удалось прочитать сохранённые проекты: реестр повреждён"}));
    }
    if scope.as_deref().map_or(true, |scope| scope == "local") {
        let owner = d.clone();
        match tauri::async_runtime::spawn_blocking(move || owner.history.projects(&owner.usage)).await {
            Ok(projects) => history.push(("local".into(), projects)),
            Err(error) => warnings.push(json!({"machine":"local","error":error.to_string()})),
        }
    }
    let mut scans = tokio::task::JoinSet::new();
    for node in d.remotes.all() {
        if scope.as_deref().is_some_and(|scope| scope != node.cfg.name) { continue; }
        let status = node.status();
        if !status.connected {
            warnings.push(json!({"machine":status.name,"error":if status.error.is_empty(){"Нет подключения к машине".into()}else{status.error}}));
            continue;
        }
        scans.spawn(async move {
            let result = match node.client() { Ok(client) => client.projects().await, Err(error) => Err(error) };
            (node.cfg.name.clone(), result)
        });
    }
    while let Some(result) = scans.join_next().await {
        match result {
            Ok((machine, Ok(projects))) => history.push((machine.clone(), crate::ipc::remote_projects_to_history(&machine, projects))),
            Ok((machine, Err(error))) => warnings.push(json!({"machine":machine,"error":error})),
            Err(error) => warnings.push(json!({"machine":"remote","error":error.to_string()})),
        }
    }
    let projects = merge(&saved, &history, &d.snapshot(), &remotes, scope.as_deref());
    json!({"ok":true,"projects":projects,"warnings":warnings})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(tag: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("jarvis-project-registry-{tag}-{}-{sequence}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> Store { Store::with_path(self.0.join("settings.json")) }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn project_metadata_persists_without_creating_or_deleting_a_directory() {
        let fixture = Fixture::new("metadata");
        let store = fixture.store();
        let directory = fixture.0.join("future-repo");
        let saved = save(&store, json!({"cwd":directory,"name":"Мой проект","pinned":true})).unwrap();
        assert!(!directory.exists(), "saving metadata must not create the project directory");
        assert_eq!(saved["machine"], "local");
        assert_eq!(saved["name"], "Мой проект");
        let reopened = fixture.store();
        assert_eq!(reopened.load()["projects"][0]["pinned"], true);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("keep.txt"), "user file").unwrap();
        remove(&reopened, "local", directory.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(directory.join("keep.txt")).unwrap(), "user file");
        assert!(reopened.load()["projects"].as_array().unwrap().is_empty());
    }

    #[test]
    fn saving_one_field_keeps_other_metadata_and_unrelated_settings() {
        let fixture = Fixture::new("patch");
        let store = fixture.store();
        store.try_set_top("voice", json!({"speaker":"test"})).unwrap();
        save(&store, json!({"machine":"vps","cwd":"/repo/","name":"Custom","pinned":true})).unwrap();
        let saved = save(&store, json!({"machine":"vps","cwd":"/repo","pinned":false})).unwrap();
        assert_eq!(saved["name"], "Custom");
        assert_eq!(saved["pinned"], false);
        assert_eq!(store.load()["projects"].as_array().unwrap().len(), 1);
        assert_eq!(store.load()["voice"]["speaker"], "test");
    }

    #[test]
    fn avatar_persists_survives_pin_merges_and_resets_explicitly() {
        let fixture = Fixture::new("avatar"); let store = fixture.store();
        let avatar = json!({"dataUrl":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6L5sAAAAASUVORK5CYII=","source":"project","path":"public/favicon.svg","label":"Favicon"});
        save(&store, json!({"machine":"vps","cwd":"/repo","avatar":avatar})).unwrap();
        let pinned = save(&store, json!({"machine":"vps","cwd":"/repo","pinned":true})).unwrap();
        assert_eq!(pinned["avatar"], avatar);
        let restored = fixture.store().load();
        assert_eq!(restored["projects"][0]["avatar"], avatar);
        assert_eq!(merge(restored["projects"].as_array().unwrap(), &[], &[], &[], None)[0]["avatar"], avatar);
        let before = store.load();
        assert!(save(&store, json!({"machine":"vps","cwd":"/repo","avatar":{"dataUrl":"data:image/svg+xml;base64,PHN2Zy8+","source":"upload"}})).is_err());
        assert_eq!(store.load(), before, "rejected image must not change any persisted metadata");
        let reset = save(&store, json!({"machine":"vps","cwd":"/repo","avatar":null})).unwrap();
        assert!(reset["avatar"].is_null()); assert_eq!(reset["pinned"], true);
    }

    #[test]
    fn catalog_never_exposes_unsafe_hand_edited_avatar_metadata() {
        let saved = [json!({"machine":"vps","cwd":"/repo","avatar":{"dataUrl":"https://example.org/track.png","source":"project"}})];
        assert!(merge(&saved, &[], &[], &[], None)[0]["avatar"].is_null());
    }

    #[test]
    fn concurrent_saves_do_not_lose_other_project_entries() {
        let fixture = Fixture::new("parallel");
        let store = Arc::new(fixture.store());
        let threads: Vec<_> = (0..12).map(|i| {
            let store = store.clone();
            std::thread::spawn(move || save(&store, json!({"machine":"vps","cwd":format!("/repo/{i}")})).unwrap())
        }).collect();
        for thread in threads { thread.join().unwrap(); }
        assert_eq!(store.load()["projects"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn invalid_project_data_and_malformed_registry_are_not_overwritten() {
        let fixture = Fixture::new("invalid");
        let store = fixture.store();
        for project in [json!({"cwd":"relative"}), json!({"cwd":"/repo\n"}), json!({"cwd":"/repo","machine":false}), json!({"cwd":"/repo","pinned":"yes"})] {
            assert!(save(&store, project).is_err());
        }
        store.try_set_top("projects", json!({"custom":"preserve"})).unwrap();
        assert!(save(&store, json!({"cwd":"/repo","machine":"vps"})).is_err());
        assert!(remove(&store, "vps", "/repo").is_err());
        assert_eq!(store.load()["projects"], json!({"custom":"preserve"}));
    }

    #[test]
    fn failed_durable_write_never_reports_a_saved_project() {
        let fixture = Fixture::new("io-failure");
        std::fs::create_dir(fixture.0.join("settings.json")).unwrap();
        assert!(save(&fixture.store(), json!({"machine":"vps","cwd":"/repo"})).is_err());
    }

    #[test]
    fn local_alias_is_saved_under_actual_directory_without_touching_target_files() {
        let fixture = Fixture::new("alias");
        let real = fixture.0.join("real");
        std::fs::create_dir(&real).unwrap();
        let alias = fixture.0.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let saved = save(&fixture.store(), json!({"cwd":alias})).unwrap();
        assert_eq!(saved["cwd"], real.canonicalize().unwrap().to_str().unwrap());
        assert!(alias.is_symlink());
    }

    #[test]
    fn catalog_keeps_saved_empty_and_offline_projects_and_separates_machines() {
        let saved = [json!({"machine":"local","cwd":"/repo","name":"Local","pinned":true}),
            json!({"machine":"vps","cwd":"/repo","name":"Remote"})];
        let out = merge(&saved, &[], &[], &[], None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["name"], "Local");
        assert_eq!(out[0]["count"], 0);
        let remote = out.iter().find(|project| project["machine"] == "vps").unwrap();
        assert_eq!(remote["connection"]["online"], false);
        assert_eq!(remote["saved"], true);
        assert_ne!(out[0]["id"], remote["id"]);
    }

    #[test]
    fn live_session_enriches_history_without_duplicating_usage_or_losing_custom_name() {
        let saved = [json!({"machine":"local","cwd":"/repo","name":"Pinned work","pinned":true})];
        let history = [("local".into(), json!([{"cwd":"/repo/","project":"repo","count":1,"lastAt":10,
            "sessions":[{"id":"s1","agent":"claude","title":"Old title","cost":1.25,"tokens":400,"lastAt":10}]}]))];
        let mut session = Session::new("s1".into(), 5);
        session.cwd = Some("/repo".into()); session.agent = Some("claude".into());
        session.title = Some("New title".into()); session.status = Status::Waiting; session.updated_at = 20;
        let out = merge(&saved, &history, &[session], &[], Some("local"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], "Pinned work");
        assert_eq!(out[0]["count"], 1);
        assert_eq!(out[0]["liveCount"], 1);
        assert_eq!(out[0]["attentionCount"], 1);
        assert_eq!(out[0]["sessions"][0]["cost"], 1.25);
        assert_eq!(out[0]["sessions"][0]["title"], "New title");
        assert_eq!(out[0]["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(out[0]["lastAt"], 20);
    }

    #[test]
    fn remote_provider_counts_add_even_when_each_history_is_truncated() {
        let history = [("vps".into(), json!([
            {"cwd":"/repo","agent":"claude","count":100,"sessions":[{"id":"vps:c","agent":"claude"}]},
            {"cwd":"/repo","agent":"codex","count":80,"sessions":[{"id":"vps:g","agent":"codex"}]}
        ]))];
        let out = merge(&[], &history, &[], &[], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["count"], 180);
        assert_eq!(out[0]["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(out[0]["agents"], json!(["claude", "codex"]));
    }
}
