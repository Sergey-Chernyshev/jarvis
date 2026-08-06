use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const CORE_WEBVIEWS: [&str; 4] = ["main", "toast", "onboarding", "agent-chat"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a repository parent")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn inventory() -> BTreeMap<String, BTreeSet<String>> {
    let path = repo_root().join("src-tauri/src/app_command_inventory.rs");
    let source = read(&path);
    let row = Regex::new(
        r#"\(\s*"(?P<name>[a-z][a-z0-9_]*)"\s*,\s*crate::(?:ipc|onboarding)::(?P<handler>[a-z][a-z0-9_]*)\s*,\s*\[(?P<webviews>[^\]]*)\]\s*\)"#,
    )
    .unwrap();
    let label = Regex::new(r#""([^"]+)""#).unwrap();

    let mut inventory = BTreeMap::new();
    for captures in row.captures_iter(&source) {
        let name = captures["name"].to_owned();
        assert_eq!(
            name, captures["handler"],
            "wire command name and Rust handler must stay identical"
        );
        let webviews = label
            .captures_iter(&captures["webviews"])
            .map(|capture| capture[1].to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            webviews.len(),
            label.captures_iter(&captures["webviews"]).count(),
            "{name} repeats a webview label"
        );
        assert!(
            inventory.insert(name.clone(), webviews).is_none(),
            "{name} occurs more than once in the command inventory"
        );
    }

    assert!(
        !inventory.is_empty(),
        "the app command inventory must contain every app command"
    );
    inventory
}

fn annotated_commands() -> BTreeSet<String> {
    let command = Regex::new(
        r#"#\[tauri::command\]\s*pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+([a-zA-Z][a-zA-Z0-9_]*)"#,
    )
    .unwrap();
    ["src-tauri/src/ipc.rs", "src-tauri/src/onboarding.rs"]
        .into_iter()
        .flat_map(|relative| {
            let source = read(repo_root().join(relative));
            command
                .captures_iter(&source)
                .map(|capture| capture[1].to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn capability(identifier: &str) -> Value {
    let path = repo_root().join(format!("src-tauri/capabilities/{identifier}.json"));
    serde_json::from_str(&read(path)).expect("capability is valid JSON")
}

fn strings<'a>(value: &'a Value, key: &str) -> BTreeSet<&'a str> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{key} must be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{key} entries must be strings"))
        })
        .collect()
}

#[test]
fn build_and_runtime_share_the_complete_command_inventory() {
    let inventory = inventory();
    assert_eq!(
        inventory.keys().cloned().collect::<BTreeSet<_>>(),
        annotated_commands(),
        "every #[tauri::command] must pass through the one command inventory"
    );

    let build = read(repo_root().join("src-tauri/build.rs"));
    assert!(
        build.contains("tauri_build::AppManifest::new().commands(APP_COMMAND_NAMES)"),
        "build.rs must generate explicit permissions from the shared command names"
    );
    assert!(
        build.contains("with_app_commands!"),
        "build.rs must expand the shared inventory"
    );

    let main = read(repo_root().join("src-tauri/src/main.rs"));
    assert!(
        main.contains("with_app_commands!"),
        "main.rs must build invoke_handler from the shared inventory"
    );
    assert!(
        main.contains("tauri::generate_handler!"),
        "the inventory callback must expand into Tauri's runtime handler"
    );
    assert!(
        !main.contains(".invoke_handler(tauri::generate_handler!["),
        "main.rs must not retain a second hand-written command list"
    );
}

#[test]
fn exact_webview_capabilities_match_the_inventory_grants() {
    let inventory = inventory();
    let known_webviews = CORE_WEBVIEWS.into_iter().collect::<BTreeSet<_>>();
    for (command, webviews) in &inventory {
        assert!(
            webviews
                .iter()
                .all(|label| known_webviews.contains(label.as_str())),
            "{command} names an unknown or plugin webview: {webviews:?}"
        );
    }

    let mut actual_grants = BTreeMap::<String, BTreeSet<String>>::new();
    for identifier in CORE_WEBVIEWS {
        let capability = capability(identifier);
        assert_eq!(
            capability.get("identifier").and_then(Value::as_str),
            Some(identifier)
        );
        assert!(
            capability.get("windows").is_none(),
            "{identifier} must never use window-wide capability scope"
        );
        assert_eq!(
            strings(&capability, "webviews"),
            BTreeSet::from([identifier]),
            "{identifier} must target exactly its matching webview"
        );

        for permission in strings(&capability, "permissions") {
            assert_ne!(permission, "core:default");
            if let Some(command_slug) = permission.strip_prefix("allow-") {
                let command = command_slug.replace('-', "_");
                actual_grants
                    .entry(command)
                    .or_default()
                    .insert(identifier.to_owned());
            }
        }
    }

    let expected_grants = inventory
        .into_iter()
        .filter(|(_, webviews)| !webviews.is_empty())
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_grants, expected_grants,
        "generated allow-<command> permissions must match only the recorded webviews"
    );
}

#[test]
fn tauri_config_enables_only_the_four_scoped_capabilities() {
    let config: Value =
        serde_json::from_str(&read(repo_root().join("src-tauri/tauri.conf.json"))).unwrap();
    let app = config.get("app").expect("app config");
    assert!(
        app.get("windows").is_none(),
        "trusted windows are created programmatically; static window config is forbidden"
    );
    assert_eq!(
        app.get("withGlobalTauri").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        strings(
            app.get("security").expect("app.security config"),
            "capabilities"
        ),
        CORE_WEBVIEWS.into_iter().collect()
    );
    assert!(
        app.pointer("/security/csp")
            .is_some_and(|value| !value.is_null()),
        "trusted core UI must have an explicit non-null CSP"
    );

    for entry in fs::read_dir(repo_root().join("src-tauri/capabilities")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let value: Value = serde_json::from_str(&read(&path)).unwrap();
        let Some(identifier) = value.get("identifier").and_then(Value::as_str) else {
            continue;
        };
        assert!(
            CORE_WEBVIEWS.contains(&identifier),
            "unlisted capability file is forbidden: {}",
            path.display()
        );
        for label in strings(&value, "webviews") {
            assert!(!label.contains('*'), "wildcard webview label is forbidden");
            assert!(
                !label.starts_with("plugin-"),
                "plugin webviews never receive Tauri capabilities"
            );
        }
    }
}
