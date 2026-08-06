use std::ffi::CString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

#[cfg(test)]
use crate::plugins::manifest::PROTOCOL_VERSION;
use crate::plugins::protocol::RegisterRequest;

pub const HANDSHAKE_TIMEOUT_MS: i64 = 10_000;

#[derive(Clone, Debug)]
pub struct VerifiedExecutable {
    verified_descriptor: Arc<File>,
    root_descriptor: Arc<File>,
    relative_path: PathBuf,
    display_path: PathBuf,
    verified_len: u64,
    verified_digest: [u8; 32],
}

impl VerifiedExecutable {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let display_path = path.into();
        let activation_root = display_path
            .parent()
            .ok_or_else(|| "verified plugin executable не имеет package root".to_owned())?
            .to_path_buf();
        let relative_path = display_path
            .file_name()
            .ok_or_else(|| "verified plugin executable не имеет имени".to_owned())?
            .into();
        let verified_descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&display_path)
            .map_err(|error| {
                format!(
                    "не открыть verified plugin descriptor {}: {error}",
                    display_path.display()
                )
            })?;
        Self::from_descriptor(verified_descriptor, activation_root, relative_path)
    }

    pub(crate) fn from_descriptor(
        verified_descriptor: File,
        activation_root: PathBuf,
        relative_path: PathBuf,
    ) -> Result<Self, String> {
        validate_relative_executable_path(&relative_path)?;
        let display_path = activation_root.join(&relative_path);
        let verified_metadata = verified_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить verified plugin descriptor {}: {error}",
                display_path.display()
            )
        })?;
        let root_descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&activation_root)
            .map_err(|error| {
                format!(
                    "не открыть verified plugin root lease {}: {error}",
                    activation_root.display()
                )
            })?;
        let root_metadata = root_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить verified plugin root lease {}: {error}",
                activation_root.display()
            )
        })?;
        if !root_metadata.is_dir()
            || root_metadata.uid() != effective_uid()
            || root_metadata.mode() & 0o7777 != 0o555
        {
            return Err(format!(
                "verified plugin root {} не является owner-owned immutable directory",
                activation_root.display()
            ));
        }
        let anchored_descriptor = open_relative_file(&root_descriptor, &relative_path)?;
        let anchored_metadata = anchored_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить anchored verified plugin executable {}: {error}",
                display_path.display()
            )
        })?;
        if verified_metadata.dev() != anchored_metadata.dev()
            || verified_metadata.ino() != anchored_metadata.ino()
        {
            return Err(format!(
                "verified plugin executable {} изменился до acquisition root lease",
                display_path.display()
            ));
        }
        let verified_len = verified_metadata.len();
        let verified_digest = descriptor_digest(&verified_descriptor, verified_len)?;
        Ok(Self {
            verified_descriptor: Arc::new(verified_descriptor),
            root_descriptor: Arc::new(root_descriptor),
            relative_path,
            display_path,
            verified_len,
            verified_digest,
        })
    }

    pub fn display_path(&self) -> &std::path::Path {
        &self.display_path
    }

    fn prepare_exec_lease(&self, profile_root: &Path) -> Result<ExactExecLease, String> {
        // Revalidate the anchored package inode immediately before copying. The
        // executable bytes themselves are read only from the held descriptor.
        let anchored_descriptor = open_relative_file(&self.root_descriptor, &self.relative_path)?;
        let anchored_metadata = anchored_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить anchored plugin executable {} перед spawn: {error}",
                self.display_path.display()
            )
        })?;
        let verified_metadata = self.verified_descriptor.metadata().map_err(|error| {
            format!(
                "не проверить held plugin executable {} перед spawn: {error}",
                self.display_path.display()
            )
        })?;
        if anchored_metadata.dev() != verified_metadata.dev()
            || anchored_metadata.ino() != verified_metadata.ino()
        {
            return Err(format!(
                "verified plugin executable {} изменился перед spawn",
                self.display_path.display()
            ));
        }

        ExactExecLease::materialize(
            profile_root,
            &self.verified_descriptor,
            self.verified_len,
            self.verified_digest,
        )
    }
}

fn descriptor_digest(file: &File, len: u64) -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < len {
        let remaining = usize::try_from((len - offset).min(buffer.len() as u64))
            .map_err(|_| "verified plugin executable size overflow".to_string())?;
        let read = file
            .read_at(&mut buffer[..remaining], offset)
            .map_err(|error| format!("не прочитать verified plugin descriptor: {error}"))?;
        if read == 0 {
            return Err("verified plugin executable усечён во время чтения".into());
        }
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    Ok(hasher.finalize().into())
}

struct ExactExecLease {
    directory: PathBuf,
    executable: PathBuf,
}

impl ExactExecLease {
    fn materialize(
        profile_root: &Path,
        source: &File,
        expected_len: u64,
        expected_digest: [u8; 32],
    ) -> Result<Self, String> {
        let parent = prepare_exec_lease_parent(profile_root)?;
        let directory = create_unique_exec_lease_dir(&parent)?;
        let executable = directory.join("bridge");
        let result = (|| {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&executable)
                .map_err(|error| format!("не создать verified exec lease: {error}"))?;
            let mut hasher = Sha256::new();
            let mut offset = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            while offset < expected_len {
                let remaining = usize::try_from((expected_len - offset).min(buffer.len() as u64))
                    .map_err(|_| "verified exec lease size overflow".to_string())?;
                let read = source
                    .read_at(&mut buffer[..remaining], offset)
                    .map_err(|error| format!("не прочитать held plugin bytes: {error}"))?;
                if read == 0 {
                    return Err("held plugin executable усечён перед spawn".into());
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|error| format!("не записать verified exec lease: {error}"))?;
                hasher.update(&buffer[..read]);
                offset += read as u64;
            }
            if <[u8; 32]>::from(hasher.finalize()) != expected_digest {
                return Err("held plugin executable digest изменился перед spawn".into());
            }
            output
                .sync_all()
                .map_err(|error| format!("не синхронизировать verified exec lease: {error}"))?;
            output
                .set_permissions(fs::Permissions::from_mode(0o500))
                .map_err(|error| format!("не заморозить verified exec lease: {error}"))?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
                .map_err(|error| format!("не заморозить verified exec lease directory: {error}"))?;
            Ok(())
        })();
        if let Err(error) = result {
            cleanup_exec_lease(&directory, &executable);
            return Err(error);
        }
        Ok(Self {
            directory,
            executable,
        })
    }
}

impl Drop for ExactExecLease {
    fn drop(&mut self) {
        cleanup_exec_lease(&self.directory, &self.executable);
    }
}

fn prepare_exec_lease_parent(profile_root: &Path) -> Result<PathBuf, String> {
    let parent = profile_root.join("plugin-exec-leases");
    match fs::create_dir(&parent) {
        Ok(()) => fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("не защитить verified exec lease root: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "не создать verified exec lease root {}: {error}",
                parent.display()
            ))
        }
    }
    let metadata = fs::symlink_metadata(&parent)
        .map_err(|error| format!("не проверить verified exec lease root: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(format!(
            "verified exec lease root {} должен быть owner-only directory",
            parent.display()
        ));
    }
    Ok(parent)
}

fn create_unique_exec_lease_dir(parent: &Path) -> Result<PathBuf, String> {
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random)
            .map_err(|_| "не получить entropy для verified exec lease".to_string())?;
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = parent.join(name);
        match fs::create_dir(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(
                    |error| format!("не защитить verified exec lease directory: {error}"),
                )?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("не создать verified exec lease directory: {error}")),
        }
    }
    Err("не выделить уникальный verified exec lease".into())
}

fn cleanup_exec_lease(directory: &Path, executable: &Path) {
    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(executable, fs::Permissions::from_mode(0o700));
    let _ = fs::remove_file(executable);
    let _ = fs::remove_dir(directory);
}

fn validate_relative_executable_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("verified plugin executable path должен быть безопасным relative path".into());
    }
    Ok(())
}

fn open_relative_file(root: &File, path: &Path) -> Result<File, String> {
    let mut directory = root.try_clone().map_err(|error| {
        format!(
            "не клонировать verified plugin root descriptor для {}: {error}",
            path.display()
        )
    })?;
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err("verified plugin executable path неканоничен".into());
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| "verified plugin executable path содержит NUL".to_owned())?;
        let is_last = index + 1 == components.len();
        let flags = if is_last {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(format!(
                "не открыть anchored verified plugin path {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { File::from_raw_fd(descriptor) };
        let metadata = opened.metadata().map_err(|error| {
            format!(
                "не проверить anchored verified plugin path {}: {error}",
                path.display()
            )
        })?;
        let expected_mode = if is_last { 0o555 } else { 0o555 };
        if metadata.uid() != effective_uid()
            || metadata.mode() & 0o7777 != expected_mode
            || (is_last && (!metadata.is_file() || metadata.nlink() != 1))
            || (!is_last && !metadata.is_dir())
        {
            return Err(format!(
                "anchored verified plugin path {} не является immutable package entry",
                path.display()
            ));
        }
        if is_last {
            return Ok(opened);
        }
        directory = opened;
    }
    Err("verified plugin executable path пуст".into())
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[derive(Clone, Debug)]
pub enum SpawnExecutable {
    VerifiedReceipt(VerifiedExecutable),
    LegacyAgentVm(PathBuf),
}

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub plugin_id: String,
    pub executable: SpawnExecutable,
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
    exec_lease: Option<ExactExecLease>,
}

impl ManagedChild for SystemChild {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        let pid =
            i32::try_from(self.child.id()).map_err(|_| "plugin PID превышает pid_t".to_string())?;
        let status = self
            .child
            .try_wait()
            .map(|status| status.map(|status| status.code().unwrap_or(-1)))
            .map_err(|err| format!("не проверить plugin process: {err}"))?;
        if status.is_some() {
            // The process leader may exit while helpers remain in the isolated
            // group. Reap those descendants before the host drops supervision.
            signal_process_group(pid, libc::SIGKILL)?;
            self.exec_lease.take();
        }
        Ok(status)
    }

    fn kill(&mut self) -> Result<(), String> {
        let pid =
            i32::try_from(self.child.id()).map_err(|_| "plugin PID превышает pid_t".to_string())?;
        if self
            .child
            .try_wait()
            .map_err(|err| format!("не проверить plugin process перед stop: {err}"))?
            .is_some()
        {
            signal_process_group(pid, libc::SIGKILL)?;
            self.exec_lease.take();
            return Ok(());
        }
        signal_process_group(pid, libc::SIGTERM)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if self
                .child
                .try_wait()
                .map_err(|err| format!("не проверить plugin process при stop: {err}"))?
                .is_some()
            {
                signal_process_group(pid, libc::SIGKILL)?;
                self.exec_lease.take();
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        signal_process_group(pid, libc::SIGKILL)?;
        self.child
            .wait()
            .map_err(|err| format!("не дождаться plugin process после stop: {err}"))?;
        self.exec_lease.take();
        Ok(())
    }
}

fn signal_process_group(pid: i32, signal: i32) -> Result<(), String> {
    if unsafe { libc::kill(-pid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(format!("не остановить plugin process group: {error}"))
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
        let profile_root = spec
            .socket
            .parent()
            .ok_or_else(|| "Jarvis socket не имеет profile root".to_string())?;
        let (mut command, display_path, exec_lease) = match &spec.executable {
            SpawnExecutable::LegacyAgentVm(path) => {
                let mut command = Command::new(path);
                command.current_dir(&spec.cwd);
                (command, path.clone(), None)
            }
            SpawnExecutable::VerifiedReceipt(executable) => {
                let lease = executable.prepare_exec_lease(profile_root)?;
                let mut command = Command::new(&lease.executable);
                command.current_dir(&spec.cwd);
                command.arg0(&executable.display_path);
                (command, executable.display_path.clone(), Some(lease))
            }
        };
        command.args(&spec.args);
        command.process_group(0);
        let mut child = command
            // Плагин получает только identity-контракт ниже. В частности, host
            // proxy, LLM proxy, API keys и credential helpers не наследуются.
            .env_clear()
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
                    display_path.display()
                )
            })?;

        if let Some(stdout) = child.stdout.take() {
            pipe_to_log(stdout, spec.plugin_id.clone(), "stdout", spec.token.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            pipe_to_log(stderr, spec.plugin_id.clone(), "stderr", spec.token.clone());
        }
        Ok(Box::new(SystemChild { child, exec_lease }))
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
    Incompatible { received: u32, expected: u32 },
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
            RegistrationError::Incompatible { received, expected } => write!(
                f,
                "несовместимый protocolVersion {received}, ожидается {expected}"
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
        expected_protocol: u32,
        request: &RegisterRequest,
        now_ms: i64,
    ) -> Result<(), RegistrationError> {
        if request.protocol_version != expected_protocol {
            self.lifecycle = Lifecycle::Incompatible;
            self.last_error = Some(format!(
                "несовместимый protocolVersion {}, ожидается {expected_protocol}",
                request.protocol_version,
            ));
            return Err(RegistrationError::Incompatible {
                received: request.protocol_version,
                expected: expected_protocol,
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

    pub fn on_error(&mut self, error: impl Into<String>) {
        self.lifecycle = Lifecycle::Error;
        self.pid = None;
        self.started_at_ms = None;
        self.registered_at_ms = None;
        self.handshake_deadline_ms = None;
        self.retry_at_ms = None;
        self.last_error = Some(error.into());
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_path(tag: &str) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "jarvis-plugin-supervisor-{tag}-{}-{}-{timestamp}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn system_spawner_does_not_inherit_host_secret_or_proxy_environment() {
        let root = temp_path("clean-env");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("capture-env.sh");
        let capture = root.join("env.txt");
        fs::write(&executable, "#!/bin/sh\n/usr/bin/env > \"$1\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var(
            "JARVIS_TEST_PROXY_CREDENTIAL_SENTINEL",
            "synthetic-must-not-reach-plugin",
        );
        let spec = SpawnSpec {
            plugin_id: "synthetic-plugin".into(),
            executable: SpawnExecutable::LegacyAgentVm(executable),
            args: vec![capture.to_string_lossy().into_owned()],
            cwd: root.clone(),
            socket: root.join("run.sock"),
            token: "synthetic-token".into(),
            protocol_version: PROTOCOL_VERSION,
        };

        let mut child = SystemSpawner.spawn(&spec).unwrap();
        let mut exit_code = None;
        for _ in 0..2_000 {
            if let Some(code) = child.try_wait().unwrap() {
                exit_code = Some(code);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::env::remove_var("JARVIS_TEST_PROXY_CREDENTIAL_SENTINEL");
        assert_eq!(exit_code, Some(0), "synthetic plugin process failed");
        let captured = fs::read_to_string(&capture).unwrap();

        assert!(
            !captured.contains("JARVIS_TEST_PROXY_CREDENTIAL_SENTINEL"),
            "plugin child inherited a host-only environment key"
        );
        for required in [
            "JARVIS_SOCKET=",
            "JARVIS_PLUGIN_ID=synthetic-plugin",
            "JARVIS_PLUGIN_TOKEN=synthetic-token",
            "JARVIS_PLUGIN_PROTOCOL=1",
        ] {
            assert!(
                captured.contains(required),
                "missing identity env {required}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_plugin_runs_in_its_own_process_group() {
        let root = temp_path("process-group");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("capture-pgid.sh");
        let capture = root.join("pgid.txt");
        fs::write(
            &executable,
            "#!/bin/sh\n/bin/ps -o pgid= -p $$ > \"$1\"\n/bin/sleep 5\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let spec = SpawnSpec {
            plugin_id: "agent-vm".into(),
            executable: SpawnExecutable::LegacyAgentVm(executable),
            args: vec![capture.to_string_lossy().into_owned()],
            cwd: root.clone(),
            socket: root.join("run.sock"),
            token: "synthetic-token".into(),
            protocol_version: PROTOCOL_VERSION,
        };

        let mut child = SystemSpawner.spawn(&spec).unwrap();
        let mut captured = String::new();
        for _ in 0..200 {
            if let Ok(value) = fs::read_to_string(&capture) {
                if !value.trim().is_empty() {
                    captured = value;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !captured.trim().is_empty(),
            "plugin did not publish its PGID"
        );
        let pgid = captured.trim().parse::<u32>().unwrap();
        assert_eq!(pgid, child.id(), "plugin inherited Jarvis process group");
        child.kill().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observing_leader_exit_kills_remaining_process_group_descendants() {
        let root = temp_path("leader-exit-descendants");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("spawn-descendant.sh");
        let capture = root.join("descendant.pid");
        fs::write(
            &executable,
            "#!/bin/sh\n/bin/sleep 30 &\nprintf '%s\\n' \"$!\" > \"$1\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let spec = SpawnSpec {
            plugin_id: "agent-vm".into(),
            executable: SpawnExecutable::LegacyAgentVm(executable),
            args: vec![capture.to_string_lossy().into_owned()],
            cwd: root.clone(),
            socket: root.join("run.sock"),
            token: "synthetic-token".into(),
            protocol_version: PROTOCOL_VERSION,
        };

        let mut child = SystemSpawner.spawn(&spec).unwrap();
        let mut descendant_pid = None;
        for _ in 0..200 {
            if let Ok(value) = fs::read_to_string(&capture) {
                if let Ok(pid) = value.trim().parse::<i32>() {
                    descendant_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let descendant_pid = descendant_pid.expect("plugin did not publish descendant PID");
        let mut leader_exit = None;
        for _ in 0..200 {
            if let Some(code) = child.try_wait().unwrap() {
                leader_exit = Some(code);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(leader_exit, Some(0), "plugin leader did not exit");

        let mut descendant_alive = true;
        for _ in 0..200 {
            let rc = unsafe { libc::kill(descendant_pid, 0) };
            if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                descendant_alive = false;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if descendant_alive {
            unsafe {
                libc::kill(descendant_pid, libc::SIGKILL);
            }
        }
        assert!(
            !descendant_alive,
            "leader exit left process-group descendant {descendant_pid} alive"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_receipt_executes_held_bytes_after_visible_path_swap() {
        let root = temp_path("verified-descriptor");
        let visible = root.join("visible");
        let package = visible.join("package");
        let bin = package.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join("bridge");
        let capture = root.join("result.txt");
        fs::write(&executable, "#!/bin/sh\nprintf verified > \"$1\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o555)).unwrap();
        let held = VerifiedExecutable::open(&executable).unwrap();

        fs::rename(&visible, root.join("held-container")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(&executable, "#!/bin/sh\nprintf replaced > \"$1\"\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o555)).unwrap();
        let spec = SpawnSpec {
            plugin_id: "dev.example.verified".into(),
            executable: SpawnExecutable::VerifiedReceipt(held),
            args: vec![capture.to_string_lossy().into_owned()],
            cwd: package.clone(),
            socket: root.join("run.sock"),
            token: "synthetic-token".into(),
            protocol_version: 2,
        };

        let mut child = SystemSpawner.spawn(&spec).unwrap();
        let mut exit_code = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(code) = child.try_wait().unwrap() {
                exit_code = Some(code);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert_eq!(exit_code, Some(0));
        assert_eq!(fs::read_to_string(&capture).unwrap(), "verified");
        assert_eq!(
            fs::read_dir(root.join("plugin-exec-leases"))
                .unwrap()
                .count(),
            0,
            "completed plugin left an executable lease behind"
        );
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(
            root.join("held-container/package/bin"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::set_permissions(
            root.join("held-container/package"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn register_accepts_matching_plugin_pid_and_protocol() {
        let mut runtime = Runtime::default();
        runtime.on_spawned(41, 1_000);

        runtime
            .register(
                PROTOCOL_VERSION,
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
        runtime.register(PROTOCOL_VERSION, &request, 1_050).unwrap();

        runtime.register(PROTOCOL_VERSION, &request, 2_000).unwrap();

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
                PROTOCOL_VERSION,
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
                PROTOCOL_VERSION,
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
