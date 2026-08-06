//! Standalone `jarvis plugin ...` entrypoint.
//!
//! Parsing and dispatch happen before Tauri/AppKit initialization. The CLI and
//! Tauri bridge both submit the same `ManagerRequest` to `PluginManagementApi`.

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal};

use crate::plugin_manager_api::{
    dispatch_manager_request, ManagerApiError, ManagerRequest, ManagerResponse,
    PluginManagementApi, PluginManagerEndpoint,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginCli {
    pub(crate) request: ManagerRequest,
    json: bool,
}

impl PluginCli {
    pub(crate) fn try_parse_from<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut args = args
            .into_iter()
            .map(|value| {
                value
                    .as_ref()
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "arguments must be UTF-8".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !args.is_empty() {
            args.remove(0);
        }
        if args.first().map(String::as_str) != Some("plugin") {
            return Err("expected `plugin` command".into());
        }
        args.remove(0);
        let json = take_flag(&mut args, "--json");
        let command = take_required(&mut args, "plugin command")?;
        let request = match command.as_str() {
            "catalog" => ManagerRequest::Catalog {
                query: take_optional_positional(&mut args),
            },
            "info" => ManagerRequest::Info {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "install" => parse_install(&mut args)?,
            "update" => ManagerRequest::Update {
                plugin_id: take_optional_positional(&mut args),
            },
            "rollback" => ManagerRequest::Rollback {
                plugin_id: take_required(&mut args, "plugin id")?,
                version: take_option(&mut args, "--to")?,
            },
            "enable" => ManagerRequest::Enable {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "disable" => ManagerRequest::Disable {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "uninstall" => ManagerRequest::Uninstall {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "purge" => ManagerRequest::Purge {
                plugin_id: take_required(&mut args, "plugin id")?,
                confirmation: take_option(&mut args, "--confirm")?
                    .ok_or_else(|| "--confirm <exact-plugin-id> is required".to_string())?,
            },
            "doctor" => ManagerRequest::Doctor {
                plugin_id: take_optional_positional(&mut args),
            },
            "validate" => ManagerRequest::Validate {
                source: take_required(&mut args, "plugin source path")?,
            },
            "pack" => ManagerRequest::Pack {
                source: take_required(&mut args, "plugin source path")?,
                output: take_option(&mut args, "--output")?,
            },
            "link" => ManagerRequest::Link {
                source: take_required(&mut args, "plugin source path")?,
                accept_permissions: take_flag(&mut args, "--accept-permissions"),
                trust_native_digest: take_option(&mut args, "--trust-native-digest")?,
            },
            "unlink" => ManagerRequest::Unlink {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "reload" => ManagerRequest::Reload {
                plugin_id: take_required(&mut args, "plugin id")?,
                accept_permissions: take_flag(&mut args, "--accept-permissions"),
                trust_native_digest: take_option(&mut args, "--trust-native-digest")?,
            },
            "logs" => ManagerRequest::Logs {
                plugin_id: take_required(&mut args, "plugin id")?,
            },
            "list" => ManagerRequest::List {
                developer_only: take_flag(&mut args, "--dev"),
            },
            "developer-mode" => {
                let enabled = match take_required(&mut args, "enable or disable")?.as_str() {
                    "enable" => true,
                    "disable" => false,
                    _ => return Err("developer-mode expects `enable` or `disable`".into()),
                };
                ManagerRequest::DeveloperMode { enabled }
            }
            "help" | "-h" | "--help" => return Err(usage().into()),
            _ => return Err(format!("unknown plugin command: {command}")),
        };
        if !args.is_empty() {
            return Err(format!("unexpected arguments: {}", args.join(" ")));
        }
        Ok(Self { request, json })
    }
}

fn parse_install(args: &mut Vec<String>) -> Result<ManagerRequest, String> {
    if take_flag(args, "--commit") {
        let operation_id = take_required(args, "operation id")?;
        return Ok(ManagerRequest::CommitInstall {
            operation_id,
            accept_permissions: take_flag(args, "--accept-permissions"),
            trust_native_digest: take_option(args, "--trust-native-digest")?,
            approve_irreversible_migration: take_flag(args, "--approve-irreversible-migration"),
        });
    }
    Ok(ManagerRequest::PrepareInstall {
        source: take_required(args, "plugin id, id@version, or archive")?,
    })
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let Some(index) = args.iter().position(|value| value == flag) else {
        return false;
    };
    args.remove(index);
    true
}

fn take_option(args: &mut Vec<String>, option: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|value| value == option) else {
        return Ok(None);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(format!("{option} requires a value"));
    }
    Ok(Some(args.remove(index)))
}

fn take_required(args: &mut Vec<String>, label: &str) -> Result<String, String> {
    if args.is_empty() || args[0].starts_with('-') {
        return Err(format!("{label} is required"));
    }
    Ok(args.remove(0))
}

fn take_optional_positional(args: &mut Vec<String>) -> Option<String> {
    if args.first().is_some_and(|value| !value.starts_with('-')) {
        Some(args.remove(0))
    } else {
        None
    }
}

pub(crate) fn dispatch_cli(
    cli: PluginCli,
    api: &dyn PluginManagementApi,
) -> Result<ManagerResponse, ManagerApiError> {
    dispatch_manager_request(api, cli.request)
}

pub fn maybe_run() -> Option<i32> {
    let args = std::env::args_os().collect::<Vec<OsString>>();
    if args.get(1).and_then(|value| value.to_str()) != Some("plugin") {
        return None;
    }
    Some(match PluginCli::try_parse_from(args) {
        Ok(cli) => {
            let json = cli.json;
            match PluginManagerEndpoint::new(crate::settings::Store::new())
                .and_then(|api| dispatch_cli(cli, &api))
            {
                Ok(response) => {
                    print_response(&response, json);
                    0
                }
                Err(error) => {
                    print_error(&error, json);
                    if error.requires_explicit_consent() || !io::stdin().is_terminal() {
                        2
                    } else {
                        1
                    }
                }
            }
        }
        Err(error) => {
            eprintln!("jarvis plugin: {error}");
            if !error.starts_with("Usage:") {
                eprintln!("{}", usage());
            }
            2
        }
    })
}

fn print_response(response: &ManagerResponse, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(response).expect("ManagerResponse always serializes")
        );
        return;
    }
    match response {
        ManagerResponse::Catalog { items } => {
            if items.is_empty() {
                println!("Trusted plugin catalog is empty.");
            } else {
                for item in items {
                    println!(
                        "{}\t{}\t{}",
                        item.plugin_id.as_str(),
                        item.version,
                        item.target.as_str()
                    );
                }
            }
        }
        ManagerResponse::InstallPlan { plan } => {
            println!("Operation: {}", plan.operation_id);
            println!("Plugin: {} {}", plan.plugin_id.as_str(), plan.version);
            println!("Digest: {}", plan.package_digest.as_str());
            if !plan.permission_diff.added.is_empty() {
                println!(
                    "Added permissions: {}",
                    plan.permission_diff.added.join(", ")
                );
            }
            if let Some(digest) = &plan.native_trust_digest {
                println!("Native digest: {}", digest.as_str());
            }
            println!(
                "Commit: jarvis plugin install --commit {} --accept-permissions{}",
                plan.operation_id,
                plan.native_trust_digest
                    .as_ref()
                    .map(|digest| format!(" --trust-native-digest {}", digest.as_str()))
                    .unwrap_or_default()
            );
        }
        ManagerResponse::DeveloperPlan { plan } => {
            println!("Plugin: {}", plan.plugin_id.as_str());
            println!("Digest: {}", plan.package_digest.as_str());
            println!("Snapshot: {}", plan.snapshot.display());
            println!("Review the permission diff and repeat with --accept-permissions.");
        }
        ManagerResponse::Packed {
            output,
            package_digest,
            trust,
            ..
        } => {
            println!("{}  {}", package_digest.as_str(), output.display());
            println!("Trust: {trust}");
        }
        ManagerResponse::DeveloperLinked { receipt, snapshot } => {
            println!(
                "Linked {} generation {} from {}",
                receipt.plugin_id.as_str(),
                receipt.generation,
                snapshot.display()
            );
        }
        ManagerResponse::DeveloperUnlinked {
            plugin_id,
            generation,
        } => println!("Unlinked {} generation {}", plugin_id.as_str(), generation),
        ManagerResponse::DeveloperMode {
            enabled,
            revoked_links,
        } => println!(
            "Developer Mode: {} (revoked links: {revoked_links})",
            if *enabled { "enabled" } else { "disabled" }
        ),
        ManagerResponse::List { plugins } => {
            if plugins.is_empty() {
                println!("No managed plugins.");
            } else {
                println!("PLUGIN\tVERSION\tSOURCE\tENABLED\tGENERATION");
                for plugin in plugins {
                    println!(
                        "{}\t{}\t{:?}\t{}\t{}",
                        plugin.plugin_id.as_str(),
                        plugin.version,
                        plugin.source,
                        plugin.enabled,
                        plugin.generation
                    );
                }
            }
        }
        ManagerResponse::Logs { path, lines } => {
            println!("{}", path.display());
            for line in lines {
                println!("{line}");
            }
        }
        other => println!(
            "{}",
            serde_json::to_string_pretty(other).expect("ManagerResponse always serializes")
        ),
    }
}

fn print_error(error: &ManagerApiError, json: bool) {
    if json {
        eprintln!(
            "{}",
            serde_json::to_string(error).expect("ManagerApiError always serializes")
        );
    } else {
        eprintln!("jarvis plugin: {error}");
    }
}

fn usage() -> &'static str {
    "Usage:
  jarvis plugin catalog [query] [--json]
  jarvis plugin info <id> [--json]
  jarvis plugin install <id[@version]> [--json]
  jarvis plugin install --commit <operation> --accept-permissions \
[--trust-native-digest sha256:...] [--approve-irreversible-migration]
  jarvis plugin update [id]
  jarvis plugin rollback <id> [--to version]
  jarvis plugin enable|disable|uninstall <id>
  jarvis plugin purge <id> --confirm <exact-id>
  jarvis plugin doctor [id]
  jarvis plugin validate <folder>
  jarvis plugin pack <folder> [--output file]
  jarvis plugin link <folder> [--accept-permissions] [--trust-native-digest sha256:...]
  jarvis plugin unlink|reload|logs <id>
  jarvis plugin list [--dev]
  jarvis plugin developer-mode enable|disable"
}
