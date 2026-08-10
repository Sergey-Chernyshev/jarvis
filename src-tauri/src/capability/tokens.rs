//! Токены потребителей сокета (R2). Идентичность входящего-по-сокету — по
//! токену из ~/.jarvis/tokens.json (права 0600), а НЕ по строке в теле запроса.
//! Панель (in-process) токена не требует и здесь не резолвится: Consumer::panel()
//! не выдаётся ни по какому токену (INV-PANEL).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde_json::{json, Value};

use super::contract::RiskClass;
use super::grant::Consumer;
use crate::util::jarvis_dir;

/// Доступ к таблице токенов. Файл читается на каждый резолв (вызовы редки).
pub struct TokenStore {
    path: PathBuf,
    state: Arc<TokenState>,
    entropy: EntropySource,
}

type EntropySource = fn(&mut [u8]) -> Result<(), String>;

struct TokenState {
    mutation_lock: Mutex<()>,
    denied_plugins: Mutex<HashSet<String>>,
}

struct TokenFileLock {
    _file: File,
}

impl Drop for TokenFileLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl TokenStore {
    pub fn new() -> Self {
        Self::for_path(jarvis_dir().join("tokens.json"))
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self::for_path_with(path, system_entropy, false)
    }

    #[cfg(test)]
    fn at_with_entropy(path: PathBuf, entropy: EntropySource) -> Self {
        Self::for_path_with(path, entropy, false)
    }

    #[cfg(test)]
    fn at_fresh_process(path: PathBuf) -> Self {
        Self::for_path_with(path, system_entropy, true)
    }

    fn for_path(path: PathBuf) -> Self {
        Self::for_path_with(path, system_entropy, true)
    }

    fn for_path_with(path: PathBuf, entropy: EntropySource, deny_persisted_plugins: bool) -> Self {
        let state = token_state_for(&path, deny_persisted_plugins);
        Self {
            path,
            state,
            entropy,
        }
    }

    fn acquire_file_lock(&self) -> Result<TokenFileLock, String> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("token lock parent cannot be created: {error}"))?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|error| format!("token lock parent cannot be inspected: {error}"))?;
        if !parent_metadata.file_type().is_dir()
            || parent_metadata.uid() != unsafe { libc::geteuid() }
            || parent_metadata.mode() & 0o022 != 0
        {
            return Err(format!(
                "token lock parent {} must be an owned non-writable-by-others directory",
                parent.display()
            ));
        }

        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let lock_path = parent.join(format!(".{file_name}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "token lockfile {} cannot be opened safely: {error}",
                    lock_path.display()
                )
            })?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("token lockfile cannot be inspected: {error}"))?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("token lockfile must be an owned single-link regular file".into());
        }
        if metadata.mode() & 0o077 != 0 {
            return Err("token lockfile must be private (0600 or stricter)".into());
        }

        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("token lockfile cannot be locked: {error}"));
            }
        }

        let path_metadata = fs::symlink_metadata(&lock_path)
            .map_err(|error| format!("token lockfile path cannot be re-inspected: {error}"))?;
        if path_metadata.dev() != metadata.dev() || path_metadata.ino() != metadata.ino() {
            return Err("token lockfile path changed while acquiring the lock".into());
        }
        Ok(TokenFileLock { _file: file })
    }

    fn read(&self) -> Value {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn read_strict(&self) -> Result<Option<Value>, String> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "не прочитать хранилище токенов {}: {error}",
                    self.path.display()
                ))
            }
        };
        let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
            format!(
                "повреждено хранилище токенов {}: {error}",
                self.path.display()
            )
        })?;
        if !value.is_object() {
            return Err(format!(
                "хранилище токенов {} не является JSON object",
                self.path.display()
            ));
        }
        Ok(Some(value))
    }

    fn write(&self, v: &Value) -> Result<(), String> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|err| format!("не создать каталог токенов: {err}"))?;
        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let temp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_string_pretty(v)
            .map_err(|err| format!("не сериализовать токены: {err}"))?
            + "\n";

        let result = (|| -> Result<(), String> {
            let mut temp = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)
                .map_err(|err| format!("не создать временный файл токенов: {err}"))?;
            temp.write_all(bytes.as_bytes())
                .map_err(|err| format!("не записать токены: {err}"))?;
            temp.sync_all()
                .map_err(|err| format!("не синхронизировать токены: {err}"))?;
            drop(temp);
            fs::rename(&temp_path, &self.path)
                .map_err(|err| format!("не заменить файл токенов: {err}"))?;
            let directory = File::open(parent)
                .map_err(|err| format!("не открыть каталог токенов для синхронизации: {err}"))?;
            directory
                .sync_all()
                .map_err(|err| format!("не синхронизировать каталог токенов: {err}"))?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// Сгенерировать/прочитать токен агента (идемпотентно).
    pub fn ensure_agent_token(&self) -> String {
        let _guard = self
            .state
            .mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _file_guard = match self.acquire_file_lock() {
            Ok(guard) => guard,
            Err(error) => {
                crate::log::line(&format!("[tokens] agent token lock failed: {error}"));
                return self
                    .read()
                    .get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
            }
        };
        let mut v = self.read();
        if let Some(t) = v.get("agent").and_then(|t| t.as_str()) {
            return t.to_string();
        }
        let tok = match gen_token(self.entropy) {
            Ok(token) => token,
            Err(error) => {
                crate::log::line(&format!("[tokens] agent token entropy failed: {error}"));
                return String::new();
            }
        };
        v.as_object_mut()
            .unwrap()
            .insert("agent".into(), json!(tok));
        if let Err(err) = self.write(&v) {
            crate::log::line(&format!("[tokens] agent token persist failed: {err}"));
        }
        tok
    }

    /// Выпустить или обновить токен внешнего плагина. Identity стабильна между
    /// рестартами, классы всегда заменяются текущим least-privilege manifest.
    pub fn ensure_plugin_token(&self, id: &str, classes: &[RiskClass]) -> Result<String, String> {
        self.issue_plugin_token(id, classes, false)
    }

    /// Выпустить новый process-bound bearer перед каждым spawn. Предыдущая
    /// identity сначала блокируется в памяти, поэтому ошибка entropy/storage не
    /// может вернуть старый bearer обратно в admission.
    pub fn rotate_plugin_token(&self, id: &str, classes: &[RiskClass]) -> Result<String, String> {
        self.issue_plugin_token(id, classes, true)
    }

    fn issue_plugin_token(
        &self,
        id: &str,
        classes: &[RiskClass],
        force_rotate: bool,
    ) -> Result<String, String> {
        if id.is_empty() {
            return Err("plugin id обязателен".into());
        }
        let _guard = self
            .state
            .mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if force_rotate {
            self.state
                .denied_plugins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(id.to_owned());
        }
        let _file_guard = self.acquire_file_lock()?;
        let mut v = self.read_strict()?.unwrap_or_else(|| json!({}));
        let denied = self
            .state
            .denied_plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(id);
        let existing = v
            .get("plugins")
            .and_then(Value::as_object)
            .and_then(|plugins| plugins.get(id))
            .and_then(|entry| entry.get("token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .filter(|_| !force_rotate)
            .filter(|_| !denied)
            .map(str::to_string);
        let token = match existing {
            Some(token) => token,
            None => gen_token(self.entropy)?,
        };

        let mut class_names = Vec::new();
        for class in classes {
            let allowed = match class {
                RiskClass::Read => Some("read"),
                RiskClass::Control => Some("control"),
                RiskClass::Settings => Some("settings"),
                RiskClass::Admin => None,
            };
            if let Some(name) = allowed {
                if !class_names.contains(&name) {
                    class_names.push(name);
                }
            }
        }

        let root = v.as_object_mut().unwrap();
        let plugins = root.entry("plugins").or_insert_with(|| json!({}));
        if !plugins.is_object() {
            *plugins = json!({});
        }
        plugins.as_object_mut().unwrap().insert(
            id.to_string(),
            json!({ "token": token, "classes": class_names }),
        );
        self.write(&v)?;
        self.state
            .denied_plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        Ok(token)
    }

    /// Отозвать plugin identity. Повторный revoke безопасен и не трогает agent.
    pub fn revoke_plugin(&self, id: &str) -> Result<bool, String> {
        let _guard = self
            .state
            .mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.state
            .denied_plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_owned());
        let _file_guard = self.acquire_file_lock()?;
        let Some(mut v) = self.read_strict()? else {
            return Ok(false);
        };
        let Some(plugins) = v.get_mut("plugins").and_then(Value::as_object_mut) else {
            if v.get("plugins").is_some() {
                return Err("поле plugins в хранилище токенов имеет неверный тип".into());
            }
            return Ok(false);
        };
        let removed = plugins.remove(id).is_some();
        if removed {
            self.write(&v)?;
        }
        Ok(removed)
    }

    /// Strict lifecycle observation. Missing storage/entry means no token, while
    /// unreadable or malformed storage is an error: callers must not purge or
    /// mutate receipts based on an unverified "empty" observation.
    pub fn plugin_token_present(&self, id: &str) -> Result<bool, String> {
        let Some(value) = self.read_strict()? else {
            return Ok(false);
        };
        let Some(plugins) = value.get("plugins") else {
            return Ok(false);
        };
        let plugins = plugins
            .as_object()
            .ok_or_else(|| "поле plugins в хранилище токенов имеет неверный тип".to_owned())?;
        let Some(entry) = plugins.get(id) else {
            return Ok(false);
        };
        let token = entry
            .as_object()
            .and_then(|entry| entry.get("token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| format!("токен плагина {id} имеет неверный формат"))?;
        Ok(!token.is_empty())
    }

    /// Резолв токена в потребителя. Неизвестный/пустой → None. panel НИКОГДА.
    pub fn resolve(&self, token: &str) -> Option<Consumer> {
        if token.is_empty() {
            return None;
        }
        let denied_before = self
            .state
            .denied_plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let v = self.read();
        if v.get("agent").and_then(|t| t.as_str()) == Some(token) {
            return Some(Consumer::agent());
        }
        // плагины: { "plugins": { "<id>": { "token": "...", "classes": ["read",...] } } }
        let plugins = v.get("plugins").and_then(|p| p.as_object())?;
        for (id, entry) in plugins {
            if entry.get("token").and_then(|t| t.as_str()) == Some(token) {
                if denied_before.contains(id)
                    || self
                        .state
                        .denied_plugins
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .contains(id)
                {
                    return None;
                }
                let classes = parse_classes(entry.get("classes"));
                return Some(Consumer::plugin(id, &classes));
            }
        }
        None
    }
}

fn token_state_for(path: &Path, deny_persisted_plugins: bool) -> Arc<TokenState> {
    static STATES: OnceLock<Mutex<HashMap<PathBuf, Weak<TokenState>>>> = OnceLock::new();

    let states = STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = states.get(path).and_then(Weak::upgrade) {
        return existing;
    }

    states.retain(|_, state| state.strong_count() > 0);
    let state = Arc::new(TokenState {
        mutation_lock: Mutex::new(()),
        denied_plugins: Mutex::new(if deny_persisted_plugins {
            persisted_plugin_ids(path)
        } else {
            HashSet::new()
        }),
    });
    states.insert(path.to_path_buf(), Arc::downgrade(&state));
    state
}

fn persisted_plugin_ids(path: &Path) -> HashSet<String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("plugins").and_then(Value::as_object).cloned())
        .map(|plugins| plugins.into_iter().map(|(id, _)| id).collect())
        .unwrap_or_default()
}

fn parse_classes(v: Option<&Value>) -> Vec<RiskClass> {
    let mut out = Vec::new();
    if let Some(arr) = v.and_then(|v| v.as_array()) {
        for c in arr {
            match c.as_str() {
                Some("read") => out.push(RiskClass::Read),
                Some("control") => out.push(RiskClass::Control),
                Some("settings") => out.push(RiskClass::Settings),
                _ => {} // admin и мусор игнорируем — least-privilege
            }
        }
    }
    out
}

fn system_entropy(bytes: &mut [u8]) -> Result<(), String> {
    getrandom::getrandom(bytes).map_err(|error| format!("system entropy unavailable: {error}"))
}

/// 256-bit CSPRNG bearer encoded as 64 lowercase hex characters.
fn gen_token(entropy: EntropySource) -> Result<String, String> {
    let mut buf = [0u8; 32];
    entropy(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("jarvis-tok-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn agent_token_is_stable_and_resolves() {
        let s = TokenStore::at(tmp());
        let t1 = s.ensure_agent_token();
        let t2 = s.ensure_agent_token();
        assert_eq!(t1, t2, "токен идемпотентен");
        assert_eq!(t1.len(), 64, "32 байта hex");
        let c = s.resolve(&t1).expect("агентский токен резолвится");
        assert_eq!(c.id, "agent");
    }

    #[test]
    fn unknown_and_empty_token_rejected() {
        let s = TokenStore::at(tmp());
        s.ensure_agent_token();
        assert!(s.resolve("deadbeef").is_none());
        assert!(s.resolve("").is_none());
    }

    #[test]
    fn no_token_yields_panel_consumer() {
        // INV-PANEL: ни один токен не даёт грант панели.
        let s = TokenStore::at(tmp());
        let agent = s.ensure_agent_token();
        assert_ne!(s.resolve(&agent).unwrap().id, "panel");
    }

    #[test]
    fn plugin_token_resolves_least_privilege() {
        let p = tmp();
        std::fs::write(
            &p,
            r#"{"agent":"aaaa","plugins":{"weather":{"token":"bbbb","classes":["read"]}}}"#,
        )
        .unwrap();
        let s = TokenStore::at(p);
        let c = s.resolve("bbbb").expect("плагин резолвится");
        assert_eq!(c.id, "plugin:weather");
        assert!(c.grant.allows(RiskClass::Read));
        assert!(
            !c.grant.allows(RiskClass::Control),
            "least-privilege: только read"
        );
    }

    #[test]
    fn plugin_token_is_stable_updates_classes_and_uses_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let p = tmp();
        let s = TokenStore::at(p.clone());
        let t1 = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();
        let t2 = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Control])
            .unwrap();

        assert_eq!(t1, t2, "повторный выпуск сохраняет identity");
        let c = s.resolve(&t2).unwrap();
        assert!(c.grant.allows(RiskClass::Control));
        assert_eq!(
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn plugin_token_generation_fails_closed_when_system_entropy_is_unavailable() {
        fn unavailable_entropy(_bytes: &mut [u8]) -> Result<(), String> {
            Err("fixture entropy unavailable".into())
        }

        let path = tmp();
        let store = TokenStore::at_with_entropy(path.clone(), unavailable_entropy);

        let error = store
            .rotate_plugin_token("agent-vm", &[RiskClass::Read])
            .expect_err("activation must abort without cryptographic entropy");

        assert!(error.contains("fixture entropy unavailable"));
        assert!(
            !path.exists(),
            "entropy failure must not persist a predictable bearer"
        );
    }

    #[test]
    fn fresh_host_process_denies_persisted_plugin_bearers_before_respawn() {
        let path = tmp();
        let stale = {
            let previous_process = TokenStore::at(path.clone());
            previous_process
                .ensure_plugin_token("agent-vm", &[RiskClass::Read])
                .unwrap()
        };

        let restarted_process = TokenStore::at_fresh_process(path);

        assert!(
            restarted_process.resolve(&stale).is_none(),
            "a bearer persisted by a crashed host must be denied at startup"
        );
        let fresh = restarted_process
            .rotate_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();
        assert_ne!(fresh, stale);
        assert!(restarted_process.resolve(&fresh).is_some());
    }

    #[test]
    fn revoke_plugin_invalidates_token_without_touching_agent() {
        let s = TokenStore::at(tmp());
        let agent = s.ensure_agent_token();
        let plugin = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();

        assert!(s.revoke_plugin("agent-vm").unwrap());
        assert!(s.resolve(&plugin).is_none());
        assert_eq!(s.resolve(&agent).unwrap().id, "agent");
        assert!(!s.revoke_plugin("agent-vm").unwrap());
    }

    #[test]
    fn strict_plugin_presence_distinguishes_absent_present_and_corrupt_storage() {
        let path = tmp();
        let s = TokenStore::at(path.clone());

        assert!(!s.plugin_token_present("agent-vm").unwrap());
        s.ensure_plugin_token("agent-vm", &[RiskClass::Read])
            .unwrap();
        assert!(s.plugin_token_present("agent-vm").unwrap());
        s.revoke_plugin("agent-vm").unwrap();
        assert!(!s.plugin_token_present("agent-vm").unwrap());

        std::fs::write(path, b"{not-json").unwrap();
        assert!(
            s.plugin_token_present("agent-vm").is_err(),
            "lifecycle must fail closed instead of treating corrupt token state as empty"
        );
    }

    #[test]
    fn plugin_token_never_persists_admin_class() {
        let s = TokenStore::at(tmp());
        let token = s
            .ensure_plugin_token("agent-vm", &[RiskClass::Read, RiskClass::Admin])
            .unwrap();

        let c = s.resolve(&token).unwrap();
        assert!(!c.grant.allows(RiskClass::Admin));
    }

    #[test]
    fn concurrent_plugin_token_mutations_preserve_every_identity() {
        const PLUGINS: usize = 32;

        let path = tmp();
        let store = TokenStore::at(path.clone());
        let barrier = Arc::new(Barrier::new(PLUGINS));
        let handles = (0..PLUGINS)
            .map(|index| {
                let store = TokenStore::at(path.clone());
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let id = format!("dev.example.concurrent-{index}");
                    let token = store.ensure_plugin_token(&id, &[RiskClass::Read]).unwrap();
                    (id, token)
                })
            })
            .collect::<Vec<_>>();
        let issued = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        for (id, token) in issued {
            let consumer = store
                .resolve(&token)
                .unwrap_or_else(|| panic!("concurrent mutation lost token for {id}"));
            assert_eq!(consumer.id, format!("plugin:{id}"));
        }
    }

    #[test]
    fn process_plugin_token_mutation_helper() {
        let Some(path) = std::env::var_os("JARVIS_TOKEN_PROCESS_TEST_PATH") else {
            return;
        };
        let id = std::env::var("JARVIS_TOKEN_PROCESS_TEST_ID").unwrap();
        let ready = PathBuf::from(
            std::env::var_os("JARVIS_TOKEN_PROCESS_TEST_READY").expect("ready marker path"),
        );
        let go = PathBuf::from(
            std::env::var_os("JARVIS_TOKEN_PROCESS_TEST_GO").expect("go marker path"),
        );
        std::fs::write(&ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !go.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent did not release process barrier"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        TokenStore::at(PathBuf::from(path))
            .ensure_plugin_token(&id, &[RiskClass::Read])
            .unwrap();
    }

    #[test]
    fn concurrent_process_mutations_preserve_every_identity() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        const PROCESSES: usize = 12;

        let directory = std::env::temp_dir().join(format!(
            "jarvis-token-process-lock-{}-{}",
            std::process::id(),
            NEXT_PROCESS_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("tokens.json");
        let go = directory.join("go");
        let executable = std::env::current_exe().unwrap();
        let mut children = (0..PROCESSES)
            .map(|index| {
                let id = format!("dev.example.process-{index}");
                let ready = directory.join(format!("ready-{index}"));
                let child = Command::new(&executable)
                    .arg("--exact")
                    .arg("capability::tokens::tests::process_plugin_token_mutation_helper")
                    .arg("--nocapture")
                    .env("JARVIS_TOKEN_PROCESS_TEST_PATH", &path)
                    .env("JARVIS_TOKEN_PROCESS_TEST_ID", &id)
                    .env("JARVIS_TOKEN_PROCESS_TEST_READY", &ready)
                    .env("JARVIS_TOKEN_PROCESS_TEST_GO", &go)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                (id, ready, child)
            })
            .collect::<Vec<_>>();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while children.iter().any(|(_, ready, _)| !ready.exists()) {
            assert!(
                std::time::Instant::now() < deadline,
                "child processes did not reach the barrier"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::fs::write(&go, b"go").unwrap();

        for (id, _, child) in children.drain(..) {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "token child {id} failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let store = TokenStore::at(path);
        for index in 0..PROCESSES {
            let id = format!("dev.example.process-{index}");
            assert!(
                store
                    .read()
                    .pointer(&format!("/plugins/{id}/token"))
                    .and_then(Value::as_str)
                    .is_some(),
                "cross-process mutation lost {id}"
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unsafe_token_lockfile_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = std::env::temp_dir().join(format!(
            "jarvis-token-lock-symlink-{}-{}",
            std::process::id(),
            NEXT_PROCESS_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("tokens.json");
        let victim = directory.join("victim");
        std::fs::write(&victim, b"must-not-change").unwrap();
        symlink(&victim, directory.join(".tokens.json.lock")).unwrap();

        let error = TokenStore::at(path.clone())
            .ensure_plugin_token("dev.example.symlink", &[RiskClass::Read])
            .expect_err("symlink lockfile must fail closed");

        assert!(error.contains("lock"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"must-not-change");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn overpermissive_token_lockfile_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "jarvis-token-lock-mode-{}-{}",
            std::process::id(),
            NEXT_PROCESS_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("tokens.json");
        let lock = directory.join(".tokens.json.lock");
        std::fs::write(&lock, b"").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = TokenStore::at(path.clone())
            .ensure_plugin_token("dev.example.mode", &[RiskClass::Read])
            .expect_err("shared lockfile must fail closed");

        assert!(error.contains("private"));
        assert!(!path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_durable_revoke_still_denies_the_old_bearer_in_memory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "jarvis-token-revoke-failure-{}-{}",
            std::process::id(),
            NEXT_PROCESS_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("tokens.json");
        let store = TokenStore::at(path);
        let old = store
            .ensure_plugin_token("dev.example.denied", &[RiskClass::Read])
            .unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();

        let error = store
            .revoke_plugin("dev.example.denied")
            .expect_err("readable store in a non-writable directory cannot commit revoke");
        assert!(error.contains("временный файл"));
        assert!(
            store.resolve(&old).is_none(),
            "failed disk mutation must still fail closed in this process"
        );

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fresh = store
            .ensure_plugin_token("dev.example.denied", &[RiskClass::Read])
            .unwrap();
        assert_ne!(
            fresh, old,
            "successful re-admission rotates the denied bearer"
        );
        assert!(store.resolve(&fresh).is_some());
        std::fs::remove_dir_all(directory).unwrap();
    }

    static NEXT_PROCESS_TEST: AtomicU64 = AtomicU64::new(0);
}
