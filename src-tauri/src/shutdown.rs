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

    // Power::dispose owns its stricter inner order: close admission, stop and
    // join renewal, release the exact helper lease, then dispose local IOKit.
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
        crate::log::line(&format!(
            "[shutdown] exit cleanup remains incomplete: {report:?}"
        ));
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
    {
        install_signal_listener(app.clone(), "SIGTERM", sigterm_stream);
        install_signal_listener(app, "SIGINT", sigint_stream);
    }

    #[cfg(not(unix))]
    let _ = app;
}

#[cfg(unix)]
fn install_signal_listener(
    app: tauri::AppHandle,
    signal_name: &'static str,
    stream_factory: fn() -> std::io::Result<Signal>,
) {
    tauri::async_runtime::spawn(async move {
        let mut stream = match stream_factory() {
            Ok(stream) => stream,
            Err(err) => {
                crate::log::line(&format!(
                    "[shutdown] не удалось установить {signal_name} handler: {err}"
                ));
                return;
            }
        };
        if stream.recv().await.is_some() {
            crate::log::line(&format!(
                "[shutdown] получен {signal_name}, завершаемся штатно"
            ));
            request_exit(&Daemon::get(&app), 0);
        }
    });
}

#[cfg(unix)]
fn sigterm_stream() -> std::io::Result<Signal> {
    signal(SignalKind::terminate())
}

#[cfg(unix)]
fn sigint_stream() -> std::io::Result<Signal> {
    signal(SignalKind::interrupt())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn termination_signals_are_observed_in_isolated_subprocesses() {
        for (signal_name, raw_signal) in [("SIGTERM", libc::SIGTERM), ("SIGINT", libc::SIGINT)] {
            let mut child = Command::new(std::env::current_exe().expect("current test binary"))
                .args([
                    "--exact",
                    "shutdown::tests::signal_probe_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("JARVIS_SIGNAL_PROBE", signal_name)
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn isolated signal probe");
            let stdout = child.stdout.take().expect("probe stdout");
            let (ready_tx, ready_rx) = mpsc::channel();
            let reader = thread::spawn(move || {
                let mut captured = String::new();
                for line in BufReader::new(stdout).lines() {
                    let line = line.expect("read probe output");
                    if line.contains("signal-probe-ready") {
                        let _ = ready_tx.send(());
                    }
                    captured.push_str(&line);
                    captured.push('\n');
                }
                captured
            });

            if ready_rx.recv_timeout(Duration::from_secs(5)).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                let output = reader.join().expect("join probe output");
                panic!("{signal_name} probe did not become ready:\n{output}");
            }

            let rc = unsafe { libc::kill(child.id() as libc::pid_t, raw_signal) };
            assert_eq!(rc, 0, "send {signal_name} to probe");
            let status = child.wait().expect("wait for signal probe");
            let output = reader.join().expect("join probe output");
            assert!(
                status.success(),
                "{signal_name} probe failed with {status}:\n{output}"
            );
        }
    }

    #[test]
    #[ignore = "subprocess helper for termination_signals_are_observed_in_isolated_subprocesses"]
    fn signal_probe_child() {
        let Ok(signal_name) = std::env::var("JARVIS_SIGNAL_PROBE") else {
            return;
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("signal probe runtime");
        runtime.block_on(async {
            let mut stream = match signal_name.as_str() {
                "SIGTERM" => sigterm_stream(),
                "SIGINT" => sigint_stream(),
                other => panic!("unsupported signal probe: {other}"),
            }
            .expect("install signal probe");
            println!("signal-probe-ready:{signal_name}");
            std::io::stdout().flush().expect("flush probe readiness");
            let received = tokio::time::timeout(Duration::from_secs(5), stream.recv())
                .await
                .expect("signal must be observed before timeout");
            assert!(received.is_some());
        });
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
