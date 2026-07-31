//! Graceful shutdown for process-level termination signals.
//!
//! Tauri's `RunEvent::Exit` owns the actual cleanup. Turning SIGTERM into an
//! `AppHandle::exit` request keeps tray exit and service-manager/logout
//! termination on the same teardown path.

#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

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
            app.exit(0);
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
}
