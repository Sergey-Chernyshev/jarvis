use std::sync::Mutex;

use crate::plugin_cli::{dispatch_cli, PluginCli};
use crate::plugin_manager_api::{
    dispatch_ipc, ManagerApiError, ManagerRequest, ManagerResponse, PluginManagementApi,
};

#[test]
fn parses_all_public_plugin_commands_without_starting_tauri() {
    for args in [
        vec!["jarvis", "plugin", "catalog", "agent"],
        vec!["jarvis", "plugin", "info", "dev.example.echo"],
        vec!["jarvis", "plugin", "install", "dev.example.echo@1.0.0"],
        vec!["jarvis", "plugin", "update", "dev.example.echo"],
        vec![
            "jarvis",
            "plugin",
            "rollback",
            "dev.example.echo",
            "--to",
            "1.0.0",
        ],
        vec!["jarvis", "plugin", "enable", "dev.example.echo"],
        vec!["jarvis", "plugin", "disable", "dev.example.echo"],
        vec!["jarvis", "plugin", "uninstall", "dev.example.echo"],
        vec![
            "jarvis",
            "plugin",
            "purge",
            "dev.example.echo",
            "--confirm",
            "dev.example.echo",
        ],
        vec!["jarvis", "plugin", "doctor", "dev.example.echo"],
        vec!["jarvis", "plugin", "validate", "./plugin"],
        vec!["jarvis", "plugin", "pack", "./plugin"],
        vec!["jarvis", "plugin", "link", "./plugin"],
        vec!["jarvis", "plugin", "unlink", "dev.example.echo"],
        vec!["jarvis", "plugin", "reload", "dev.example.echo"],
        vec!["jarvis", "plugin", "logs", "dev.example.echo"],
        vec!["jarvis", "plugin", "list", "--dev"],
        vec!["jarvis", "plugin", "developer-mode", "enable"],
    ] {
        assert!(PluginCli::try_parse_from(args).is_ok());
    }
}

#[derive(Default)]
struct RecordingManager {
    requests: Mutex<Vec<ManagerRequest>>,
}

impl PluginManagementApi for RecordingManager {
    fn request(&self, request: ManagerRequest) -> Result<ManagerResponse, ManagerApiError> {
        self.requests.lock().unwrap().push(request);
        Ok(ManagerResponse::List {
            plugins: Vec::new(),
        })
    }
}

#[test]
fn cli_and_ipc_dispatch_the_same_manager_request() {
    let api = RecordingManager::default();
    let cli =
        PluginCli::try_parse_from(["jarvis", "plugin", "disable", "dev.example.echo"]).unwrap();
    dispatch_cli(cli, &api).unwrap();
    dispatch_ipc(
        ManagerRequest::Disable {
            plugin_id: "dev.example.echo".into(),
        },
        &api,
    )
    .unwrap();
    assert_eq!(
        api.requests.lock().unwrap().as_slice(),
        [
            ManagerRequest::Disable {
                plugin_id: "dev.example.echo".into()
            },
            ManagerRequest::Disable {
                plugin_id: "dev.example.echo".into()
            },
        ]
    );
}

#[test]
fn commit_requires_explicit_non_interactive_consent_flags() {
    let cli =
        PluginCli::try_parse_from(["jarvis", "plugin", "install", "--commit", "op-1"]).unwrap();
    assert_eq!(
        cli.request,
        ManagerRequest::CommitInstall {
            operation_id: "op-1".into(),
            accept_permissions: false,
            trust_native_digest: None,
            approve_irreversible_migration: false,
        }
    );
}
