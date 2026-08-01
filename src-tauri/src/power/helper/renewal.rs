use std::error::Error;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use jarvis_power_core::protocol::{ErrorCode, Request, Response, DEFAULT_TTL_MS};

use super::client::{HelperClient, HelperClientError, ProductionXpcClient};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseReceipt {
    pub(crate) lease_id: String,
    pub(crate) owner_generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeaseError {
    HelperUnavailable,
    HelperUnapproved,
    Rejected(ErrorCode),
    UnexpectedResponse,
    LeaseMismatch,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HelperUnavailable => formatter.write_str("power-helper is unavailable"),
            Self::HelperUnapproved => {
                formatter.write_str("power-helper transport is not production-attested")
            }
            Self::Rejected(code) => write!(formatter, "power-helper rejected the lease: {code:?}"),
            Self::UnexpectedResponse => {
                formatter.write_str("power-helper returned an unexpected response")
            }
            Self::LeaseMismatch => {
                formatter.write_str("power-helper response does not match the exact lease")
            }
        }
    }
}

impl Error for LeaseError {}

impl LeaseError {
    /// The helper may have committed an acquire before the host lost or
    /// rejected its response. Reusing the same owner generation is the only
    /// safe reconciliation path because acquire is idempotent on that key.
    pub(crate) const fn acquire_may_have_committed(self) -> bool {
        matches!(
            self,
            Self::HelperUnavailable
                | Self::HelperUnapproved
                | Self::UnexpectedResponse
                | Self::LeaseMismatch
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExactReleaseOutcome {
    Confirmed,
    AlreadyAbsent(ErrorCode),
    Retryable(LeaseError),
}

impl ExactReleaseOutcome {
    pub(crate) fn from_result(result: Result<(), LeaseError>) -> Self {
        match result {
            Ok(()) => Self::Confirmed,
            Err(LeaseError::Rejected(
                code @ (ErrorCode::LeaseExpired | ErrorCode::LeaseNotFound),
            )) => Self::AlreadyAbsent(code),
            Err(error) => Self::Retryable(error),
        }
    }

    #[cfg(test)]
    pub(crate) const fn resolved(self) -> bool {
        matches!(self, Self::Confirmed | Self::AlreadyAbsent(_))
    }
}

#[derive(Clone)]
pub(crate) struct LeaseClient {
    helper: Arc<dyn HelperClient>,
}

impl LeaseClient {
    pub(crate) fn production() -> Self {
        Self::new(Arc::new(ProductionXpcClient::new()))
    }

    pub(crate) fn new(helper: Arc<dyn HelperClient>) -> Self {
        Self { helper }
    }

    pub(crate) fn acquire(
        &self,
        profile: &str,
        owner_generation: &str,
    ) -> Result<LeaseReceipt, LeaseError> {
        let response = self.send(Request::AcquireLease {
            profile: profile.into(),
            owner_generation: owner_generation.into(),
            ttl_ms: DEFAULT_TTL_MS,
        })?;
        match response {
            Response::Acquired { lease_id, .. } => Ok(LeaseReceipt {
                lease_id,
                owner_generation: owner_generation.into(),
            }),
            Response::Error { code } => Err(LeaseError::Rejected(code)),
            _ => Err(LeaseError::UnexpectedResponse),
        }
    }

    pub(crate) fn renew(&self, receipt: &LeaseReceipt) -> Result<(), LeaseError> {
        let response = self.send(Request::RenewLease {
            lease_id: receipt.lease_id.clone(),
            owner_generation: receipt.owner_generation.clone(),
            ttl_ms: DEFAULT_TTL_MS,
        })?;
        match response {
            Response::Renewed { lease_id, .. } if lease_id == receipt.lease_id => Ok(()),
            Response::Renewed { .. } => Err(LeaseError::LeaseMismatch),
            Response::Error { code } => Err(LeaseError::Rejected(code)),
            _ => Err(LeaseError::UnexpectedResponse),
        }
    }

    pub(crate) fn release(&self, receipt: &LeaseReceipt) -> Result<(), LeaseError> {
        let response = self.send(Request::ReleaseLease {
            lease_id: receipt.lease_id.clone(),
            owner_generation: receipt.owner_generation.clone(),
        })?;
        match response {
            Response::Released { lease_id } if lease_id == receipt.lease_id => Ok(()),
            Response::Released { .. } => Err(LeaseError::LeaseMismatch),
            Response::Error { code } => Err(LeaseError::Rejected(code)),
            _ => Err(LeaseError::UnexpectedResponse),
        }
    }

    fn send(&self, request: Request) -> Result<Response, LeaseError> {
        if !self.helper.trust().authorizes_production() {
            return Err(LeaseError::HelperUnapproved);
        }
        let reply = self.helper.send(request).map_err(map_client_error)?;
        if !reply.trust.authorizes_production() {
            return Err(LeaseError::HelperUnapproved);
        }
        Ok(reply.response.response)
    }
}

fn map_client_error(_error: HelperClientError) -> LeaseError {
    LeaseError::HelperUnavailable
}

struct RenewalControl {
    cancelled: Mutex<bool>,
    wake: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenewalExit {
    Cancelled,
    AttemptStopped,
    ControlFailed,
    Panicked,
}

pub(crate) struct RenewalHandle {
    control: Arc<RenewalControl>,
    worker: Option<JoinHandle<()>>,
}

impl RenewalHandle {
    #[cfg(test)]
    pub(crate) fn try_start(
        interval: Duration,
        attempt: impl FnMut() -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        Self::try_start_with_exit(interval, attempt, |_| {})
    }

    pub(crate) fn try_start_with_exit(
        interval: Duration,
        mut attempt: impl FnMut() -> bool + Send + 'static,
        on_exit: impl FnOnce(RenewalExit) + Send + 'static,
    ) -> std::io::Result<Self> {
        let control = Arc::new(RenewalControl {
            cancelled: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker_control = control.clone();
        let worker = std::thread::Builder::new()
            .name("jarvis-power-renewal".into())
            .spawn(move || {
                let exit = catch_unwind(AssertUnwindSafe(|| loop {
                    let cancelled = worker_control
                        .cancelled
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let cancelled = match worker_control.wake.wait_timeout_while(
                        cancelled,
                        interval,
                        |cancelled| !*cancelled,
                    ) {
                        Ok((cancelled, _)) => *cancelled,
                        Err(_) => return RenewalExit::ControlFailed,
                    };
                    if cancelled {
                        return RenewalExit::Cancelled;
                    }
                    if !attempt() {
                        return RenewalExit::AttemptStopped;
                    }
                }))
                .unwrap_or(RenewalExit::Panicked);
                on_exit(exit);
            })?;
        Ok(Self {
            control,
            worker: Some(worker),
        })
    }

    #[cfg(test)]
    pub(crate) fn start(
        interval: Duration,
        attempt: impl FnMut() -> bool + Send + 'static,
    ) -> Self {
        Self::try_start(interval, attempt).expect("renewal test worker")
    }

    #[cfg(test)]
    pub(crate) fn start_with_exit(
        interval: Duration,
        attempt: impl FnMut() -> bool + Send + 'static,
        on_exit: impl FnOnce(RenewalExit) + Send + 'static,
    ) -> Self {
        Self::try_start_with_exit(interval, attempt, on_exit)
            .expect("renewal test worker with exit reporting")
    }

    pub(crate) fn stop(mut self) {
        self.stop_inner();
    }

    pub(crate) fn is_finished(&self) -> bool {
        match self.worker.as_ref() {
            Some(worker) => worker.is_finished(),
            None => true,
        }
    }

    fn stop_inner(&mut self) {
        *self
            .control
            .cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.control.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                crate::log::line("[power-helper] renewal worker panicked during shutdown");
            }
        }
    }
}

impl Drop for RenewalHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

pub(crate) fn run_shutdown_sequence<R>(
    close_admission: impl FnOnce(),
    stop_renewal: impl FnOnce(),
    release_lease: impl FnOnce() -> R,
    dispose_iokit: impl FnOnce(),
) -> R {
    close_admission();
    stop_renewal();
    let release = release_lease();
    dispose_iokit();
    release
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use jarvis_power_core::protocol::{
        ErrorCode, Request, RequestId, Response, ResponseEnvelope, DEFAULT_TTL_MS, PROTOCOL_VERSION,
    };

    use super::{
        run_shutdown_sequence, ExactReleaseOutcome, LeaseClient, LeaseError, LeaseReceipt,
        RenewalExit, RenewalHandle,
    };
    use crate::power::helper::client::{HelperClient, HelperClientError, HelperReply, HelperTrust};

    const LEASE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const LEASE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    struct FakeHelper {
        trust: HelperTrust,
        requests: Arc<Mutex<Vec<Request>>>,
        replies: Mutex<VecDeque<Result<HelperReply, HelperClientError>>>,
    }

    impl FakeHelper {
        fn new(
            trust: HelperTrust,
            replies: impl IntoIterator<Item = Result<HelperReply, HelperClientError>>,
        ) -> (Arc<Self>, Arc<Mutex<Vec<Request>>>) {
            let requests = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::new(Self {
                    trust,
                    requests: requests.clone(),
                    replies: Mutex::new(replies.into_iter().collect()),
                }),
                requests,
            )
        }
    }

    impl HelperClient for FakeHelper {
        fn send(&self, request: Request) -> Result<HelperReply, HelperClientError> {
            self.requests.lock().unwrap().push(request);
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("fixture reply")
        }

        fn trust(&self) -> HelperTrust {
            self.trust
        }
    }

    fn reply(response: Response) -> Result<HelperReply, HelperClientError> {
        Ok(HelperReply {
            response: ResponseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::parse("018f0000-0000-7000-8000-000000000001").unwrap(),
                response,
            },
            trust: HelperTrust::ProductionAttested,
        })
    }

    fn acquired(lease_id: &str) -> Result<HelperReply, HelperClientError> {
        reply(Response::Acquired {
            lease_id: lease_id.into(),
            granted_ttl_ms: DEFAULT_TTL_MS,
        })
    }

    #[test]
    fn acquire_renew_release_use_the_exact_receipt_and_default_ttl() {
        let (helper, requests) = FakeHelper::new(
            HelperTrust::ProductionAttested,
            [
                acquired(LEASE_A),
                reply(Response::Renewed {
                    lease_id: LEASE_A.into(),
                    granted_ttl_ms: DEFAULT_TTL_MS,
                }),
                reply(Response::Released {
                    lease_id: LEASE_A.into(),
                }),
            ],
        );
        let client = LeaseClient::new(helper);

        let receipt = client.acquire("profile-a", "g").unwrap();
        assert_eq!(
            receipt,
            LeaseReceipt {
                lease_id: LEASE_A.into(),
                owner_generation: "g".into(),
            }
        );
        client.renew(&receipt).unwrap();
        client.release(&receipt).unwrap();

        assert_eq!(
            *requests.lock().unwrap(),
            [
                Request::AcquireLease {
                    profile: "profile-a".into(),
                    owner_generation: "g".into(),
                    ttl_ms: DEFAULT_TTL_MS,
                },
                Request::RenewLease {
                    lease_id: LEASE_A.into(),
                    owner_generation: "g".into(),
                    ttl_ms: DEFAULT_TTL_MS,
                },
                Request::ReleaseLease {
                    lease_id: LEASE_A.into(),
                    owner_generation: "g".into(),
                },
            ]
        );
    }

    #[test]
    fn unapproved_or_unavailable_helper_fails_closed_before_any_fallback() {
        let (unapproved, unapproved_requests) =
            FakeHelper::new(HelperTrust::DevelopmentOnly, [acquired(LEASE_A)]);
        let client = LeaseClient::new(unapproved);
        assert_eq!(
            client.acquire("profile-a", "g"),
            Err(LeaseError::HelperUnapproved)
        );
        assert!(unapproved_requests.lock().unwrap().is_empty());

        let (unavailable, unavailable_requests) = FakeHelper::new(
            HelperTrust::ProductionAttested,
            [Err(HelperClientError::Unavailable)],
        );
        let client = LeaseClient::new(unavailable);
        assert_eq!(
            client.acquire("profile-a", "g"),
            Err(LeaseError::HelperUnavailable)
        );
        assert_eq!(unavailable_requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn ambiguous_acquire_failures_require_same_generation_reconciliation() {
        for error in [
            LeaseError::HelperUnavailable,
            LeaseError::HelperUnapproved,
            LeaseError::UnexpectedResponse,
            LeaseError::LeaseMismatch,
        ] {
            assert!(error.acquire_may_have_committed(), "{error:?}");
        }
        assert!(!LeaseError::Rejected(ErrorCode::InvalidRequest).acquire_may_have_committed());

        let (helper, requests) = FakeHelper::new(
            HelperTrust::ProductionAttested,
            [
                Err(HelperClientError::Unavailable),
                acquired(LEASE_A),
                reply(Response::Released {
                    lease_id: LEASE_A.into(),
                }),
            ],
        );
        let client = LeaseClient::new(helper);
        assert_eq!(
            client.acquire("profile-a", "generation-a"),
            Err(LeaseError::HelperUnavailable)
        );
        let receipt = client.acquire("profile-a", "generation-a").unwrap();
        assert_eq!(receipt.owner_generation, "generation-a");
        client.release(&receipt).unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            [
                Request::AcquireLease {
                    profile: "profile-a".into(),
                    owner_generation: "generation-a".into(),
                    ttl_ms: DEFAULT_TTL_MS,
                },
                Request::AcquireLease {
                    profile: "profile-a".into(),
                    owner_generation: "generation-a".into(),
                    ttl_ms: DEFAULT_TTL_MS,
                },
                Request::ReleaseLease {
                    lease_id: LEASE_A.into(),
                    owner_generation: "generation-a".into(),
                },
            ]
        );
    }

    #[test]
    fn mismatched_or_rejected_lease_responses_fail_closed() {
        let receipt = LeaseReceipt {
            lease_id: LEASE_A.into(),
            owner_generation: "g".into(),
        };
        let (helper, _) = FakeHelper::new(
            HelperTrust::ProductionAttested,
            [
                reply(Response::Renewed {
                    lease_id: LEASE_B.into(),
                    granted_ttl_ms: DEFAULT_TTL_MS,
                }),
                reply(Response::Released {
                    lease_id: LEASE_B.into(),
                }),
                reply(Response::Error {
                    code: ErrorCode::LeaseExpired,
                }),
            ],
        );
        let client = LeaseClient::new(helper);

        assert_eq!(client.renew(&receipt), Err(LeaseError::LeaseMismatch));
        assert_eq!(client.release(&receipt), Err(LeaseError::LeaseMismatch));
        assert_eq!(
            client.renew(&receipt),
            Err(LeaseError::Rejected(ErrorCode::LeaseExpired))
        );
    }

    #[test]
    fn terminal_absence_resolves_only_the_exact_receipt_debt() {
        for code in [ErrorCode::LeaseExpired, ErrorCode::LeaseNotFound] {
            let outcome = ExactReleaseOutcome::from_result(Err(LeaseError::Rejected(code)));
            assert_eq!(outcome, ExactReleaseOutcome::AlreadyAbsent(code));
            assert!(outcome.resolved());
        }

        for error in [
            LeaseError::HelperUnavailable,
            LeaseError::HelperUnapproved,
            LeaseError::Rejected(ErrorCode::RecoveryRequired),
            LeaseError::LeaseMismatch,
        ] {
            let outcome = ExactReleaseOutcome::from_result(Err(error));
            assert_eq!(outcome, ExactReleaseOutcome::Retryable(error));
            assert!(!outcome.resolved());
        }
    }

    #[test]
    fn renewal_worker_panic_is_reported_as_a_terminal_exit() {
        let (exit_tx, exit_rx) = mpsc::channel();
        let renewal = RenewalHandle::start_with_exit(
            Duration::ZERO,
            || panic!("simulated renewal panic"),
            move |exit| exit_tx.send(exit).unwrap(),
        );

        assert_eq!(
            exit_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            RenewalExit::Panicked
        );
        renewal.stop();
    }

    #[test]
    fn stop_cancels_a_pending_timer_and_waits_for_an_inflight_attempt() {
        let pending_count = Arc::new(AtomicUsize::new(0));
        let worker_count = pending_count.clone();
        let pending = RenewalHandle::start(Duration::from_secs(60), move || {
            worker_count.fetch_add(1, Ordering::SeqCst);
            true
        });
        pending.stop();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(pending_count.load(Ordering::SeqCst), 0);

        let (attempt_started_tx, attempt_started_rx) = mpsc::channel();
        let (finish_attempt_tx, finish_attempt_rx) = mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = attempts.clone();
        let inflight = RenewalHandle::start(Duration::ZERO, move || {
            worker_attempts.fetch_add(1, Ordering::SeqCst);
            attempt_started_tx.send(()).unwrap();
            finish_attempt_rx.recv().unwrap();
            true
        });
        attempt_started_rx.recv().unwrap();

        let (stopped_tx, stopped_rx) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            inflight.stop();
            stopped_tx.send(()).unwrap();
        });
        assert!(stopped_rx.recv_timeout(Duration::from_millis(20)).is_err());
        finish_attempt_tx.send(()).unwrap();
        stopped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        stopper.join().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_order_stops_renewal_before_exact_release_and_iokit() {
        let events = Mutex::new(Vec::new());
        let released = run_shutdown_sequence(
            || events.lock().unwrap().push("close-admission"),
            || events.lock().unwrap().push("stop-renewal"),
            || {
                events.lock().unwrap().push("release:lease-a:g");
                true
            },
            || events.lock().unwrap().push("dispose-iokit"),
        );

        assert!(released);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "close-admission",
                "stop-renewal",
                "release:lease-a:g",
                "dispose-iokit",
            ]
        );
    }

    #[test]
    fn task6_runtime_has_no_pmset_fallback_before_tracked_task7_sudoers_boundary() {
        let source = include_str!("../mod.rs");
        let (runtime, task7_legacy_boundary) = source
            .split_once("async fn arm")
            .expect("arm boundary")
            .1
            .split_once("/// Установка sudoers-правила")
            .expect("legacy installer boundary");

        assert!(runtime.contains("lease_client.acquire"));
        assert!(runtime.contains("lease_client.release"));
        assert!(runtime.contains("force_sleep_now"));
        assert!(
            task7_legacy_boundary.contains("async fn install_sudoers"),
            "Task 7 must remove the still-tracked legacy sudoers installer"
        );
        for forbidden in [
            "SystemPmset",
            "acquire_with(",
            "release_with(",
            "set_disabled(",
        ] {
            assert!(
                !runtime.contains(forbidden),
                "runtime app-side power mutation found: {forbidden}"
            );
        }
    }
}
