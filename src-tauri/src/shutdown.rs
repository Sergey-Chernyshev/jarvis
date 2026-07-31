//! Graceful shutdown for process-level termination signals.
//!
//! Every explicit exit path runs the same ordered, per-phase-idempotent
//! coordinator before asking Tauri to terminate. `RunEvent::Exit` invokes it
//! again as a fallback for exits which did not use a Jarvis wrapper.

use crate::daemon::Daemon;
use crate::power::Power;
use std::any::Any;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseStatus {
    AlreadyComplete,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    pub power: PhaseStatus,
    pub state: PhaseStatus,
    pub plugins: PhaseStatus,
    pub voice: PhaseStatus,
    pub stt: PhaseStatus,
    pub wake: PhaseStatus,
    pub audio: PhaseStatus,
    pub socket: PhaseStatus,
}

impl CleanupReport {
    pub fn complete(self) -> bool {
        [
            self.power,
            self.state,
            self.plugins,
            self.voice,
            self.stt,
            self.wake,
            self.audio,
            self.socket,
        ]
        .into_iter()
        .all(|status| status != PhaseStatus::Failed)
    }
}

#[derive(Default)]
struct CleanupState {
    power: bool,
    state: bool,
    plugins: bool,
    voice: bool,
    stt: bool,
    wake: bool,
    audio: bool,
    socket: bool,
}

#[derive(Default)]
struct CleanupGate {
    state: Mutex<CleanupState>,
}

static CLEANUP: OnceLock<CleanupGate> = OnceLock::new();

fn global_cleanup() -> &'static CleanupGate {
    CLEANUP.get_or_init(CleanupGate::default)
}

fn run_phase(
    name: &'static str,
    completed: &mut bool,
    action: impl FnOnce() -> bool,
) -> PhaseStatus {
    if *completed {
        return PhaseStatus::AlreadyComplete;
    }

    let started = Instant::now();
    let succeeded = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)) {
        Ok(succeeded) => succeeded,
        Err(payload) => {
            crate::log::line(&format!(
                "[shutdown] phase={name} panic={} elapsed_ms={}",
                panic_message(payload.as_ref()),
                started.elapsed().as_millis()
            ));
            false
        }
    };
    if succeeded {
        *completed = true;
        crate::log::line(&format!(
            "[shutdown] phase={name} complete elapsed_ms={}",
            started.elapsed().as_millis()
        ));
        PhaseStatus::Completed
    } else {
        crate::log::line(&format!(
            "[shutdown] phase={name} pending elapsed_ms={}",
            started.elapsed().as_millis()
        ));
        PhaseStatus::Failed
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Test-sized projection of the production coordinator. The coordinator lock
/// remains held while phases advance, so concurrent exit signals cannot run a
/// teardown phase twice.
#[cfg(test)]
fn run_ordered(
    gate: &CleanupGate,
    power: impl FnOnce() -> bool,
    state_phase: impl FnOnce() -> bool,
    rest: impl FnOnce() -> bool,
) {
    let mut state = gate
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = run_phase("power", &mut state.power, power);
    let _ = run_phase("state", &mut state.state, state_phase);
    let _ = run_phase("rest", &mut state.plugins, rest);
}

pub fn cleanup(d: &Arc<Daemon>) -> CleanupReport {
    let mut state = global_cleanup()
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let power = run_phase("power", &mut state.power, || {
        let report = Power::dispose(d);
        if !report.released() {
            crate::log::line(&format!("[shutdown] power cleanup incomplete: {report:?}"));
        }
        report.released()
    });
    let persisted_state = run_phase("state", &mut state.state, || {
        d.write_state_now();
        true
    });
    let plugins = run_phase("plugins", &mut state.plugins, || {
        d.plugins.dispose(d);
        true
    });
    let voice = run_phase("voice", &mut state.voice, || {
        d.voice.dispose();
        true
    });
    let stt = run_phase("stt", &mut state.stt, || {
        d.stt.dispose();
        true
    });
    let wake = run_phase("wake", &mut state.wake, || {
        d.wake.dispose();
        true
    });
    let audio = run_phase("audio", &mut state.audio, || {
        d.audio.dispose();
        true
    });
    let socket = run_phase("socket", &mut state.socket, || {
        match std::fs::remove_file(crate::util::sock_path()) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                crate::log::line(&format!("[shutdown] socket removal failed: {error}"));
                false
            }
        }
    });

    CleanupReport {
        power,
        state: persisted_state,
        plugins,
        voice,
        stt,
        wake,
        audio,
        socket,
    }
}

pub fn request_exit(d: &Arc<Daemon>, exit_code: i32) {
    let report = cleanup(d);
    if !report.complete() {
        crate::log::line(&format!("[shutdown] exit cleanup remains incomplete: {report:?}"));
    }
    d.app.exit(exit_code);
}

pub fn request_restart(d: &Arc<Daemon>) {
    let report = cleanup(d);
    if !report.complete() {
        crate::log::line(&format!(
            "[shutdown] restart cleanup remains incomplete: {report:?}"
        ));
    }
    d.app.request_restart();
}

pub fn install(app: tauri::AppHandle) {
    #[cfg(unix)]
    tauri::async_runtime::spawn(async move {
        let mut sigterm = match sigterm_stream() {
            Ok(stream) => stream,
            Err(err) => {
                crate::log::line(&format!(
                    "[shutdown] не удалось установить SIGTERM handler: {err}"
                ));
                return;
            }
        };

        if sigterm.recv().await.is_some() {
            crate::log::line("[shutdown] получен SIGTERM, завершаемся штатно");
            request_exit(&Daemon::get(&app), 0);
        }
    });

    #[cfg(not(unix))]
    let _ = app;
}

#[cfg(unix)]
fn sigterm_stream() -> std::io::Result<Signal> {
    signal(SignalKind::terminate())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn sigterm_is_observed_for_graceful_shutdown() {
        let mut sigterm = sigterm_stream().expect("SIGTERM handler must install");

        let rc = unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(rc, 0);

        let received = tokio::time::timeout(Duration::from_secs(1), sigterm.recv())
            .await
            .expect("SIGTERM must be observed before timeout");
        assert!(received.is_some());
    }

    #[test]
    fn cleanup_runs_power_before_rest_and_retries_only_failed_phase() {
        let gate = CleanupGate::default();
        let trace = RefCell::new(Vec::new());

        run_ordered(
            &gate,
            || {
                trace.borrow_mut().push("power-failed");
                false
            },
            || {
                trace.borrow_mut().push("state");
                true
            },
            || {
                trace.borrow_mut().push("rest");
                true
            },
        );
        run_ordered(
            &gate,
            || {
                trace.borrow_mut().push("power-retry");
                true
            },
            || {
                trace.borrow_mut().push("state");
                true
            },
            || {
                trace.borrow_mut().push("rest");
                true
            },
        );

        assert_eq!(
            trace.into_inner(),
            ["power-failed", "state", "rest", "power-retry"]
        );
    }

    #[test]
    fn failed_or_panicking_phase_does_not_skip_later_phases_and_is_retried() {
        let gate = CleanupGate::default();
        let trace = RefCell::new(Vec::new());

        run_ordered(
            &gate,
            || {
                trace.borrow_mut().push("power");
                true
            },
            || {
                trace.borrow_mut().push("state-panic");
                panic!("fixture panic");
            },
            || {
                trace.borrow_mut().push("rest");
                true
            },
        );
        run_ordered(
            &gate,
            || {
                trace.borrow_mut().push("power");
                true
            },
            || {
                trace.borrow_mut().push("state-retry");
                true
            },
            || {
                trace.borrow_mut().push("rest");
                true
            },
        );

        assert_eq!(
            trace.into_inner(),
            ["power", "state-panic", "rest", "state-retry"]
        );
    }

    #[test]
    fn repeated_cleanup_after_complete_work_is_a_noop() {
        let gate = CleanupGate::default();
        let trace = RefCell::new(Vec::new());

        for _ in 0..2 {
            run_ordered(
                &gate,
                || {
                    trace.borrow_mut().push("power");
                    true
                },
                || {
                    trace.borrow_mut().push("state");
                    true
                },
                || {
                    trace.borrow_mut().push("rest");
                    true
                },
            );
        }

        assert_eq!(trace.into_inner(), ["power", "state", "rest"]);
    }
}
