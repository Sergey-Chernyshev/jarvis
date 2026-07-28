use std::path::PathBuf;

use serde_json::Value;

pub mod manifest;
pub mod protocol;
pub mod supervisor;

fn roots_from_sources(
    settings: &Value,
    env_override: Option<&str>,
    installed: PathBuf,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let path = PathBuf::from(trimmed);
        if !roots.contains(&path) {
            roots.push(path);
        }
    };
    if let Some(raw) = env_override {
        push(raw);
    }
    if let Some(raw) = settings.get("pluginsDevDir").and_then(Value::as_str) {
        push(raw);
    }
    if !roots.contains(&installed) {
        roots.push(installed);
    }
    roots
}

pub fn roots_from_settings(settings: &Value) -> Vec<PathBuf> {
    let env_override = std::env::var("JARVIS_PLUGIN_DEV_DIR").ok();
    roots_from_sources(
        settings,
        env_override.as_deref(),
        crate::util::jarvis_dir().join("plugins"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn roots_prefer_env_then_settings_then_installed_and_dedupe() {
        let roots = roots_from_sources(
            &json!({ "pluginsDevDir": "/settings/plugins" }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            roots,
            [
                PathBuf::from("/env/plugins"),
                PathBuf::from("/settings/plugins"),
                PathBuf::from("/installed/plugins"),
            ]
        );

        let deduped = roots_from_sources(
            &json!({ "pluginsDevDir": "/env/plugins" }),
            Some("/env/plugins"),
            PathBuf::from("/installed/plugins"),
        );
        assert_eq!(
            deduped,
            [
                PathBuf::from("/env/plugins"),
                PathBuf::from("/installed/plugins"),
            ]
        );
    }

    #[test]
    fn roots_ignore_blank_or_non_string_dev_values() {
        for settings in [
            json!({ "pluginsDevDir": "" }),
            json!({ "pluginsDevDir": 42 }),
            json!({}),
        ] {
            assert_eq!(
                roots_from_sources(&settings, Some("  "), PathBuf::from("/installed/plugins"),),
                [PathBuf::from("/installed/plugins")]
            );
        }
    }
}
