//! CLI-обёртка над общей install-логикой (она в src/install/mod.rs).
//!
//!   jarvis-setup install     — вшить хуки + транспорт + Silero
//!   jarvis-setup uninstall   — вычистить интеграцию
//!   jarvis-setup status      — показать, что установлено
//!   jarvis-setup repair      — только интеграция агентов (хуки + шим)
//!   jarvis-setup remote add <имя> <ssh-хост> [--dir <путь>]
//!                            — поставить узел на удалённую машину (docs/remote.md)
//!   jarvis-setup remote status <имя>
//!                            — жив ли узел: процесс, сокет, версия, паны tmux
//!
//! Та же логика используется приложением (онбординг первого запуска).

#[path = "../install/mod.rs"]
mod install;
#[path = "../agent_instances.rs"]
mod agent_instances;
#[path = "../codex_hooks.rs"]
mod codex_hooks;

use install::{Step, StepState};

const USAGE: &str = "\
Использование:
  jarvis-setup install                    вшить хуки + транспорт + Silero
  jarvis-setup uninstall                  вычистить интеграцию
  jarvis-setup status                     показать, что установлено
  jarvis-setup repair                     починить интеграцию агентов (хуки + шим)
  jarvis-setup instances list             показать найденные профили Codex
  jarvis-setup instances repair [id]      настроить точные хуки выбранного профиля
  jarvis-setup remote add <имя> <ssh-хост> [--transport ssh|teleport] [параметры]
                                          поставить узел на удалённую машину
  jarvis-setup remote status <имя>        жив ли узел на той стороне
";

const REMOTE_USAGE: &str = "\
Удалённые узлы (подробности — docs/remote.md):
  jarvis-setup remote add <имя> <ssh-хост> [--dir <путь>]
      Ставит jarvis-node на ту машину, прописывает хуки агентов, настраивает
      автозапуск и добавляет узел в настройки Jarvis.
      <имя>      как узел будет называться в списке сессий (латиница/цифры/.-_)
      <ssh-хост> то же, что пишешь в ssh: алиас из ~/.ssh/config или user@адрес
      --transport ssh|teleport  транспорт подключения (по умолчанию ssh)
      --proxy     адрес Teleport proxy, например teleport.example.com:443
      --cluster   кластер Teleport (без этих флагов используется профиль tsh)
      --ssh-config абсолютный путь к конфигурации OpenSSH (только ssh)
      --run-as-user пользователь, от которого ставится узел через sudo -n
      --dir      каталог Jarvis на той стороне (по умолчанию ~/.jarvis)
      --tcp=N    порт узла на петле для мобильного клиента (по умолчанию 7717)
      --no-tcp   не поднимать этот порт (только ssh; Teleport требует TCP)

  jarvis-setup remote status <имя>
      Процесс узла, сокет, версия, состояние юнита и живые паны tmux.
";

/// Печать шага установки для терминала.
fn print_step(s: Step) {
    match s.state {
        StepState::Start => println!("▸ {}", s.phase),
        StepState::Done => println!("  ✓ {}", s.msg),
        StepState::Warn => println!("  ⚠ {}", s.msg),
        StepState::Info => println!("  • {}", s.msg),
    }
}

/// Итог команды: ошибка — это внятная строка в stderr и ненулевой код, а не
/// паника со стектрейсом. Установка узла ходит по чужой машине, и половина
/// причин отказа (ключи, архитектура, systemd) требует текста, а не бэктрейса.
fn finish(res: Result<(), String>) {
    if let Err(e) = res {
        eprintln!("✗ {e}");
        std::process::exit(1);
    }
}

fn die(msg: &str) -> ! {
    eprintln!("✗ {msg}\n\n{REMOTE_USAGE}");
    std::process::exit(1);
}

#[derive(Debug)]
enum RemoteCommand {
    Add {
        name: String,
        connection: install::remote::Connection,
        dir: Option<String>,
        tcp: Option<u16>,
    },
    Status(String),
}

/// Parse and validate before the installer performs any remote operation.
fn parse_remote(args: &[String]) -> Result<RemoteCommand, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut dir: Option<String> = None;
    let mut tcp: Option<u16> = Some(install::remote::DEFAULT_TCP_PORT);
    let mut connection = install::remote::Connection::default();
    let mut no_tcp = false;
    let mut has_options = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with('-') {
            positional.push(arg);
            continue;
        }
        has_options = true;
        if arg == "--no-tcp" {
            no_tcp = true;
            tcp = None;
            continue;
        }
        let (flag, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)));
        if !matches!(flag, "--dir" | "--tcp" | "--transport" | "--proxy" | "--cluster" | "--ssh-config" | "--run-as-user") {
            return Err(format!("не знаю ключ {flag}"));
        }
        let value = inline.or_else(|| rest.next().map(String::as_str))
            .filter(|value| !value.is_empty() && !value.starts_with('-'))
            .ok_or_else(|| format!("{flag} ждёт значение"))?;
        match flag {
            "--dir" => dir = Some(value.into()),
            "--tcp" => tcp = Some(value.parse::<u16>().ok().filter(|port| *port > 0)
                .ok_or_else(|| "--tcp ждёт номер порта от 1 до 65535".to_string())?),
            "--transport" if matches!(value, "ssh" | "teleport") => connection.transport = value.into(),
            "--transport" => return Err("--transport ждёт ssh или teleport".into()),
            "--proxy" => connection.teleport_proxy = Some(value.into()),
            "--cluster" => connection.teleport_cluster = Some(value.into()),
            "--ssh-config" => connection.ssh_config_file = Some(value.into()),
            "--run-as-user" => connection.run_as_user = Some(value.into()),
            _ => unreachable!(),
        }
    }
    match positional.split_first() {
        Some((&"add", [name, host])) => {
            connection.ssh_host = (*host).into();
            if connection.transport == "teleport" {
                if no_tcp {
                    return Err("--no-tcp несовместим с Teleport: нужен TCP-порт узла (обычно 7717)".into());
                }
                connection.node_tcp_port = tcp;
            } else if connection.teleport_proxy.is_some() || connection.teleport_cluster.is_some() {
                return Err("--proxy и --cluster требуют --transport teleport".into());
            }
            connection.validate()?;
            Ok(RemoteCommand::Add { name: (*name).into(), connection, dir, tcp })
        }
        Some((&"status", [_])) if has_options => Err("remote status использует сохранённые параметры подключения и ждёт только имя узла".into()),
        Some((&"status", [name])) => Ok(RemoteCommand::Status((*name).into())),
        Some((&"add", _)) => Err("remote add ждёт ровно два аргумента: <имя> <ssh-хост>".into()),
        Some((&"status", _)) => Err("remote status ждёт одно имя узла".into()),
        _ => Err("Нужна команда remote add или remote status".into()),
    }
}

/// `jarvis-setup remote …` — узлы на других машинах.
fn remote(args: &[String]) {
    match parse_remote(args).unwrap_or_else(|error| die(&error)) {
        RemoteCommand::Add { name, connection, dir, tcp } => {
            finish(install::remote::add_connection(&print_step, &name, &connection, dir.as_deref(), tcp))
        }
        RemoteCommand::Status(name) => finish(install::remote::status(&print_step, &name)),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => {
            // прокси для скачивания моделей — из env (HTTPS_PROXY) или флага
            let proxy = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("HTTP_PROXY").ok());
            install::install(&print_step, proxy.as_deref());
            println!("\nГотово. Активные сессии Claude Code перезапусти — хуки берутся");
            println!("снапшотом на старте сессии. Шим в текущем шелле: exec zsh (или новая вкладка).");
        }
        Some("uninstall") => install::uninstall(&print_step),
        Some("status") => print!("{}", install::status_report()),
        Some("repair") => {
            // Только интеграция агентов (хуки + шим), без Silero/STT/моделей.
            install::repair(&print_step);
            finish(repair_instances(None));
            println!("\nИнтеграция починена. Если codex-шим доустановлен — перезапусти");
            println!("шелл (exec zsh) или открой новую вкладку, чтобы `codex` пошёл через Jarvis.");
        }
        Some("remote") => remote(&args[1..]),
        Some("instances") => {
            let dir = data_dir();
            match args.get(1).map(String::as_str) {
                Some("list") if args.len() == 2 => match agent_instances::load_registry(&dir) {
                    Ok(registry) => println!("{}", serde_json::to_string_pretty(&registry).unwrap()),
                    Err(error) => finish(Err(error)),
                },
                Some("repair") if args.len() <= 3 => finish(repair_instances(args.get(2).cloned().map(|id| vec![id]))),
                _ => finish(Err("Использование: jarvis-setup instances list | repair [id]".into())),
            }
        },
        _ => {
            eprint!("{USAGE}");
            std::process::exit(1);
        }
    }
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os("JARVIS_DIR").filter(|s| !s.is_empty()).map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".jarvis"))
}

fn repair_instances(selected: Option<Vec<String>>) -> Result<(), String> {
    let registry = agent_instances::load_registry(&data_dir())?;
    let health = install::repair_hooks_for_instances(selected.as_deref(), &print_step)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e|e.to_string())?;
    let mut failures = Vec::new();
    for mut item in health.into_iter().filter(|item| item.enabled && selected.as_ref().map_or(true, |ids| ids.contains(&item.instance_id))) {
        let result = if !item.rules_installed { Err(item.errors.join("; ")) } else {
            registry.launch_spec(Some(&item.instance_id)).and_then(|spec|
                runtime.block_on(codex_hooks::reconcile(&spec.program,&spec.codex_home,std::path::Path::new(&item.hook_bin),true)))
        };
        match result {
            Ok(response) => { install::apply_runtime_hook_health(&mut item,&response); println!("{}: правила установлены, доверие Codex — {}",item.label,item.trust_status); },
            Err(error) => failures.push(format!("{}: {error}",item.label)),
        }
    }
    if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
}

#[cfg(test)]
mod remote_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<RemoteCommand, String> {
        parse_remote(&args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn teleport_keeps_explicit_identity_and_noninteractive_auth_flags() {
        let RemoteCommand::Add { name, connection, dir, tcp } = parse(&[
            "add", "runner", "root@node-id", "--transport", "teleport",
            "--proxy", "proxy.example:443", "--cluster=leaf.example",
            "--run-as-user", "coder", "--dir", "/srv/jarvis workspace",
        ]).unwrap() else { panic!("expected add") };
        assert_eq!(name, "runner");
        assert_eq!(tcp, Some(7717));
        assert_eq!(connection.node_tcp_port, tcp);
        assert_eq!(dir.as_deref(), Some("/srv/jarvis workspace"));
        assert_eq!(connection.run_as_user.as_deref(), Some("coder"));
        let command = connection.command_with_script("true").unwrap();
        assert!(command.get_program().to_string_lossy().ends_with("tsh"));
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        for flag in ["--no-forward-agent", "--no-relogin", "--request-mode=off", "--proxy=proxy.example:443", "--cluster=leaf.example"] {
            assert!(args.iter().any(|arg| arg == flag), "missing {flag}");
        }
        assert_eq!(args[args.len() - 2], "root@node-id");
        assert!(args.last().unwrap().starts_with("sudo -n -H -u 'coder' -- /bin/sh -c "));

        let RemoteCommand::Add { connection, tcp, .. } = parse(&[
            "--transport=teleport", "add", "runner", "root@node-id", "--tcp=8817",
        ]).unwrap() else { panic!("expected add") };
        assert_eq!(tcp, Some(8817));
        assert_eq!(connection.node_tcp_port, tcp);
    }

    #[test]
    fn ssh_defaults_config_and_status_keep_existing_routing() {
        let RemoteCommand::Add { connection, tcp, .. } = parse(&[
            "add", "vm", "ssh-alias",
        ]).unwrap() else { panic!("expected add") };
        assert_eq!(tcp, Some(install::remote::DEFAULT_TCP_PORT));
        assert_eq!(connection, install::remote::Connection::ssh("ssh-alias"));

        let RemoteCommand::Add { connection, tcp, .. } = parse(&[
            "add", "vm", "ssh-alias", "--transport=ssh", "--no-tcp",
            "--ssh-config", "/tmp/space path/config", "--run-as-user=coder",
        ]).unwrap() else { panic!("expected add") };
        assert_eq!(tcp, None);
        assert_eq!(connection.node_tcp_port, None);
        let command = connection.command().unwrap();
        assert_eq!(command.get_program(), "ssh");
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..2], &["-F", "/tmp/space path/config"]);
        assert_eq!(args.last().unwrap(), "ssh-alias");
        assert!(matches!(parse(&["status", "vm"]).unwrap(), RemoteCommand::Status(name) if name == "vm"));
    }

    #[test]
    fn invalid_connections_are_rejected_before_installation() {
        for extra in [
            vec!["--transport", "teleport", "--no-tcp"],
            vec!["--no-tcp", "--tcp=7717", "--transport=teleport"],
            vec!["--transport=teleport", "--ssh-config=/tmp/config"],
            vec!["--proxy=proxy.example"],
            vec!["--transport=ssh", "--cluster=leaf.example"],
            vec!["--transport=other"],
            vec!["--transport=teleport", "--proxy=https://proxy.example"],
            vec!["--transport=teleport", "--cluster=leaf;command"],
            vec!["--ssh-config=relative/config"],
            vec!["--run-as-user=coder;command"],
            vec!["--tcp=0"],
            vec!["--tcp=65536"],
            vec!["--proxy"],
            vec!["--transport", "--proxy=proxy.example"],
        ] {
            let mut args = vec!["add", "vm", "user@node"];
            args.extend(extra);
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
        assert!(parse(&["add", "vm", "user@node;command"]).is_err());
        assert!(parse(&["status", "vm", "--transport=teleport"]).is_err());
    }
}
