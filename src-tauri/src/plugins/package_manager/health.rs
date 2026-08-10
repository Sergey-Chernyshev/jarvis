use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use jarvis_plugin_protocol::manifest::Digest;

use super::manager::{ManagerError, ManagerResult};

const MAX_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEALTH_ARGS: usize = 16;
const MAX_HEALTH_ARG_BYTES: usize = 4096;

#[derive(Clone, Debug)]
pub struct HealthCheck {
    pub package_root: PathBuf,
    pub program_relative: String,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub package_digest: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthReport {
    pub elapsed: Duration,
}

pub trait HealthRunner: Send + Sync {
    fn check(&self, request: &HealthCheck) -> ManagerResult<HealthReport>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeHealthRunner;

impl HealthRunner for NativeHealthRunner {
    fn check(&self, request: &HealthCheck) -> ManagerResult<HealthReport> {
        validate_request(request)?;
        Err(ManagerError::new(
            "health_exact_exec_unavailable",
            format!(
                "native health execution for {} is disabled until the host provides an atomic exact-exec primitive",
                request.package_digest.as_str()
            ),
        ))
    }
}

fn validate_request(request: &HealthCheck) -> ManagerResult<()> {
    if !request.package_root.is_absolute()
        || request.timeout.is_zero()
        || request.timeout > MAX_HEALTH_TIMEOUT
        || request.args.len() > MAX_HEALTH_ARGS
        || request
            .args
            .iter()
            .any(|arg| arg.len() > MAX_HEALTH_ARG_BYTES || arg.contains('\0'))
    {
        return Err(ManagerError::new(
            "health_request",
            "native health request exceeds its policy limits",
        ));
    }
    let program = Path::new(&request.program_relative);
    if program.is_absolute()
        || request.program_relative.len() > MAX_HEALTH_ARG_BYTES
        || request.program_relative.contains('\0')
        || program
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManagerError::new(
            "health_program",
            "health program must be a safe package-relative path",
        ));
    }
    Ok(())
}
