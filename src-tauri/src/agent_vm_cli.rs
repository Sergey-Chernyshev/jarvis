//! Standalone Agent VM terminal control.
//!
//! `jarvis vm ...` is handled before AppKit/Tauri startup, so a shell can
//! inspect and attach to an already-running interactive Agent VM session even
//! when the Jarvis panel is closed. The command never creates or starts a VM.

use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use crate::agent_vm_terminal::{self, TerminalSession, TerminalTools};

#[derive(Clone, Debug, PartialEq, Eq)]
enum VmCommand {
    Help,
    List { all: bool },
    Attach { target: Option<String>, all: bool },
}

pub fn maybe_run() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let (first, rest) = args.split_first()?;
    if first != "vm" {
        return None;
    }
    Some(match parse(rest) {
        Ok(command) => run(command),
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            eprintln!("{}", usage());
            2
        }
    })
}

fn parse(args: &[OsString]) -> Result<VmCommand, String> {
    let args = args
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| "аргументы должны быть UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    match args.as_slice() {
        [] => Ok(VmCommand::Help),
        [value] if value == "help" || value == "-h" || value == "--help" => Ok(VmCommand::Help),
        [command] if command == "list" => Ok(VmCommand::List { all: false }),
        [command, flag] if command == "list" && flag == "--all" => {
            Ok(VmCommand::List { all: true })
        }
        [command] if command == "attach" => Ok(VmCommand::Attach {
            target: None,
            all: false,
        }),
        [command, flag] if command == "attach" && flag == "--all" => Ok(VmCommand::Attach {
            target: None,
            all: true,
        }),
        [command, target] if command == "attach" => Ok(VmCommand::Attach {
            target: Some(target.clone()),
            all: true,
        }),
        _ => Err("неизвестная команда или лишние аргументы".into()),
    }
}

fn run(command: VmCommand) -> i32 {
    if command == VmCommand::Help {
        println!("{}", usage());
        return 0;
    }
    let tools = match TerminalTools::discover() {
        Ok(tools) => tools,
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    let sessions = match agent_vm_terminal::list_sessions(&tools) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("jarvis vm: {error}");
            return 1;
        }
    };
    match command {
        VmCommand::Help => 0,
        VmCommand::List { all } => {
            let visible = visible_sessions(&sessions, current_project_id().as_deref(), all);
            print_sessions(&visible);
            0
        }
        VmCommand::Attach { target, all } => {
            let visible = visible_sessions(&sessions, current_project_id().as_deref(), all);
            let selected = match choose_session(&sessions, &visible, target.as_deref()) {
                Ok(selected) => selected,
                Err(error) => {
                    eprintln!("jarvis vm: {error}");
                    if !visible.is_empty() {
                        print_sessions(&visible);
                    }
                    return 2;
                }
            };
            match agent_vm_terminal::attach_session(&tools, &selected.session_name) {
                Ok(status) => status,
                Err(error) => {
                    eprintln!("jarvis vm: {error}");
                    1
                }
            }
        }
    }
}

fn current_project_id() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    crate::agent_vm::identity_for_path(Path::new(&cwd))
        .ok()
        .map(|identity| identity.project_id)
}

fn visible_sessions<'a>(
    sessions: &'a [TerminalSession],
    project_id: Option<&str>,
    all: bool,
) -> Vec<&'a TerminalSession> {
    if all {
        return sessions.iter().collect();
    }
    let local = project_id
        .map(|project_id| {
            sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if local.is_empty() {
        sessions.iter().collect()
    } else {
        local
    }
}

fn choose_session<'a>(
    all_sessions: &'a [TerminalSession],
    visible: &[&'a TerminalSession],
    target: Option<&str>,
) -> Result<&'a TerminalSession, String> {
    if let Some(target) = target {
        return all_sessions
            .iter()
            .find(|session| session.session_name == target)
            .ok_or_else(|| "указанная Agent VM session не найдена".to_string());
    }
    match visible {
        [] => Err("нет активных Agent VM terminal sessions".into()),
        [only] => Ok(*only),
        many if !io::stdin().is_terminal() => Err(format!(
            "найдено {} sessions; укажите точное имя: jarvis vm attach <session>",
            many.len()
        )),
        many => {
            eprintln!("Выберите Agent VM session:");
            for (index, session) in many.iter().enumerate() {
                eprintln!(
                    "  {}) {} [{}]{}",
                    index + 1,
                    session.session_name,
                    session.backend.as_str(),
                    if session.attached {
                        " — уже подключена"
                    } else {
                        ""
                    }
                );
            }
            eprint!("> ");
            let _ = io::stderr().flush();
            let mut input = String::new();
            io::stdin()
                .lock()
                .read_line(&mut input)
                .map_err(|_| "не прочитать выбор session".to_string())?;
            let selected = input
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=many.len()).contains(index))
                .ok_or_else(|| "некорректный номер session".to_string())?;
            Ok(many[selected - 1])
        }
    }
}

fn print_sessions(sessions: &[&TerminalSession]) {
    if sessions.is_empty() {
        println!("Нет активных Agent VM terminal sessions.");
        return;
    }
    println!("SESSION\tPROJECT\tBACKEND\tATTACHED");
    for session in sessions {
        println!(
            "{}\t{}\t{}\t{}",
            session.session_name,
            session.project_id,
            session.backend.as_str(),
            if session.attached { "yes" } else { "no" }
        );
    }
}

fn usage() -> &'static str {
    "Использование:
  jarvis vm list [--all]
  jarvis vm attach [--all | <session>]

Без --all сначала выбираются terminal sessions текущей project-папки.
Команда attach не запускает и не пересоздаёт VM."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_vm_terminal::TerminalBackend;

    fn session(name: &str, project: &str, backend: TerminalBackend) -> TerminalSession {
        TerminalSession {
            session_name: name.into(),
            project_id: project.into(),
            backend,
            attached: false,
            activity: 42,
        }
    }

    #[test]
    fn parses_list_attach_and_help_without_starting_tauri() {
        assert_eq!(parse(&[]).unwrap(), VmCommand::Help);
        assert_eq!(
            parse(&[OsString::from("list")]).unwrap(),
            VmCommand::List { all: false }
        );
        assert_eq!(
            parse(&[OsString::from("attach"), OsString::from("--all")]).unwrap(),
            VmCommand::Attach {
                target: None,
                all: true
            }
        );
        assert_eq!(
            parse(&[
                OsString::from("attach"),
                OsString::from("avm-project-0123456789abcdef-claude")
            ])
            .unwrap(),
            VmCommand::Attach {
                target: Some("avm-project-0123456789abcdef-claude".into()),
                all: true
            }
        );
        assert!(parse(&[OsString::from("destroy")]).is_err());
    }

    #[test]
    fn current_directory_sessions_are_preferred_but_all_remain_discoverable() {
        let sessions = vec![
            session(
                "avm-project-0123456789abcdef-claude",
                "project-0123456789abcdef",
                TerminalBackend::Claude,
            ),
            session(
                "avm-project-fedcba9876543210-codex",
                "project-fedcba9876543210",
                TerminalBackend::Codex,
            ),
        ];

        let local = visible_sessions(&sessions, Some("project-fedcba9876543210"), false);
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].backend, TerminalBackend::Codex);
        assert_eq!(visible_sessions(&sessions, None, false).len(), 2);
        assert_eq!(
            visible_sessions(&sessions, Some("project-fedcba9876543210"), true).len(),
            2
        );
    }

    #[test]
    fn explicit_attach_only_selects_an_observed_exact_session() {
        let sessions = vec![session(
            "avm-project-0123456789abcdef-claude",
            "project-0123456789abcdef",
            TerminalBackend::Claude,
        )];
        let visible = sessions.iter().collect::<Vec<_>>();

        assert_eq!(
            choose_session(
                &sessions,
                &visible,
                Some("avm-project-0123456789abcdef-claude")
            )
            .unwrap()
            .session_name,
            "avm-project-0123456789abcdef-claude"
        );
        assert!(choose_session(&sessions, &visible, Some("$(touch /tmp/no)")).is_err());
    }
}
