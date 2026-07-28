use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandResult {
    pub fn success_or_error(self, operation: &str) -> Result<Self, String> {
        if self.status == 0 {
            Ok(self)
        } else {
            // stderr намеренно не включается: CLI может отразить credential,
            // proxy URL или чужой config. Для UI достаточно операции и exit code.
            Err(format!("{operation} завершился с кодом {}", self.status))
        }
    }

    pub fn stdout_text(&self, operation: &str) -> Result<&str, String> {
        std::str::from_utf8(&self.stdout).map_err(|_| format!("{operation} вернул не-UTF-8 output"))
    }
}

pub trait CommandRunner: Clone + Send + Sync + 'static {
    fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String> {
        if !spec.program.is_absolute() {
            return Err("program path должен быть absolute".into());
        }
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .env_clear()
            .envs(&spec.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        if spec.stdin.is_some() {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        let mut child = command.spawn().map_err(|err| {
            format!(
                "не запустить runtime tool {}: {err}",
                spec.program.display()
            )
        })?;
        if let Some(input) = &spec.stdin {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "runtime tool stdin недоступен".to_string())?;
            stdin
                .write_all(input)
                .map_err(|err| format!("не передать runtime tool stdin: {err}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("не дождаться runtime tool: {err}"))?;
        Ok(CommandResult {
            status: output.status.code().unwrap_or(-1),
            stdout: bounded(output.stdout),
            stderr: bounded(output.stderr),
        })
    }
}

fn bounded(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() > MAX_COMMAND_OUTPUT_BYTES {
        bytes.truncate(MAX_COMMAND_OUTPUT_BYTES);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn system_runner_rejects_relative_programs_before_spawn() {
        let spec = CommandSpec {
            program: PathBuf::from("avm"),
            args: vec!["list".into()],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
        };

        let err = SystemRunner.run(&spec).unwrap_err();

        assert!(
            err.contains("absolute"),
            "program boundary is explicit: {err}"
        );
    }

    #[test]
    fn command_failure_never_embeds_stderr_that_may_contain_credentials() {
        let result = CommandResult {
            status: 7,
            stdout: Vec::new(),
            stderr: b"synthetic credential payload".to_vec(),
        };

        let err = result.success_or_error("avm start").unwrap_err();

        assert!(err.contains("кодом 7"));
        assert!(!err.contains("credential"));
        assert!(!err.contains("payload"));
    }
}
