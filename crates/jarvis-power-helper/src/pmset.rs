use std::fmt;
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PMSET_PROGRAM: &str = "/usr/bin/pmset";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CAPTURE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmsetError {
    Unsupported,
    Spawn,
    Io,
    Timeout,
    CommandFailed,
    InvalidOutput,
    OutputTooLarge,
}

impl fmt::Display for PmsetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "system power backend is unsupported",
            Self::Spawn => "system power command could not start",
            Self::Io => "system power command I/O failed",
            Self::Timeout => "system power command timed out",
            Self::CommandFailed => "system power command failed",
            Self::InvalidOutput => "system power output is invalid",
            Self::OutputTooLarge => "system power output exceeded its bound",
        })
    }
}

impl std::error::Error for PmsetError {}

/// The complete privileged power mutation surface.
///
/// There is deliberately no generic command/argv method.
pub trait PmsetBackend: Send {
    fn read_disabled(&mut self) -> Result<bool, PmsetError>;
    fn set_disabled(&mut self, value: bool) -> Result<(), PmsetError>;
    fn boot_id(&mut self) -> Result<String, PmsetError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPmset;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemPmsetPolicy;

impl SystemPmset {
    pub const fn policy() -> SystemPmsetPolicy {
        SystemPmsetPolicy
    }

    fn run(&self, invocation: PmsetInvocation) -> Result<BoundedOutput, PmsetError> {
        run_bounded(PMSET_PROGRAM, invocation.args(), COMMAND_TIMEOUT).map_err(PmsetError::from)
    }
}

impl SystemPmsetPolicy {
    pub const fn program(self) -> &'static str {
        PMSET_PROGRAM
    }

    pub const fn timeout(self) -> Duration {
        COMMAND_TIMEOUT
    }

    pub const fn read_args(self) -> [&'static str; 1] {
        ["-g"]
    }

    pub const fn write_args(self, value: bool) -> [&'static str; 3] {
        ["-a", "disablesleep", if value { "1" } else { "0" }]
    }

    pub const fn stdin_is_null(self) -> bool {
        true
    }

    pub const fn environment_is_cleared(self) -> bool {
        true
    }

    pub const fn output_is_bounded(self) -> bool {
        true
    }
}

impl PmsetBackend for SystemPmset {
    fn read_disabled(&mut self) -> Result<bool, PmsetError> {
        let output = self.run(PmsetInvocation::Read)?;
        if !output.status.success() {
            return Err(PmsetError::CommandFailed);
        }
        parse_disabled(&output.stdout)
    }

    fn set_disabled(&mut self, value: bool) -> Result<(), PmsetError> {
        let output = self.run(PmsetInvocation::Write(value))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(PmsetError::CommandFailed)
        }
    }

    fn boot_id(&mut self) -> Result<String, PmsetError> {
        system_boot_id()
    }
}

#[derive(Clone, Copy)]
enum PmsetInvocation {
    Read,
    Write(bool),
}

impl PmsetInvocation {
    fn args(self) -> &'static [&'static str] {
        const READ: &[&str] = &["-g"];
        const WRITE_FALSE: &[&str] = &["-a", "disablesleep", "0"];
        const WRITE_TRUE: &[&str] = &["-a", "disablesleep", "1"];
        match self {
            Self::Read => READ,
            Self::Write(false) => WRITE_FALSE,
            Self::Write(true) => WRITE_TRUE,
        }
    }
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    #[allow(dead_code)]
    stderr: Vec<u8>,
}

struct CapturedPipe {
    bytes: Vec<u8>,
    overflowed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunError {
    Spawn,
    Io,
    Timeout { kill_attempted: bool, reaped: bool },
    OutputTooLarge,
}

impl From<RunError> for PmsetError {
    fn from(error: RunError) -> Self {
        match error {
            RunError::Spawn => Self::Spawn,
            RunError::Io => Self::Io,
            RunError::Timeout { .. } => Self::Timeout,
            RunError::OutputTooLarge => Self::OutputTooLarge,
        }
    }
}

fn run_bounded(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<BoundedOutput, RunError> {
    let mut child = Command::new(program)
        .args(arguments)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| RunError::Spawn)?;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Io);
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Io);
    };
    let stdout_reader = thread::spawn(move || drain_pipe(stdout));
    let stderr_reader = thread::spawn(move || drain_pipe(stderr));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let kill_attempted = child.kill().is_ok();
                let reaped = child.wait().is_ok();
                break Err(RunError::Timeout {
                    kill_attempted,
                    reaped,
                });
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(RunError::Io);
            }
        }
    };

    let stdout = stdout_reader.join().map_err(|_| RunError::Io)??;
    let stderr = stderr_reader.join().map_err(|_| RunError::Io)??;
    let status = status?;
    if stdout.overflowed || stderr.overflowed {
        return Err(RunError::OutputTooLarge);
    }
    Ok(BoundedOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn drain_pipe(mut pipe: impl Read) -> Result<CapturedPipe, RunError> {
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES);
    let mut overflowed = false;
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let read = pipe.read(&mut buffer).map_err(|_| RunError::Io)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        overflowed |= retained < read;
    }
    Ok(CapturedPipe { bytes, overflowed })
}

fn parse_disabled(bytes: &[u8]) -> Result<bool, PmsetError> {
    let output = std::str::from_utf8(bytes).map_err(|_| PmsetError::InvalidOutput)?;
    let mut found = None;
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let first = fields.next();
        if first == Some("SleepDisabled") {
            let value = match (fields.next(), fields.next()) {
                (Some("0"), None) => false,
                (Some("1"), None) => true,
                _ => return Err(PmsetError::InvalidOutput),
            };
            if found.replace(value).is_some() {
                return Err(PmsetError::InvalidOutput);
            }
        } else if fields.any(|field| field == "SleepDisabled") {
            return Err(PmsetError::InvalidOutput);
        }
    }
    found.ok_or(PmsetError::InvalidOutput)
}

#[cfg(target_os = "macos")]
fn system_boot_id() -> Result<String, PmsetError> {
    const NAME: &std::ffi::CStr = c"kern.boottime";
    // SAFETY: timeval is plain old data and zero is a valid initial buffer.
    let mut boot = unsafe { std::mem::zeroed::<libc::timeval>() };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: NAME is NUL-terminated; boot and size are valid writable
    // buffers; the call performs a read-only sysctl.
    let result = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr(),
            (&mut boot as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0
        || size != std::mem::size_of::<libc::timeval>()
        || boot.tv_sec <= 0
        || boot.tv_usec < 0
        || boot.tv_usec >= 1_000_000
    {
        return Err(PmsetError::InvalidOutput);
    }
    Ok(format!("darwin-v1-{}-{}", boot.tv_sec, boot.tv_usec))
}

#[cfg(not(target_os = "macos"))]
fn system_boot_id() -> Result<String, PmsetError> {
    Err(PmsetError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_exactly_one_boolean_sleep_disabled_field() {
        assert_eq!(parse_disabled(b" SleepDisabled 1\n"), Ok(true));
        assert_eq!(parse_disabled(b" SleepDisabled 0\n"), Ok(false));
        assert_eq!(
            parse_disabled(b"SleepDisabled 1\nSleepDisabled 1\n"),
            Err(PmsetError::InvalidOutput)
        );
        assert_eq!(
            parse_disabled(b"SleepDisabled 2\n"),
            Err(PmsetError::InvalidOutput)
        );
        assert_eq!(
            parse_disabled(b"prefix SleepDisabled 1\nSleepDisabled 1\n"),
            Err(PmsetError::InvalidOutput)
        );
        assert_eq!(
            parse_disabled(b"SleepDisabled 1 trailing\n"),
            Err(PmsetError::InvalidOutput)
        );
        assert_eq!(parse_disabled(&[0xff]), Err(PmsetError::InvalidOutput));
    }

    #[test]
    fn runner_drains_both_overflowing_pipes_without_deadlock() {
        let result = run_bounded(
            "/bin/sh",
            &[
                "-c",
                "/usr/bin/yes x | /usr/bin/head -c 70000; /usr/bin/yes y | /usr/bin/head -c 70000 >&2",
            ],
            Duration::from_secs(2),
        );
        assert_eq!(result.map(|_| ()), Err(RunError::OutputTooLarge));
    }

    #[test]
    fn timeout_kills_and_reaps_the_child() {
        let error = run_bounded(
            "/bin/sh",
            &["-c", "exec /bin/sleep 30"],
            Duration::from_millis(50),
        )
        .map(|_| ())
        .unwrap_err();
        assert_eq!(
            error,
            RunError::Timeout {
                kill_attempted: true,
                reaped: true
            }
        );
    }

    #[test]
    fn nonzero_output_is_bounded_and_never_enters_the_error() {
        let output = run_bounded(
            "/bin/sh",
            &[
                "-c",
                "printf do-not-log-me; printf do-not-log-me >&2; exit 7",
            ],
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(!output.status.success());
        let error = PmsetError::CommandFailed;
        assert!(!format!("{error:?}").contains("do-not-log-me"));
        assert!(output.stdout.len() <= MAX_CAPTURE_BYTES);
        assert!(output.stderr.len() <= MAX_CAPTURE_BYTES);
    }
}
