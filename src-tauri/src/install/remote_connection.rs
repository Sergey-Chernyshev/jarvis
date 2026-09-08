//! Explicit SSH transport shared by setup and the desktop poller. No shell
//! command is accepted as a host and account/approval settings are never changed.
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Connection {
    pub ssh_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_config_file: Option<String>,
    /// Empty/ssh uses OpenSSH; teleport uses the existing tsh authentication.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub teleport_proxy: Option<String>,
    /// Bind the selected leaf cluster instead of following tsh's active profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub teleport_cluster: Option<String>,
    /// Teleport forwards TCP, never assumes Unix-socket forwarding support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_tcp_port: Option<u16>,
    /// Optional account owning the remote agents when SSH authenticates as an
    /// administrator. Never guessed from an authentication file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_as_user: Option<String>,
}
impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.ssh_host.fmt(f) }
}
impl Connection {
    pub fn ssh(host: &str) -> Self { Self { ssh_host: host.trim().into(), ..Self::default() } }
    pub fn validate(&self) -> Result<(), String> {
        if self.ssh_host.is_empty() || self.ssh_host.starts_with('-') || self.ssh_host.chars().any(|c| !(c.is_ascii_alphanumeric() || ".-_@:%[]".contains(c))) {
            return Err("Нужен SSH-хост или алиас, без команд и параметров".into());
        }
        if !matches!(self.transport.as_str(), "" | "ssh" | "teleport") { return Err("Неизвестный SSH-транспорт".into()); }
        if let Some(path) = &self.ssh_config_file {
            if !Path::new(path).is_absolute() || path.chars().any(char::is_control) { return Err("SSH config должен быть абсолютным путём".into()); }
            if self.transport == "teleport" { return Err("Для Teleport укажи proxy; SSH config применяется только к OpenSSH".into()); }
        }
        if let Some(proxy) = &self.teleport_proxy {
            validate_teleport_proxy(proxy)?;
        }
        if let Some(cluster) = &self.teleport_cluster {
            if cluster.is_empty() || cluster.starts_with('-') || cluster.chars().any(|c| !(c.is_ascii_alphanumeric() || ".-_".contains(c))) {
                return Err("Некорректное имя кластера Teleport".into());
            }
        }
        if let Some(user) = &self.run_as_user {
            if user.is_empty() || user.starts_with('-') || user.chars().any(|c| !(c.is_ascii_alphanumeric() || "_-".contains(c))) { return Err("Некорректный пользователь узла".into()); }
        }
        if self.node_tcp_port == Some(0) { return Err("TCP-порт узла должен быть больше нуля".into()); }
        Ok(())
    }
    fn base(&self) -> Command {
        if self.transport == "teleport" {
            // Finder's PATH may omit Homebrew. Do not fall back to plain ssh.
            let path = ["/opt/homebrew/bin/tsh", "/usr/local/bin/tsh"].into_iter().find(|p| Path::new(p).is_file()).unwrap_or("tsh");
            let mut cmd = Command::new(path);
            cmd.args(["ssh", "--no-forward-agent", "--no-relogin", "--request-mode=off"]);
            if let Some(proxy) = &self.teleport_proxy { cmd.arg(format!("--proxy={proxy}")); }
            if let Some(cluster) = &self.teleport_cluster { cmd.arg(format!("--cluster={cluster}")); }
            cmd
        } else {
            let mut cmd = Command::new("ssh");
            if let Some(path) = &self.ssh_config_file { cmd.args(["-F", path]); }
            cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ForwardAgent=no", "-o", "ForkAfterAuthentication=no"]);
            cmd
        }
    }
    pub fn command(&self) -> Result<Command, String> {
        self.validate()?;
        let mut cmd = self.base(); cmd.arg(&self.ssh_host); Ok(cmd)
    }
    pub fn command_with_script(&self, script: &str) -> Result<Command, String> {
        let mut cmd = self.command()?;
        // ssh/tsh hands its command to the account's login shell. These scripts
        // use POSIX syntax: zsh rejects unmatched globs and fish cannot parse
        // the loops/assignments. Let that shell launch sh, never parse the body.
        // -c also leaves stdin available for file uploads and other payloads.
        let posix = posix_script(script);
        let command = if let Some(user) = &self.run_as_user {
            format!("sudo -n -H -u '{user}' -- {posix}")
        } else { posix };
        cmd.arg(command); Ok(cmd)
    }
    pub fn tunnel(&self, port: u16, socket: &str) -> Result<Command, String> {
        self.validate()?;
        let endpoint = match self.node_tcp_port {
            Some(remote) => format!("127.0.0.1:{remote}"),
            None if self.transport == "teleport" => return Err("Для Teleport нужен TCP-порт узла (обычно 7717)".into()),
            None => socket.to_string(),
        };
        let mut cmd = self.base();
        cmd.args(["-N", "-L", &format!("127.0.0.1:{port}:{endpoint}")]);
        if self.transport != "teleport" {
            // Lima's config may hand the forward to an existing background
            // ControlMaster and exit successfully. Jarvis owns the tunnel's
            // child lifetime, so it must remain a foreground process.
            cmd.args(["-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ForkAfterAuthentication=no",
                "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]);
        }
        cmd.arg(&self.ssh_host); Ok(cmd)
    }
}

pub fn posix_script(script: &str) -> String {
    format!("/bin/sh -c {}", login_shell_quote(script))
}

// Keep backslashes outside quoted segments: fish treats \\ and \' specially
// inside single quotes, unlike POSIX shells. Both accept escaped characters
// outside quotes, so this envelope works without changing the script's bytes.
fn login_shell_quote(script: &str) -> String {
    let mut quoted = String::from("'");
    for c in script.chars() {
        match c {
            '\'' => quoted.push_str("'\\''"),
            '\\' => quoted.push_str("'\\\\'"),
            _ => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

pub fn validate_teleport_proxy(proxy: &str) -> Result<(), String> {
    let valid = if let Some(rest) = proxy.strip_prefix('[') {
        rest.split_once(']').is_some_and(|(host, suffix)| {
            host.parse::<std::net::Ipv6Addr>().is_ok()
                && (suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_proxy_port))
        })
    } else {
        let (host, port) = proxy.split_once(':').map_or((proxy, None), |(h, p)| (h, Some(p)));
        !host.is_empty() && !host.starts_with('-') && host.len() <= 253
            && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
            && port.map_or(true, valid_proxy_port)
    };
    if valid { Ok(()) } else { Err("Укажи адрес Teleport proxy, например teleport.example.com:443, без https:// и пути".into()) }
}
fn valid_proxy_port(port: &str) -> bool {
    !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && port.parse::<u16>().is_ok_and(|p| p > 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(cmd: Command) -> Vec<String> { cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect() }
    #[test] fn open_ssh_config_is_an_argument_not_a_shell_fragment() {
        let c = Connection { ssh_config_file: Some("/tmp/a b/ssh.config".into()), ..Connection::ssh("vm") };
        let a = args(c.tunnel(1234, "/home/me/.jarvis/node.sock").unwrap());
        assert!(a.windows(2).any(|v| v == ["-F", "/tmp/a b/ssh.config"]));
        assert!(a.contains(&"127.0.0.1:1234:/home/me/.jarvis/node.sock".into()));
        assert!(a.contains(&"ControlPath=none".into()));
        assert!(a.contains(&"ForkAfterAuthentication=no".into()));
        assert_eq!(a.last().unwrap(), "vm");
    }
    #[test] fn teleport_never_falls_back_to_plain_ssh_or_unix_forward() {
        let mut c = Connection { transport: "teleport".into(), teleport_proxy: Some("proxy.example".into()), teleport_cluster: Some("leaf.example".into()), ..Connection::ssh("me@node") };
        assert!(c.tunnel(1234, "/node.sock").is_err());
        c.node_tcp_port = Some(7717);
        let cmd = c.tunnel(1234, "/node.sock").unwrap();
        assert!(cmd.get_program().to_string_lossy().ends_with("tsh"));
        let a = args(cmd);
        assert!(a.contains(&"127.0.0.1:1234:127.0.0.1:7717".into()));
        assert!(a.contains(&"--no-forward-agent".into()));
        assert!(a.contains(&"--no-relogin".into()));
        assert!(a.contains(&"--cluster=leaf.example".into()));
        assert!(args(c.command_with_script("true").unwrap()).contains(&"--cluster=leaf.example".into()));
    }
    #[test] fn proxy_requires_an_address_and_valid_port() {
        for proxy in ["proxy.example", "proxy.example:443", "localhost:3080", "[::1]:443"] { assert!(validate_teleport_proxy(proxy).is_ok(), "{proxy}"); }
        for proxy in ["", "-bad", "https://example.com", "user@proxy", "proxy/path", "proxy:0", "proxy:65536", "proxy:bad", "proxy\n", "[::1]:22:33"] { assert!(validate_teleport_proxy(proxy).is_err(), "{proxy}"); }
    }
    #[test] fn command_injection_and_unknown_transports_are_rejected() {
        for host in ["", "-oProxyCommand=bad", "vm -p 22", "vm\nother", "$(bad)", "vm;bad"] { assert!(Connection::ssh(host).command().is_err()); }
        let c = Connection { transport: "custom-shell".into(), ..Connection::ssh("vm") };
        assert!(c.command().is_err());
    }
    #[test] fn explicit_owner_is_noninteractive_and_keeps_script_one_argument() {
        let c = Connection { run_as_user: Some("hermes".into()), ..Connection::ssh("root@node") };
        let a = args(c.command_with_script("printf '%s' \"$HOME\"").unwrap());
        assert_eq!(a.last().unwrap(), "sudo -n -H -u 'hermes' -- /bin/sh -c 'printf '\\''%s'\\'' \"$HOME\"'");
        assert!(Connection { run_as_user: Some("hermes;bad".into()), ..c }.validate().is_err());
    }

    #[test] fn posix_scripts_survive_zsh_login_shell_and_keep_stdin_for_data() {
        use std::io::Write;
        use std::process::Stdio;
        let path = std::env::temp_dir().join(format!("jarvis-shell-glob-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&path).unwrap();
        let script = "for p in \"$JARVIS_QA_HOME\"/.codex-* \"$JARVIS_QA_HOME\"/.claude-*; do [ -d \"$p\" ] && printf '%s\\n' \"$p\"; done\nWANT='tmux curl'\nfor b in $WANT; do printf '[%s]' \"$b\"; done\nprintf '%s\\n' \"quoted ' value\"\ncat";
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            if !Path::new(shell).is_file() { continue; }
            let command = Connection::ssh("unused").command_with_script(script).unwrap();
            let mut child = Command::new(shell).args(["-c"]).arg(command.get_args().last().unwrap())
                .env("JARVIS_QA_HOME", &path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(b"payload ' $() *\n\0end").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{shell}: {}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout, b"[tmux][curl]quoted ' value\npayload ' $() *\n\0end", "{shell}");
        }
        std::fs::remove_dir(path).unwrap();
    }
}
