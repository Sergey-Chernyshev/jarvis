//! Portable, validated local analytics settings. Paths are data, never shell code.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const MAX_CONFIG_BYTES: usize = 256 * 1024;
static WRITER: Mutex<()> = Mutex::new(());
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub const TOOL_ALIAS_TARGETS: &[&str] = &[
    "Bash",
    "exec_command",
    "shell_command",
    "shell",
    "Edit",
    "MultiEdit",
    "Write",
    "apply_patch",
    "Read",
    "Glob",
    "Grep",
    "read_file",
    "search",
    "exec",
    "functions.exec",
    "wait",
    "functions.wait",
    "multi_tool_use.parallel",
    "write_stdin",
    "wait_agent",
    "wait_threads",
    "sleep",
    "get_handoff_status",
];

pub fn defaults() -> Value {
    json!({"version":1,"autoDiscover":true,"sources":[],
        "limits":{"maxFiles":200,"maxScanMiB":256,"maxFileMiB":32,"maxLines":200000,"maxProjects":20},
        "economics":{"currency":"USD","hourlyRate":null},
        "rules":{"idleCapMinutes":5,"contextWarningPct":85,"minModelSamples":5,
            "harnessWeights":{"toolReliability":1,"resultObservability":1,"verificationAfterEdit":1},"toolAliases":{}},
        "git":{"sourceExtensions":["rs","js","mjs","cjs","jsx","ts","tsx","py","go","c","h","cpp","hpp","cc",
            "java","kt","kts","swift","m","mm","cs","fs","rb","php","scala","sh","bash","zsh","sql","html",
            "css","scss","sass","vue","svelte","ex","exs","erl","hrl","clj","cljs","dart","lua","pl","r","R","jl","zig","nix","tf"],
            "excludeDirectories":["node_modules","vendor","target","dist","build",".git"],
            "excludeSuffixes":[".lock"],"excludeNameFragments":[".min.",".generated."],"minMatchChars":12}})
}

fn merge(base: &mut Value, input: &Value, path: &str) -> Result<(), String> {
    if path == "rules.toolAliases" {
        *base = input.clone();
        return Ok(());
    }
    if let Some(base) = base.as_object_mut() {
        let input = input
            .as_object()
            .ok_or_else(|| format!("{path}: требуется JSON-объект"))?;
        for (key, value) in input {
            let next = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let target = base
                .get_mut(key)
                .ok_or_else(|| format!("Неизвестная настройка: {next}"))?;
            merge(target, value, &next)?;
        }
    } else {
        *base = input.clone();
    }
    Ok(())
}

fn integer(value: &Value, path: &str, min: u64, max: u64) -> Result<(), String> {
    if value
        .pointer(path)
        .and_then(Value::as_u64)
        .is_some_and(|n| n >= min && n <= max)
    {
        Ok(())
    } else {
        Err(format!("{path}: требуется целое число от {min} до {max}"))
    }
}

fn decimal(value: &Value, path: &str, min: f64, max: f64) -> Result<(), String> {
    if value
        .pointer(path)
        .and_then(Value::as_f64)
        .is_some_and(|n| n.is_finite() && n >= min && n <= max)
    {
        Ok(())
    } else {
        Err(format!(
            "{path}: требуется конечное число от {min} до {max}"
        ))
    }
}

/// Only a leading ~ or ${HOME} is expanded. No arbitrary environment variables,
/// command substitution, globbing, or shell evaluation are performed.
pub fn resolve_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return Err("Путь должен содержать от 1 до 4096 байт без управляющих символов".into());
    }
    let resolved = if path == "~" || path == "${HOME}" {
        crate::util::home_dir()
    } else if let Some(relative) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("${HOME}/"))
    {
        crate::util::home_dir().join(relative.trim_start_matches('/'))
    } else {
        PathBuf::from(path)
    };
    if !resolved.is_absolute()
        || path.starts_with("~") && path != "~" && !path.starts_with("~/")
        || path.contains("${") && !path.starts_with("${HOME}")
    {
        return Err(
            "Используйте абсолютный путь, ~/ или ${HOME}/; другие переменные не раскрываются"
                .into(),
        );
    }
    // Preserve portable syntax in the configuration; resolve only for filesystem access.
    Ok(resolved)
}

fn string_list(value: &mut Value, path: &str, kind: &str) -> Result<(), String> {
    let values = value
        .pointer_mut(path)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("{path}: требуется массив строк"))?;
    if values.len() > 100 {
        return Err(format!("{path}: не больше 100 значений"));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for item in values.iter() {
        let raw = item
            .as_str()
            .ok_or_else(|| format!("{path}: ожидается строка"))?;
        let text = if kind == "extension" {
            raw.trim_start_matches('.')
        } else {
            raw
        };
        if text.is_empty()
            || text.len() > 128
            || text.chars().any(char::is_control)
            || text.contains('/')
            || text.contains('\\')
            || (kind == "directory" && matches!(text, "." | ".."))
            || (kind == "extension"
                && !text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')))
        {
            return Err(format!("{path}: недопустимое значение {raw:?}"));
        }
        if seen.insert(text.to_string()) {
            normalized.push(json!(text));
        }
    }
    *values = normalized;
    Ok(())
}

pub fn normalize(input: Value) -> Result<Value, String> {
    let mut value = defaults();
    merge(&mut value, &input, "")?;
    integer(&value, "/version", 1, 1)?;
    if !value["autoDiscover"].is_boolean() {
        return Err("autoDiscover: требуется true или false".into());
    }
    for (key, min, max) in [
        ("maxFiles", 1, 1000),
        ("maxScanMiB", 16, 1024),
        ("maxFileMiB", 1, 128),
        ("maxLines", 100, 1_000_000),
        ("maxProjects", 1, 100),
    ] {
        integer(&value, &format!("/limits/{key}"), min, max)?;
    }
    decimal(&value, "/rules/idleCapMinutes", 0.1, 60.0)?;
    decimal(&value, "/rules/contextWarningPct", 1.0, 100.0)?;
    integer(&value, "/rules/minModelSamples", 1, 1000)?;
    if !value["economics"]["currency"]
        .as_str()
        .is_some_and(|currency| {
            currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase())
        })
    {
        return Err("economics.currency: три заглавные латинские буквы валюты".into());
    }
    if !value["economics"]["hourlyRate"].is_null() {
        decimal(&value, "/economics/hourlyRate", 0.0, 100_000_000.0)?;
    }
    let mut weight_sum = 0.0;
    for key in [
        "toolReliability",
        "resultObservability",
        "verificationAfterEdit",
    ] {
        let path = format!("/rules/harnessWeights/{key}");
        decimal(&value, &path, 0.0, 10.0)?;
        weight_sum += value.pointer(&path).and_then(Value::as_f64).unwrap_or(0.0);
    }
    if weight_sum == 0.0 {
        return Err("Хотя бы один вес harness должен быть положительным".into());
    }
    let aliases = value["rules"]["toolAliases"]
        .as_object()
        .ok_or("rules.toolAliases: требуется объект")?;
    if aliases.len() > 128 {
        return Err("Не больше 128 псевдонимов инструментов".into());
    }
    for (alias, target) in aliases {
        if alias.is_empty()
            || alias.len() > 128
            || alias.chars().any(char::is_control)
            || !target
                .as_str()
                .is_some_and(|target| TOOL_ALIAS_TARGETS.contains(&target))
        {
            return Err(format!(
                "Недопустимый псевдоним {alias:?}. Цель должна быть одним из: {}",
                TOOL_ALIAS_TARGETS.join(", ")
            ));
        }
    }
    let sources = value["sources"]
        .as_array_mut()
        .ok_or("sources: требуется массив")?;
    if sources.len() > 32 {
        return Err("Не больше 32 настроенных источников".into());
    }
    let mut ids = HashSet::new();
    for source in sources {
        let object = source
            .as_object_mut()
            .ok_or("Источник должен быть объектом")?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "id" | "format" | "path" | "enabled"))
        {
            return Err("Неизвестное поле источника".into());
        }
        object.entry("enabled").or_insert(json!(true));
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or("У источника отсутствует id")?;
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            || !ids.insert(id.to_string())
        {
            return Err(
                "id источника должен быть уникальным: 1–64 символа a-z, A-Z, 0-9, _ или -".into(),
            );
        }
        if !object
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|f| matches!(f, "claude" | "codex" | "normalized"))
        {
            return Err("format источника: claude, codex или normalized".into());
        }
        if !object.get("enabled").is_some_and(Value::is_boolean) {
            return Err("enabled источника: true или false".into());
        }
        resolve_path(
            object
                .get("path")
                .and_then(Value::as_str)
                .ok_or("У источника отсутствует path")?,
        )?;
    }
    string_list(&mut value, "/git/sourceExtensions", "extension")?;
    string_list(&mut value, "/git/excludeDirectories", "directory")?;
    string_list(&mut value, "/git/excludeSuffixes", "suffix")?;
    string_list(&mut value, "/git/excludeNameFragments", "fragment")?;
    integer(&value, "/git/minMatchChars", 1, 1000)?;
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_CONFIG_BYTES {
        return Err("Конфигурация превышает 256 КиБ".into());
    }
    Ok(value)
}

pub fn load(dir: &Path) -> Result<Value, String> {
    let path = dir.join("analytics-config.json");
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(defaults()),
        Err(e) => return Err(format!("Не удалось прочитать настройки аналитики: {e}")),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Файл настроек аналитики не является обычным файлом".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err("Конфигурация превышает 256 КиБ".into());
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Повреждены настройки аналитики: {e}"))?;
    normalize(value)
}

pub fn save(dir: &Path, input: Value) -> Result<Value, String> {
    let value = normalize(input)?;
    let mut serialized = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    serialized.push(b'\n');
    if serialized.len() > MAX_CONFIG_BYTES {
        return Err("Конфигурация превышает 256 КиБ".into());
    }
    let _lock = WRITER.lock().map_err(|_| "Хранилище настроек недоступно")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!(
        ".analytics-config-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    // Clean up only a temporary file successfully created by this writer.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&serialized)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&temp, dir.join("analytics-config.json")).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_roundtrip_and_partial_config_normalizes() {
        assert_eq!(normalize(defaults()).unwrap(), defaults());
        let value = normalize(
            json!({"autoDiscover":false,"git":{"sourceExtensions":[".rs","rs"]},
            "sources":[{"id":"my-agent","format":"normalized","path":"~/traces"}]}),
        )
        .unwrap();
        assert_eq!(value["git"]["sourceExtensions"], json!(["rs"]));
        assert_eq!(value["sources"][0]["enabled"], true);
        assert_eq!(value["limits"]["maxFiles"], 200);
    }
    #[test]
    fn invalid_limits_weights_aliases_and_unknown_fields_are_rejected() {
        for input in [
            json!({"limits":{"maxFiles":0}}),
            json!({"limits":{"maxFileMiB":129}}),
            json!({"rules":{"idleCapMinutes":0}}),
            json!({"rules":{"harnessWeights":{"toolReliability":0,"resultObservability":0,"verificationAfterEdit":0}}}),
            json!({"rules":{"toolAliases":{"custom":"eval_shell"}}}),
            json!({"version":2}),
            json!({"unexpected":true}),
            json!({"economics":{"currency":"usd"}}),
            json!({"economics":{"hourlyRate":-1}}),
        ] {
            assert!(normalize(input).is_err());
        }
        assert!(normalize(json!({"git":{"sourceExtensions":[]}})).is_ok());
        assert!(normalize(json!({"economics":{"currency":"EUR","hourlyRate":0}})).is_ok());
        assert!(normalize(json!({"rules":{"toolAliases":{"terminal":"exec_command"}}})).is_ok());
    }
    #[test]
    fn paths_are_portable_and_never_evaluated() {
        assert_eq!(
            resolve_path("~/traces").unwrap(),
            crate::util::home_dir().join("traces")
        );
        assert_eq!(
            resolve_path("~//traces").unwrap(),
            crate::util::home_dir().join("traces")
        );
        assert_eq!(
            resolve_path("${HOME}/traces").unwrap(),
            crate::util::home_dir().join("traces")
        );
        assert!(resolve_path("$HOME/traces").is_err());
        assert!(resolve_path("${SECRET}/traces").is_err());
        assert!(resolve_path("relative/traces").is_err());
        assert!(resolve_path("~other/traces").is_err());
        assert_eq!(
            resolve_path("/tmp/$(touch impossible)").unwrap(),
            PathBuf::from("/tmp/$(touch impossible)")
        );
    }
    #[test]
    fn invalid_save_preserves_file_and_malformed_load_does_not_reset() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "jarvis-config-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let saved = save(&dir, json!({"autoDiscover":false})).unwrap();
        assert_eq!(load(&dir).unwrap(), saved);
        assert_eq!(
            std::fs::metadata(dir.join("analytics-config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(save(&dir, json!({"limits":{"maxFiles":1001}})).is_err());
        assert_eq!(load(&dir).unwrap(), saved);
        std::fs::write(dir.join("analytics-config.json"), b"broken").unwrap();
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
