use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

pub const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

impl Drop for CommandSpec {
    fn drop(&mut self) {
        if let Some(stdin) = &mut self.stdin {
            stdin.zeroize();
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl fmt::Debug for CommandResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandResult")
            .field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

impl Drop for CommandResult {
    fn drop(&mut self) {
        self.stdout.zeroize();
        self.stderr.zeroize();
    }
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

    fn run_with_timeout(
        &self,
        spec: &CommandSpec,
        _timeout: Duration,
    ) -> Result<CommandResult, String> {
        self.run(spec)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandResult, String> {
        run_system(spec, None)
    }

    fn run_with_timeout(
        &self,
        spec: &CommandSpec,
        timeout: Duration,
    ) -> Result<CommandResult, String> {
        run_system(spec, Some(timeout))
    }
}

fn run_system(spec: &CommandSpec, timeout: Option<Duration>) -> Result<CommandResult, String> {
    if !spec.program.is_absolute() {
        return Err("program path должен быть absolute".into());
    }
    let deadline = timeout
        .map(|timeout| {
            Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| "runtime tool timeout имеет unsafe значение".to_string())
        })
        .transpose()?;
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
    if timeout.is_some() {
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|err| {
        format!(
            "не запустить runtime tool {}: {err}",
            spec.program.display()
        )
    })?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            return Err("runtime tool stdout недоступен".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            return Err("runtime tool stderr недоступен".into());
        }
    };
    let stdout_worker = thread::spawn(move || read_bounded(stdout));
    let stderr_worker = thread::spawn(move || read_bounded(stderr));
    let stdin_worker = match (&spec.stdin, child.stdin.take()) {
        (Some(input), Some(mut stdin)) => {
            let mut input = input.clone();
            Some(thread::spawn(move || {
                let result = stdin
                    .write_all(&input)
                    .map_err(|err| format!("не передать runtime tool stdin: {err}"));
                input.zeroize();
                result
            }))
        }
        (Some(_), None) => {
            terminate_process_group(&mut child);
            return Err("runtime tool stdin недоступен".into());
        }
        (None, _) => None,
    };
    let wait = match deadline {
        None => child
            .wait()
            .map(WaitOutcome::Exited)
            .map_err(|err| format!("не дождаться runtime tool: {err}")),
        Some(deadline) => wait_until(&mut child, deadline),
    };
    let wait = match wait {
        Ok(wait) => wait,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = join_stdin(stdin_worker);
            let _ = join_output(stdout_worker, "stdout");
            let _ = join_output(stderr_worker, "stderr");
            return Err(error);
        }
    };
    if matches!(wait, WaitOutcome::TimedOut) {
        terminate_process_group(&mut child);
    }
    let stdin_result = join_stdin(stdin_worker);
    let stdout = join_output(stdout_worker, "stdout");
    let stderr = join_output(stderr_worker, "stderr");
    let (mut stdout, mut stderr) = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Ok(mut stdout), Err(error)) => {
            stdout.zeroize();
            return Err(error);
        }
        (Err(error), Ok(mut stderr)) => {
            stderr.zeroize();
            return Err(error);
        }
        (Err(error), Err(_)) => return Err(error),
    };
    if matches!(wait, WaitOutcome::TimedOut) {
        stdout.zeroize();
        stderr.zeroize();
        return Err(format!(
            "runtime tool timeout after {} ms",
            timeout.unwrap().as_millis()
        ));
    }
    if let Err(error) = stdin_result {
        stdout.zeroize();
        stderr.zeroize();
        return Err(error);
    }
    let status = match wait {
        WaitOutcome::Exited(status) => status,
        WaitOutcome::TimedOut => unreachable!(),
    };
    Ok(CommandResult {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

enum WaitOutcome {
    Exited(ExitStatus),
    TimedOut,
}

fn wait_until(child: &mut Child, deadline: Instant) -> Result<WaitOutcome, String> {
    loop {
        match child
            .try_wait()
            .map_err(|err| format!("не проверить runtime tool: {err}"))?
        {
            Some(status) => return Ok(WaitOutcome::Exited(status)),
            None if Instant::now() >= deadline => return Ok(WaitOutcome::TimedOut),
            None => {
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
        }
    }
}

fn terminate_process_group(child: &mut Child) {
    let process_group = child.id() as libc::pid_t;
    let group_killed = unsafe { libc::killpg(process_group, libc::SIGKILL) } == 0;
    if !group_killed {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn read_bounded(mut stream: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error) => {
                output.zeroize();
                chunk.zeroize();
                return Err(format!("не прочитать runtime tool output: {error}"));
            }
        };
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
    }
    chunk.zeroize();
    Ok(output)
}

fn join_stdin(worker: Option<thread::JoinHandle<Result<(), String>>>) -> Result<(), String> {
    match worker {
        Some(worker) => worker
            .join()
            .map_err(|_| "runtime tool stdin worker завершился аварийно".to_string())?,
        None => Ok(()),
    }
}

fn join_output(
    worker: thread::JoinHandle<Result<Vec<u8>, String>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    worker
        .join()
        .map_err(|_| format!("runtime tool {stream} worker завершился аварийно"))?
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

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

    #[test]
    fn command_debug_redacts_stdin_and_environment_values() {
        let spec = CommandSpec {
            program: PathBuf::from("/synthetic/bin/tool"),
            args: vec!["run".into()],
            cwd: None,
            env: BTreeMap::from([("SYNTHETIC_ENV".into(), "SYNTHETIC_PRIVATE_VALUE".into())]),
            stdin: Some(b"SYNTHETIC_STDIN_VALUE".to_vec()),
        };

        let debug = format!("{spec:?}");

        assert!(debug.contains("stdin_bytes"));
        assert!(debug.contains("SYNTHETIC_ENV"));
        assert!(!debug.contains("SYNTHETIC_PRIVATE_VALUE"));
        assert!(!debug.contains("SYNTHETIC_STDIN_VALUE"));
    }

    #[test]
    fn system_runner_timeout_kills_a_hung_child_with_bounded_wait() {
        let spec = CommandSpec {
            program: PathBuf::from("/bin/sleep"),
            args: vec!["5".into()],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
        };
        let started = Instant::now();

        let error = SystemRunner
            .run_with_timeout(&spec, Duration::from_millis(100))
            .unwrap_err();

        assert!(error.contains("timeout"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout must bound the child wait"
        );
    }

    #[test]
    fn timed_runner_drains_large_output_before_child_exit() {
        let spec = CommandSpec {
            program: PathBuf::from("/bin/dd"),
            args: vec!["if=/dev/zero".into(), "bs=131072".into(), "count=16".into()],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
        };

        let result = SystemRunner
            .run_with_timeout(&spec, Duration::from_secs(2))
            .expect("finite output larger than a pipe buffer must not look hung");

        assert_eq!(result.status, 0);
        assert_eq!(result.stdout.len(), MAX_COMMAND_OUTPUT_BYTES);
    }

    #[test]
    fn timeout_kills_and_reaps_the_spawned_process_group() {
        let pid_file = std::env::temp_dir().join(format!(
            "jarvis-agent-vm-timeout-child-{}.pid",
            uuid::Uuid::new_v4()
        ));
        let spec = CommandSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                "sleep 30 & child=$!; echo \"$child\" > \"$1\"; wait".into(),
                "runner-timeout-test".into(),
                pid_file.to_string_lossy().into_owned(),
            ],
            cwd: None,
            env: BTreeMap::from([("PATH".into(), "/bin:/usr/bin".into())]),
            stdin: None,
        };

        let error = SystemRunner
            .run_with_timeout(&spec, Duration::from_millis(150))
            .unwrap_err();
        let child_pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let survived = wait_for_process_exit(child_pid, Duration::from_millis(500)).is_err();
        if survived {
            unsafe {
                libc::kill(child_pid, libc::SIGKILL);
            }
        }
        fs::remove_file(pid_file).unwrap();

        assert!(error.contains("timeout"), "{error}");
        assert!(
            !survived,
            "timeout left descendant process {child_pid} alive"
        );
    }

    fn wait_for_process_exit(pid: i32, timeout: Duration) -> Result<(), ()> {
        let deadline = Instant::now() + timeout;
        while process_exists(pid) {
            if Instant::now() >= deadline {
                return Err(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn process_exists(pid: i32) -> bool {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}
