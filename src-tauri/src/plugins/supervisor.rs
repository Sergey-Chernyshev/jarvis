use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde::Serialize;

use crate::plugins::manifest::PROTOCOL_VERSION;
use crate::plugins::protocol::RegisterRequest;

pub const HANDSHAKE_TIMEOUT_MS: i64 = 10_000;

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub plugin_id: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub socket: PathBuf,
    pub token: String,
    pub protocol_version: u32,
}

pub trait ProcessSpawner: Send + Sync {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ManagedChild>, String>;
}

pub trait ManagedChild: Send {
    fn id(&self) -> u32;
    fn try_wait(&mut self) -> Result<Option<i32>, String>;
    fn kill(&mut self) -> Result<(), String>;
}

pub struct SystemSpawner;

struct SystemChild {
    child: Child,
}

impl ManagedChild for SystemChild {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        self.child
            .try_wait()
            .map(|status| status.map(|status| status.code().unwrap_or(-1)))
            .map_err(|err| format!("не проверить plugin process: {err}"))
    }

    fn kill(&mut self) -> Result<(), String> {
        if self
            .child
            .try_wait()
            .map_err(|err| format!("не проверить plugin process перед stop: {err}"))?
            .is_some()
        {
            return Ok(());
        }
        self.child
            .kill()
            .map_err(|err| format!("не остановить plugin process: {err}"))?;
        let _ = self.child.wait();
        Ok(())
    }
}

fn pipe_to_log(
    reader: impl Read + Send + 'static,
    plugin_id: String,
    channel: &'static str,
    token: String,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let line = line.replace(&token, "[REDACTED]");
            if !line.trim().is_empty() {
                crate::log::line(&format!("[plugin:{plugin_id}] {channel}: {line}"));
            }
        }
    });
}

impl ProcessSpawner for SystemSpawner {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ManagedChild>, String> {
        let mut child = Command::new(&spec.executable)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env("JARVIS_SOCKET", &spec.socket)
            .env("JARVIS_PLUGIN_ID", &spec.plugin_id)
            .env("JARVIS_PLUGIN_TOKEN", &spec.token)
            .env("JARVIS_PLUGIN_PROTOCOL", spec.protocol_version.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                format!(
                    "не запустить plugin '{}' ({}): {err}",
                    spec.plugin_id,
                    spec.executable.display()
                )
            })?;

        if let Some(stdout) = child.stdout.take() {
            pipe_to_log(stdout, spec.plugin_id.clone(), "stdout", spec.token.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            pipe_to_log(stderr, spec.plugin_id.clone(), "stderr", spec.token.clone());
        }
        Ok(Box::new(SystemChild { child }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    Stopped,
    Starting,
    Running,
    Backoff,
    Error,
    Incompatible,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Stopped => "stopped",
            Lifecycle::Starting => "starting",
            Lifecycle::Running => "running",
            Lifecycle::Backoff => "backoff",
            Lifecycle::Error => "error",
            Lifecycle::Incompatible => "incompatible",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Runtime {
    pub lifecycle: Lifecycle,
    pub pid: Option<u32>,
    pub started_at_ms: Option<i64>,
    pub registered_at_ms: Option<i64>,
    pub handshake_deadline_ms: Option<i64>,
    pub retry_at_ms: Option<i64>,
    pub restart_attempt: u32,
    pub last_error: Option<String>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            lifecycle: Lifecycle::Stopped,
            pid: None,
            started_at_ms: None,
            registered_at_ms: None,
            handshake_deadline_ms: None,
            retry_at_ms: None,
            restart_attempt: 0,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    Conflict(String),
    Incompatible { received: u32 },
}

impl RegistrationError {
    pub fn code(&self) -> &'static str {
        match self {
            RegistrationError::Conflict(_) => "registration_conflict",
            RegistrationError::Incompatible { .. } => "incompatible_protocol",
        }
    }
}

impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistrationError::Conflict(message) => f.write_str(message),
            RegistrationError::Incompatible { received } => write!(
                f,
                "несовместимый protocolVersion {received}, поддерживается {PROTOCOL_VERSION}"
            ),
        }
    }
}

impl Runtime {
    pub fn on_spawned(&mut self, pid: u32, now_ms: i64) {
        self.lifecycle = Lifecycle::Starting;
        self.pid = Some(pid);
        self.started_at_ms = Some(now_ms);
        self.registered_at_ms = None;
        self.handshake_deadline_ms = Some(now_ms + HANDSHAKE_TIMEOUT_MS);
        self.retry_at_ms = None;
        self.last_error = None;
    }

    pub fn register(
        &mut self,
        request: &RegisterRequest,
        now_ms: i64,
    ) -> Result<(), RegistrationError> {
        if request.protocol_version != PROTOCOL_VERSION {
            self.lifecycle = Lifecycle::Incompatible;
            self.last_error = Some(format!(
                "несовместимый protocolVersion {}, поддерживается {PROTOCOL_VERSION}",
                request.protocol_version
            ));
            return Err(RegistrationError::Incompatible {
                received: request.protocol_version,
            });
        }

        let expected = self.pid.ok_or_else(|| {
            RegistrationError::Conflict("для плагина нет ожидаемого процесса".into())
        })?;
        if request.pid != expected {
            return Err(RegistrationError::Conflict(format!(
                "pid handshake {} не совпадает с ожидаемым {expected}",
                request.pid
            )));
        }
        if self.lifecycle == Lifecycle::Running {
            return Ok(());
        }
        if self.lifecycle != Lifecycle::Starting {
            return Err(RegistrationError::Conflict(format!(
                "регистрация недоступна в состоянии {}",
                self.lifecycle.as_str()
            )));
        }

        self.lifecycle = Lifecycle::Running;
        self.registered_at_ms = Some(now_ms);
        self.handshake_deadline_ms = None;
        self.retry_at_ms = None;
        self.restart_attempt = 0;
        self.last_error = None;
        Ok(())
    }

    pub fn handshake_timed_out(&self, now_ms: i64) -> bool {
        self.lifecycle == Lifecycle::Starting
            && self
                .handshake_deadline_ms
                .is_some_and(|deadline| now_ms >= deadline)
    }

    pub fn on_failure(&mut self, now_ms: i64, error: impl Into<String>) -> i64 {
        let delay_seconds = if self.restart_attempt >= 5 {
            30
        } else {
            1_i64 << self.restart_attempt
        };
        self.restart_attempt = self.restart_attempt.saturating_add(1);
        let retry_at = now_ms + delay_seconds * 1_000;
        self.lifecycle = Lifecycle::Backoff;
        self.pid = None;
        self.started_at_ms = None;
        self.registered_at_ms = None;
        self.handshake_deadline_ms = None;
        self.retry_at_ms = Some(retry_at);
        self.last_error = Some(error.into());
        retry_at
    }

    pub fn retry_due(&self, now_ms: i64) -> bool {
        self.lifecycle == Lifecycle::Backoff
            && self.retry_at_ms.is_some_and(|retry_at| now_ms >= retry_at)
    }

    pub fn disable(&mut self) {
        self.lifecycle = Lifecycle::Stopped;
        self.pid = None;
        self.started_at_ms = None;
        self.registered_at_ms = None;
        self.handshake_deadline_ms = None;
        self.retry_at_ms = None;
        self.restart_attempt = 0;
        self.last_error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_accepts_matching_plugin_pid_and_protocol() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);

        runtime
            .register(
                &RegisterRequest {
                    protocol_version: PROTOCOL_VERSION,
                    pid: 41,
                },
                1_050,
            )
            .unwrap();

        assert_eq!(runtime.lifecycle, Lifecycle::Running);
        assert_eq!(runtime.pid, Some(41));
        assert_eq!(runtime.registered_at_ms, Some(1_050));
        assert_eq!(runtime.restart_attempt, 0);
        assert!(runtime.last_error.is_none());
    }

    #[test]
    fn register_is_idempotent_for_same_running_pid() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);
        let request = RegisterRequest {
            protocol_version: PROTOCOL_VERSION,
            pid: 41,
        };
        runtime.register(&request, 1_050).unwrap();

        runtime.register(&request, 2_000).unwrap();

        assert_eq!(runtime.lifecycle, Lifecycle::Running);
        assert_eq!(runtime.pid, Some(41));
        assert_eq!(
            runtime.registered_at_ms,
            Some(1_050),
            "retry не переписывает первый successful handshake"
        );
    }

    #[test]
    fn register_rejects_wrong_plugin_pid_or_protocol() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);

        let pid_error = runtime
            .register(
                &RegisterRequest {
                    protocol_version: PROTOCOL_VERSION,
                    pid: 42,
                },
                1_050,
            )
            .unwrap_err();
        assert_eq!(pid_error.code(), "registration_conflict");
        assert_eq!(runtime.lifecycle, Lifecycle::Starting);

        let protocol_error = runtime
            .register(
                &RegisterRequest {
                    protocol_version: PROTOCOL_VERSION + 1,
                    pid: 41,
                },
                1_060,
            )
            .unwrap_err();
        assert_eq!(protocol_error.code(), "incompatible_protocol");
        assert_eq!(runtime.lifecycle, Lifecycle::Incompatible);
    }

    #[test]
    fn handshake_timeout_schedules_exponential_restart() {
        let mut runtime = Runtime::default();
        let mut now = 10_000;
        let mut delays = Vec::new();

        for _ in 0..7 {
            let retry_at = runtime.on_failure(now, "handshake timeout");
            delays.push((retry_at - now) / 1_000);
            now = retry_at;
        }

        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(runtime.lifecycle, Lifecycle::Backoff);
    }

    #[test]
    fn clean_disable_stops_without_restart() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);
        runtime.disable();
        assert_eq!(runtime.lifecycle, Lifecycle::Stopped);
        assert!(runtime.pid.is_none());
        assert!(runtime.retry_at_ms.is_none());

        runtime.on_failure(2_000, "crash");
        runtime.disable();
        assert_eq!(runtime.lifecycle, Lifecycle::Stopped);
        assert!(runtime.retry_at_ms.is_none());
    }

    #[test]
    fn handshake_deadline_is_explicit_and_inclusive() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);

        assert!(!runtime.handshake_timed_out(1_000 + HANDSHAKE_TIMEOUT_MS - 1));
        assert!(runtime.handshake_timed_out(1_000 + HANDSHAKE_TIMEOUT_MS));
    }
}
