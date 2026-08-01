#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use jarvis_power_core::protocol::{
        ErrorCode, Request, RequestId, Response, ResponseEnvelope, DEFAULT_TTL_MS, PROTOCOL_VERSION,
    };

    use super::{run_shutdown_sequence, LeaseClient, LeaseError, LeaseReceipt, RenewalHandle};
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
}
