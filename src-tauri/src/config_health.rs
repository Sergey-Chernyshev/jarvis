//! Проверка сырого `settings.json` до того, как Store домержит дефолты.

use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Healthy,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepairMode {
    Preserve,
    ResetDefault,
    RecreateFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigIssue {
    pub path: String,
    pub code: String,
    pub severity: IssueSeverity,
    pub message: String,
    pub repair: RepairMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigHealth {
    pub status: HealthStatus,
    pub path: String,
    pub issues: Vec<ConfigIssue>,
    pub repairable: bool,
    pub restart_required: bool,
}

impl ConfigHealth {
    pub fn healthy(path: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Healthy,
            path: path.into(),
            issues: Vec::new(),
            repairable: false,
            restart_required: false,
        }
    }

    pub fn has_errors(&self) -> bool {
        self.status == HealthStatus::Error
    }
}

fn issue(issues: &mut Vec<ConfigIssue>, path: &str, code: &str, message: &str, repair: RepairMode) {
    if issues
        .iter()
        .any(|existing| existing.path == path && existing.code == code)
    {
        return;
    }
    issues.push(ConfigIssue {
        path: path.into(),
        code: code.into(),
        severity: IssueSeverity::Error,
        message: message.into(),
        repair,
    });
}

fn same_json_kind(current: &Value, expected: &Value) -> bool {
    matches!(
        (current, expected),
        (Value::Null, Value::Null)
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_))
            | (Value::Array(_), Value::Array(_))
            | (Value::Object(_), Value::Object(_))
    )
}

fn validate_default_shape(
    current: &Map<String, Value>,
    expected: &Map<String, Value>,
    prefix: &str,
    issues: &mut Vec<ConfigIssue>,
) {
    for (key, default) in expected {
        let Some(value) = current.get(key) else {
            continue;
        };
        let path = format!("{prefix}.{key}");
        if !same_json_kind(value, default) {
            issue(
                issues,
                &path,
                "wrong_type",
                "Неверный тип значения; Jarvis использует безопасное значение по умолчанию.",
                RepairMode::ResetDefault,
            );
            continue;
        }
        if let (Some(value), Some(default)) = (value.as_object(), default.as_object()) {
            validate_default_shape(value, default, &path, issues);
        }
    }
}

fn object_at<'a>(
    root: &'a Map<String, Value>,
    key: &str,
    issues: &mut Vec<ConfigIssue>,
) -> Option<&'a Map<String, Value>> {
    match root.get(key) {
        None => None,
        Some(Value::Object(object)) => Some(object),
        Some(_) => {
            issue(
                issues,
                &format!("$.{key}"),
                "wrong_type",
                "Раздел конфигурации должен быть JSON-объектом.",
                RepairMode::ResetDefault,
            );
            None
        }
    }
}

fn check_type(
    object: &Map<String, Value>,
    prefix: &str,
    key: &str,
    predicate: impl Fn(&Value) -> bool,
    issues: &mut Vec<ConfigIssue>,
) {
    if object.get(key).is_some_and(|value| !predicate(value)) {
        issue(
            issues,
            &format!("{prefix}.{key}"),
            "wrong_type",
            "Неверный тип значения; Jarvis использует безопасное значение по умолчанию.",
            RepairMode::ResetDefault,
        );
    }
}

fn check_enum(
    object: &Map<String, Value>,
    prefix: &str,
    key: &str,
    allowed: &[&str],
    issues: &mut Vec<ConfigIssue>,
) {
    let Some(value) = object.get(key) else {
        return;
    };
    if let Some(value) = value.as_str() {
        if !allowed.contains(&value) {
            issue(
                issues,
                &format!("{prefix}.{key}"),
                "invalid_enum",
                "Значение не поддерживается этой версией Jarvis.",
                RepairMode::ResetDefault,
            );
        }
    }
}

fn check_range(
    object: &Map<String, Value>,
    prefix: &str,
    key: &str,
    min: f64,
    max: f64,
    issues: &mut Vec<ConfigIssue>,
) {
    let Some(value) = object.get(key) else {
        return;
    };
    if let Some(value) = value.as_f64() {
        if !(min..=max).contains(&value) {
            issue(
                issues,
                &format!("{prefix}.{key}"),
                "out_of_range",
                "Число находится вне безопасного диапазона.",
                RepairMode::ResetDefault,
            );
        }
    }
}

fn shortcut_is_valid(raw: &str, template: bool) -> bool {
    let candidate = if template {
        if !raw.contains("{n}") {
            return false;
        }
        raw.replace("{n}", "1")
    } else {
        raw.to_string()
    };
    let parts: Vec<_> = candidate
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return false;
    }
    let key = parts.last().copied().unwrap_or_default();
    !key.is_empty()
        && parts[..parts.len().saturating_sub(1)].iter().all(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "command"
                    | "cmd"
                    | "meta"
                    | "super"
                    | "control"
                    | "ctrl"
                    | "alt"
                    | "option"
                    | "shift"
            )
        })
}

fn normalize_shortcut(raw: &str) -> String {
    raw.split('+')
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("+")
}

fn validate_known(root: &Map<String, Value>, issues: &mut Vec<ConfigIssue>) {
    let bool_fields = [
        "notifyDone",
        "notifyWaiting",
        "autoResume",
        "autoUpdate",
        "diagnostics",
        "launchDangerous",
    ];
    for key in bool_fields {
        check_type(root, "$", key, Value::is_boolean, issues);
    }
    for key in [
        "hotkey",
        "quietHotkey",
        "continueHotkey",
        "repeatHotkey",
        "muteHotkey",
        "selectHotkeyTemplate",
        "launchCustomCmd",
        "launchProxyCmd",
        "proxy",
    ] {
        check_type(root, "$", key, Value::is_string, issues);
    }
    check_enum(root, "$", "position", &["center", "corner"], issues);
    check_enum(
        root,
        "$",
        "launchTerminal",
        &["terminal-app", "iterm2", "custom"],
        issues,
    );

    let object_rules = [
        ("notify", &["content", "events"][..]),
        ("voice", &["events"][..]),
        ("stt", &[][..]),
        ("wake", &[][..]),
        ("verification", &[][..]),
        ("service", &[][..]),
        ("plugins", &[][..]),
    ];
    for (block, children) in object_rules {
        if let Some(object) = object_at(root, block, issues) {
            for child in children {
                let _ = object_at(object, child, issues);
            }
        }
    }

    if let Some(notify) = object_at(root, "notify", issues) {
        check_type(notify, "$.notify", "ttlSec", Value::is_number, issues);
        check_range(notify, "$.notify", "ttlSec", 1.0, 300.0, issues);
        for child in ["content", "events"] {
            if let Some(group) = object_at(notify, child, issues) {
                let known: &[&str] = if child == "content" {
                    &["branch", "model", "effort", "tokens", "time"]
                } else {
                    &["done", "waiting", "limit"]
                };
                for key in known {
                    if group.get(*key).is_some_and(|value| !value.is_boolean()) {
                        issue(
                            issues,
                            &format!("$.notify.{child}.{key}"),
                            "wrong_type",
                            "Флаг уведомления должен быть true или false.",
                            RepairMode::ResetDefault,
                        );
                    }
                }
            }
        }
    }

    if let Some(voice) = object_at(root, "voice", issues) {
        for key in ["engine", "speaker", "rate", "verbosity"] {
            check_type(voice, "$.voice", key, Value::is_string, issues);
        }
        for key in ["mute", "duckOthers", "bluetoothOnly"] {
            check_type(voice, "$.voice", key, Value::is_boolean, issues);
        }
        check_type(voice, "$.voice", "sampleRate", Value::is_number, issues);
        check_range(voice, "$.voice", "sampleRate", 8_000.0, 192_000.0, issues);
        check_enum(voice, "$.voice", "engine", &["silero"], issues);
        check_enum(
            voice,
            "$.voice",
            "rate",
            &["x-slow", "slow", "medium", "fast", "x-fast"],
            issues,
        );
        check_enum(
            voice,
            "$.voice",
            "verbosity",
            &["short", "descriptive"],
            issues,
        );
        if let Some(events) = object_at(voice, "events", issues) {
            for key in [
                "stop",
                "notification",
                "stopFailure",
                "subagentStop",
                "sessionEnd",
            ] {
                if events.get(key).is_some_and(|value| !value.is_boolean()) {
                    issue(
                        issues,
                        &format!("$.voice.events.{key}"),
                        "wrong_type",
                        "Флаг голосового события должен быть true или false.",
                        RepairMode::ResetDefault,
                    );
                }
            }
        }
    }

    if let Some(stt) = object_at(root, "stt", issues) {
        for key in ["engine", "dominantLang", "task", "hotkey"] {
            check_type(stt, "$.stt", key, Value::is_string, issues);
        }
        check_type(
            stt,
            "$.stt",
            "audioDevice",
            |value| value.is_string() || value.is_null(),
            issues,
        );
        check_type(stt, "$.stt", "noiseGate", Value::is_boolean, issues);
        check_enum(
            stt,
            "$.stt",
            "engine",
            &["whisper-turbo", "qwen3-0.6b", "qwen3-1.7b"],
            issues,
        );
        check_enum(stt, "$.stt", "task", &["transcribe", "translate"], issues);
    }

    if let Some(wake) = object_at(root, "wake", issues) {
        check_type(wake, "$.wake", "enabled", Value::is_boolean, issues);
        for key in ["engine", "model"] {
            check_type(wake, "$.wake", key, Value::is_string, issues);
        }
        for key in ["threshold", "debounce"] {
            check_type(wake, "$.wake", key, Value::is_number, issues);
        }
        check_enum(wake, "$.wake", "engine", &["openwakeword", "stub"], issues);
        check_range(wake, "$.wake", "threshold", 0.0, 1.0, issues);
        check_range(wake, "$.wake", "debounce", 1.0, 50.0, issues);
    }

    if let Some(verification) = object_at(root, "verification", issues) {
        check_type(
            verification,
            "$.verification",
            "enabled",
            Value::is_boolean,
            issues,
        );
        check_type(
            verification,
            "$.verification",
            "threshold",
            Value::is_number,
            issues,
        );
        check_type(
            verification,
            "$.verification",
            "profile",
            |value| value.is_string() || value.is_null(),
            issues,
        );
        check_range(
            verification,
            "$.verification",
            "threshold",
            0.0,
            1.0,
            issues,
        );
    }

    if let Some(service) = object_at(root, "service", issues) {
        for key in [
            "backend",
            "codexModel",
            "codexEffort",
            "claudeAuthMode",
            "claudeSecret",
            "proxy",
        ] {
            check_type(service, "$.service", key, Value::is_string, issues);
        }
        check_enum(
            service,
            "$.service",
            "backend",
            &["auto", "claude", "codex"],
            issues,
        );
        check_enum(
            service,
            "$.service",
            "codexEffort",
            &["", "minimal", "low", "medium", "high", "xhigh"],
            issues,
        );
        check_enum(
            service,
            "$.service",
            "claudeAuthMode",
            &["", "key", "subscription"],
            issues,
        );
    }

    let shortcut_fields = [
        ("hotkey", false),
        ("quietHotkey", false),
        ("continueHotkey", false),
        ("repeatHotkey", false),
        ("muteHotkey", false),
        ("selectHotkeyTemplate", true),
    ];
    let mut seen: Vec<(String, String)> = Vec::new();
    for (key, template) in shortcut_fields {
        let Some(raw) = root.get(key).and_then(Value::as_str) else {
            continue;
        };
        if raw == "none" {
            continue;
        }
        let path = format!("$.{key}");
        if !shortcut_is_valid(raw, template) {
            issue(
                issues,
                &path,
                "invalid_shortcut",
                "Сочетание клавиш записано в неподдерживаемом формате.",
                RepairMode::ResetDefault,
            );
            continue;
        }
        if !template {
            let normalized = normalize_shortcut(raw);
            if seen.iter().any(|(_, value)| value == &normalized) {
                issue(
                    issues,
                    &path,
                    "shortcut_conflict",
                    "Сочетание уже назначено другому действию Jarvis.",
                    RepairMode::ResetDefault,
                );
            } else {
                seen.push((path, normalized));
            }
        }
    }
    if let Some(raw) = root
        .get("stt")
        .and_then(Value::as_object)
        .and_then(|stt| stt.get("hotkey"))
        .and_then(Value::as_str)
    {
        if raw != "none" && !shortcut_is_valid(raw, false) {
            issue(
                issues,
                "$.stt.hotkey",
                "invalid_shortcut",
                "Сочетание клавиш записано в неподдерживаемом формате.",
                RepairMode::ResetDefault,
            );
        } else if raw != "none"
            && seen
                .iter()
                .any(|(_, value)| value == &normalize_shortcut(raw))
        {
            issue(
                issues,
                "$.stt.hotkey",
                "shortcut_conflict",
                "Сочетание уже назначено другому действию Jarvis.",
                RepairMode::ResetDefault,
            );
        }
    }

    if root.get("launchTerminal").and_then(Value::as_str) == Some("custom") {
        let valid = root
            .get("launchCustomCmd")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains("{cmd}"));
        if !valid {
            issue(
                issues,
                "$.launchCustomCmd",
                "missing_placeholder",
                "Пользовательская команда запуска должна содержать {cmd}.",
                RepairMode::ResetDefault,
            );
        }
    }
}

fn health_from_issues(path: impl Into<String>, issues: Vec<ConfigIssue>) -> ConfigHealth {
    if issues.is_empty() {
        return ConfigHealth::healthy(path);
    }
    ConfigHealth {
        status: HealthStatus::Error,
        path: path.into(),
        repairable: true,
        restart_required: true,
        issues,
    }
}

pub fn validate_raw(
    raw: Option<&str>,
    defaults: &Value,
    current_schema: u64,
    path: impl Into<String>,
) -> ConfigHealth {
    let path = path.into();
    let Some(raw) = raw else {
        return ConfigHealth::healthy(path);
    };
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(_) => {
            return health_from_issues(
                path,
                vec![ConfigIssue {
                    path: "$".into(),
                    code: "invalid_json".into(),
                    severity: IssueSeverity::Error,
                    message: "Файл не является корректным JSON.".into(),
                    repair: RepairMode::RecreateFile,
                }],
            )
        }
    };
    let Some(root) = value.as_object() else {
        return health_from_issues(
            path,
            vec![ConfigIssue {
                path: "$".into(),
                code: "root_not_object".into(),
                severity: IssueSeverity::Error,
                message: "Корень файла должен быть JSON-объектом.".into(),
                repair: RepairMode::RecreateFile,
            }],
        );
    };

    let mut issues = Vec::new();
    match root.get("schemaVersion") {
        Some(Value::Number(number)) if number.as_u64().is_some() => {
            if number.as_u64().unwrap_or_default() > current_schema {
                issue(
                    &mut issues,
                    "$.schemaVersion",
                    "unsupported_schema",
                    "Версия конфигурации новее, чем поддерживает этот Jarvis.",
                    RepairMode::ResetDefault,
                );
            }
        }
        Some(_) => issue(
            &mut issues,
            "$.schemaVersion",
            "wrong_type",
            "Версия схемы должна быть целым числом.",
            RepairMode::ResetDefault,
        ),
        None => {}
    }
    if let Some(defaults) = defaults.as_object() {
        validate_default_shape(root, defaults, "$", &mut issues);
    }
    validate_known(root, &mut issues);
    health_from_issues(path, issues)
}

fn replace_with_default_at_path(target: &mut Value, defaults: &Value, path: &str) {
    let segments: Vec<_> = path
        .trim_start_matches("$.")
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        *target = defaults.clone();
        return;
    }

    let fallback = defaults
        .pointer(&format!("/{}", segments.join("/")))
        .cloned()
        .or_else(|| known_default(path));
    let Some(fallback) = fallback else {
        return;
    };
    let mut cursor = target;
    for segment in &segments[..segments.len() - 1] {
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        cursor = cursor
            .as_object_mut()
            .expect("object assigned above")
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(object) = cursor.as_object_mut() {
        object.insert(segments[segments.len() - 1].to_string(), fallback);
    }
}

fn known_default(path: &str) -> Option<Value> {
    let value = match path {
        "$.voice" | "$.stt" | "$.wake" | "$.verification" | "$.service" | "$.plugins" => {
            serde_json::json!({})
        }
        "$.voice.events" => serde_json::json!({}),
        "$.voice.engine" => Value::from("silero"),
        "$.voice.speaker" => Value::from(""),
        "$.voice.sampleRate" => Value::from(48_000),
        "$.voice.rate" => Value::from("fast"),
        "$.voice.mute" => Value::from(false),
        "$.voice.duckOthers" => Value::from(true),
        "$.voice.verbosity" => Value::from("short"),
        "$.voice.bluetoothOnly" => Value::from(true),
        "$.stt.engine" => Value::from("whisper-turbo"),
        "$.stt.dominantLang" => Value::from("ru"),
        "$.stt.task" => Value::from("transcribe"),
        "$.stt.audioDevice" => Value::from(""),
        "$.stt.hotkey" => Value::from("F8"),
        "$.stt.noiseGate" => Value::from(false),
        "$.wake.enabled" => Value::from(false),
        "$.wake.engine" => Value::from("openwakeword"),
        "$.wake.model" => Value::from("hey_jarvis"),
        "$.wake.threshold" => Value::from(0.5),
        "$.wake.debounce" => Value::from(2),
        "$.verification.enabled" => Value::from(false),
        "$.verification.threshold" => Value::from(0.5),
        "$.verification.profile" => Value::from(""),
        "$.service.backend" => Value::from("auto"),
        "$.service.codexModel" => Value::from(""),
        "$.service.codexEffort" => Value::from("low"),
        "$.service.claudeAuthMode" => Value::from(""),
        "$.service.claudeSecret" => Value::from(""),
        "$.service.proxy" => Value::from(""),
        "$.launchCustomCmd" => Value::from("{cmd}"),
        "$.notify.content.branch" => Value::from(true),
        "$.notify.content.model"
        | "$.notify.content.effort"
        | "$.notify.content.tokens"
        | "$.notify.content.time" => Value::from(false),
        "$.notify.events.done" | "$.notify.events.waiting" | "$.notify.events.limit" => {
            Value::from(true)
        }
        "$.voice.events.stop" | "$.voice.events.notification" | "$.voice.events.stopFailure" => {
            Value::from(true)
        }
        "$.voice.events.subagentStop" | "$.voice.events.sessionEnd" => Value::from(false),
        _ => return None,
    };
    Some(value)
}

pub fn repair_raw(raw: &str, defaults: &Value, current_schema: u64) -> Result<Value, String> {
    let mut value = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(root)) => Value::Object(root),
        Ok(_) | Err(_) => return Ok(defaults.clone()),
    };
    let health = validate_raw(Some(raw), defaults, current_schema, "");
    for issue in health.issues {
        match issue.repair {
            RepairMode::Preserve => {}
            RepairMode::ResetDefault => {
                replace_with_default_at_path(&mut value, defaults, &issue.path)
            }
            RepairMode::RecreateFile => return Ok(defaults.clone()),
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defaults() -> serde_json::Value {
        json!({
            "schemaVersion": 1,
            "hotkey": "Command+J",
            "position": "center",
            "launchTerminal": "terminal-app",
            "notifyDone": true,
            "notify": {
                "ttlSec": 8,
                "content": { "branch": true },
                "events": { "done": true }
            }
        })
    }

    #[test]
    fn missing_file_is_healthy() {
        let health = validate_raw(None, &defaults(), 1, "/tmp/settings.json");
        assert_eq!(health.status, HealthStatus::Healthy);
        assert!(health.issues.is_empty());
        assert!(!health.repairable);
    }

    #[test]
    fn malformed_json_and_non_object_root_are_repairable() {
        let malformed = validate_raw(
            Some("{ definitely not json"),
            &defaults(),
            1,
            "/tmp/settings.json",
        );
        assert_eq!(malformed.status, HealthStatus::Error);
        assert_eq!(malformed.issues[0].code, "invalid_json");
        assert!(malformed.repairable);

        let array = validate_raw(Some("[]"), &defaults(), 1, "/tmp/settings.json");
        assert_eq!(array.issues[0].code, "root_not_object");
    }

    #[test]
    fn reports_known_type_enum_range_and_schema_errors_without_values() {
        let health = validate_raw(
            Some(
                r#"{
                    "schemaVersion": 99,
                    "notifyDone": "yes",
                    "position": "somewhere",
                    "notify": { "ttlSec": 999 }
                }"#,
            ),
            &defaults(),
            1,
            "/tmp/settings.json",
        );
        let codes: Vec<_> = health
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect();
        assert!(codes.contains(&"unsupported_schema"));
        assert!(codes.contains(&"wrong_type"));
        assert!(codes.contains(&"invalid_enum"));
        assert!(codes.contains(&"out_of_range"));
        let serialized = serde_json::to_string(&health).unwrap();
        assert!(!serialized.contains("somewhere"));
        assert!(!serialized.contains("999"));
    }

    #[test]
    fn repair_preserves_unknown_and_valid_fields_but_resets_invalid_known_fields() {
        let raw = r#"{
            "schemaVersion": 1,
            "hotkey": "Command+K",
            "notifyDone": "yes",
            "position": "corner",
            "pluginSecret": { "token": "keep-me" },
            "notify": { "ttlSec": 999, "future": true }
        }"#;
        let repaired = repair_raw(raw, &defaults(), 1).unwrap();
        assert_eq!(repaired["hotkey"], "Command+K");
        assert_eq!(repaired["position"], "corner");
        assert_eq!(repaired["notifyDone"], true);
        assert_eq!(repaired["notify"]["ttlSec"], 8);
        assert_eq!(repaired["notify"]["future"], true);
        assert_eq!(repaired["pluginSecret"]["token"], "keep-me");
    }

    #[test]
    fn repair_recreates_unusable_root_from_defaults() {
        assert_eq!(repair_raw("{", &defaults(), 1).unwrap(), defaults());
        assert_eq!(repair_raw("[]", &defaults(), 1).unwrap(), defaults());
    }
}
