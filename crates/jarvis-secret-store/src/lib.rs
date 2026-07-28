use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use zeroize::Zeroize;

pub const KEYCHAIN_SERVICE: &str = "app.jarvis.monitor.agent-vm";
pub const CLAUDE_CODE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
pub const MAX_SECRET_BYTES: usize = 256 * 1024;
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretKind {
    ClaudeApiKey,
    ClaudeOauthToken,
}

impl SecretKind {
    pub fn from_claude_mode(mode: &str) -> Option<Self> {
        match mode {
            "key" => Some(Self::ClaudeApiKey),
            "subscription" => Some(Self::ClaudeOauthToken),
            _ => None,
        }
    }

    fn account(self) -> &'static str {
        match self {
            Self::ClaudeApiKey => "claude-api-key",
            Self::ClaudeOauthToken => "claude-oauth-token",
        }
    }
}

pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(mut bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            bytes.zeroize();
            return Err("secret имеет недопустимый размер".into());
        }
        Ok(Self(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(&self.0);
        hex_prefix(&digest, 12)
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

pub trait SecretStore: Clone + Send + Sync + 'static {
    fn get(&self, kind: SecretKind) -> Result<Option<SecretValue>, String>;
    fn set(&self, kind: SecretKind, value: &SecretValue) -> Result<(), String>;
    fn delete(&self, kind: SecretKind) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
pub fn read_claude_code_credentials() -> Result<Option<SecretValue>, String> {
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    const ITEM_NOT_FOUND: i32 = -25_300;
    let results = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(CLAUDE_CODE_KEYCHAIN_SERVICE)
        .load_data(true)
        .limit(2_i64)
        .search();
    let results = match results {
        Ok(results) => results,
        Err(error) if error.code() == ITEM_NOT_FOUND => return Ok(None),
        Err(_) => return Err("Claude Code macOS Keychain read failed".into()),
    };
    let mut values = Vec::new();
    for result in results {
        match result {
            SearchResult::Data(bytes) => values.push(bytes),
            _ => {
                for bytes in &mut values {
                    bytes.zeroize();
                }
                return Err("Claude Code macOS Keychain returned unsafe item".into());
            }
        }
    }
    if values.len() != 1 {
        for bytes in &mut values {
            bytes.zeroize();
        }
        return Err("Claude Code macOS Keychain item is ambiguous".into());
    }
    SecretValue::new(values.pop().unwrap()).map(Some)
}

#[cfg(not(target_os = "macos"))]
pub fn read_claude_code_credentials() -> Result<Option<SecretValue>, String> {
    Ok(None)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacKeychainStore;

#[cfg(target_os = "macos")]
impl SecretStore for MacKeychainStore {
    fn get(&self, kind: SecretKind) -> Result<Option<SecretValue>, String> {
        const ITEM_NOT_FOUND: i32 = -25_300;
        match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, kind.account())
        {
            Ok(bytes) => SecretValue::new(bytes).map(Some),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(_) => Err("macOS Keychain read failed".into()),
        }
    }

    fn set(&self, kind: SecretKind, value: &SecretValue) -> Result<(), String> {
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            kind.account(),
            value.expose(),
        )
        .map_err(|_| "macOS Keychain write failed".to_string())
    }

    fn delete(&self, kind: SecretKind) -> Result<(), String> {
        const ITEM_NOT_FOUND: i32 = -25_300;
        match security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            kind.account(),
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
            Err(_) => Err("macOS Keychain delete failed".into()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for MacKeychainStore {
    fn get(&self, _kind: SecretKind) -> Result<Option<SecretValue>, String> {
        Err("system credential store unsupported".into())
    }

    fn set(&self, _kind: SecretKind, _value: &SecretValue) -> Result<(), String> {
        Err("system credential store unsupported".into())
    }

    fn delete(&self, _kind: SecretKind) -> Result<(), String> {
        Err("system credential store unsupported".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    pub migrated: bool,
    pub kind: Option<SecretKind>,
    pub fingerprint: Option<String>,
}

pub fn migrate_legacy_claude_secret<S: SecretStore>(
    settings_path: &Path,
    store: &S,
) -> Result<MigrationReport, String> {
    let metadata = match fs::symlink_metadata(settings_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MigrationReport {
                migrated: false,
                kind: None,
                fingerprint: None,
            });
        }
        Err(_) => return Err("не проверить private settings перед migration".into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_SETTINGS_BYTES {
        return Err("private settings имеют небезопасный тип или размер".into());
    }
    let mut bytes =
        fs::read(settings_path).map_err(|_| "не прочитать private settings".to_string())?;
    let parsed = serde_json::from_slice(&bytes)
        .map_err(|_| "private settings содержат некорректный JSON".to_string());
    bytes.zeroize();
    let mut settings: serde_json::Value = parsed?;
    let Some(service) = settings
        .as_object_mut()
        .and_then(|root| root.get_mut("service"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(MigrationReport {
            migrated: false,
            kind: None,
            fingerprint: None,
        });
    };
    let kind = service
        .get("claudeAuthMode")
        .and_then(serde_json::Value::as_str)
        .and_then(SecretKind::from_claude_mode);
    let Some(kind) = kind else {
        return Ok(MigrationReport {
            migrated: false,
            kind: None,
            fingerprint: None,
        });
    };
    let legacy = service
        .remove("claudeSecret")
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty());
    let Some(mut legacy) = legacy else {
        let stored = store.get(kind)?;
        return Ok(MigrationReport {
            migrated: false,
            kind: stored.as_ref().map(|_| kind),
            fingerprint: stored.as_ref().map(SecretValue::fingerprint),
        });
    };

    let value = SecretValue::new(legacy.as_bytes().to_vec());
    legacy.zeroize();
    let value = value?;
    store.set(kind, &value)?;
    let verified = store
        .get(kind)?
        .is_some_and(|stored| constant_time_eq(stored.expose(), value.expose()));
    if !verified {
        return Err("Keychain verification failed; plaintext preserved".into());
    }

    atomic_write_json(settings_path, &settings)?;
    Ok(MigrationReport {
        migrated: true,
        kind: Some(kind),
        fingerprint: Some(value.fingerprint()),
    })
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(chars);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        if out.len() == chars {
            break;
        }
        out.push(HEX[(byte & 0x0f) as usize] as char);
        if out.len() == chars {
            break;
        }
    }
    out
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let max = left.len().max(right.len());
    for index in 0..max {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "private settings path не содержит parent".to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let temp = parent.join(format!(
        ".{name}.secret-migration-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "не сериализовать migrated private settings".to_string())?;
    bytes.push(b'\n');
    let result = (|| -> Result<(), String> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc_no_follow())
            .open(&temp)
            .map_err(|_| "не создать private migration file".to_string())?;
        output
            .write_all(&bytes)
            .map_err(|_| "не записать private migration file".to_string())?;
        output
            .sync_all()
            .map_err(|_| "не сохранить private migration file".to_string())?;
        drop(output);
        fs::rename(&temp, path).map_err(|_| "не завершить atomic private migration".to_string())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "не защитить migrated private settings".to_string())?;
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    bytes.zeroize();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
const fn libc_no_follow() -> i32 {
    libc::O_NOFOLLOW
}

#[cfg(not(unix))]
const fn libc_no_follow() -> i32 {
    0
}

#[derive(Clone, Default)]
pub struct MemorySecretStore {
    values: Arc<Mutex<BTreeMap<SecretKind, Vec<u8>>>>,
    fail_verification: Arc<Mutex<bool>>,
}

impl MemorySecretStore {
    pub fn set_fail_verification(&self, fail: bool) {
        *self.fail_verification.lock().unwrap() = fail;
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, kind: SecretKind) -> Result<Option<SecretValue>, String> {
        if *self.fail_verification.lock().unwrap() {
            return Ok(None);
        }
        self.values
            .lock()
            .unwrap()
            .get(&kind)
            .cloned()
            .map(SecretValue::new)
            .transpose()
    }

    fn set(&self, kind: SecretKind, value: &SecretValue) -> Result<(), String> {
        self.values
            .lock()
            .unwrap()
            .insert(kind, value.expose().to_vec());
        Ok(())
    }

    fn delete(&self, kind: SecretKind) -> Result<(), String> {
        self.values.lock().unwrap().remove(&kind);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "jarvis-secret-store-{tag}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        (root.clone(), root.join("settings.json"))
    }

    #[test]
    fn secret_debug_never_contains_secret_bytes() {
        let value = SecretValue::new(b"SYNTHETIC_PRIVATE_VALUE".to_vec()).unwrap();

        let debug = format!("{value:?}");

        assert_eq!(debug, "SecretValue([REDACTED])");
        assert!(!debug.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert_eq!(value.fingerprint().len(), 12);
    }

    #[test]
    fn verified_migration_moves_legacy_value_and_scrubs_settings_atomically() {
        let (root, path) = fixture("migrate");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "service": {
                    "backend": "claude",
                    "claudeAuthMode": "subscription",
                    "claudeSecret": "SYNTHETIC_PRIVATE_VALUE",
                    "proxy": "http://synthetic.invalid"
                },
                "unrelated": true
            }))
            .unwrap(),
        )
        .unwrap();
        let store = MemorySecretStore::default();

        let report = migrate_legacy_claude_secret(&path, &store).unwrap();

        assert!(report.migrated);
        assert_eq!(report.kind, Some(SecretKind::ClaudeOauthToken));
        assert_eq!(report.fingerprint.as_deref().map(str::len), Some(12));
        assert_eq!(
            store
                .get(SecretKind::ClaudeOauthToken)
                .unwrap()
                .unwrap()
                .expose(),
            b"SYNTHETIC_PRIVATE_VALUE"
        );
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes
            .windows(b"SYNTHETIC_PRIVATE_VALUE".len())
            .any(|part| part == b"SYNTHETIC_PRIVATE_VALUE"));
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved.pointer("/service/backend"), Some(&json!("claude")));
        assert_eq!(
            saved.pointer("/service/proxy"),
            Some(&json!("http://synthetic.invalid"))
        );
        assert!(saved.pointer("/service/claudeSecret").is_none());
        assert_eq!(saved.get("unrelated"), Some(&json!(true)));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_keychain_verification_preserves_plaintext_source() {
        let (root, path) = fixture("verify-failure");
        fs::write(
            &path,
            br#"{"service":{"claudeAuthMode":"key","claudeSecret":"SYNTHETIC_PRIVATE_VALUE"}}"#,
        )
        .unwrap();
        let store = MemorySecretStore::default();
        store.set_fail_verification(true);

        let error = migrate_legacy_claude_secret(&path, &store).unwrap_err();

        assert!(!error.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("SYNTHETIC_PRIVATE_VALUE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_legacy_value_is_idempotent_and_reports_existing_keychain_kind() {
        let (root, path) = fixture("idempotent");
        fs::write(&path, br#"{"service":{"claudeAuthMode":"subscription"}}"#).unwrap();
        let store = MemorySecretStore::default();
        let value = SecretValue::new(b"SYNTHETIC_PRIVATE_VALUE".to_vec()).unwrap();
        store.set(SecretKind::ClaudeOauthToken, &value).unwrap();

        let report = migrate_legacy_claude_secret(&path, &store).unwrap();

        assert!(!report.migrated);
        assert_eq!(report.kind, Some(SecretKind::ClaudeOauthToken));
        assert_eq!(report.fingerprint.as_deref().map(str::len), Some(12));
        fs::remove_dir_all(root).unwrap();
    }
}
