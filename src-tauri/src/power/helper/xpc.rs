#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use jarvis_power_core::protocol::{
        encode_response, Request, RequestEnvelope, RequestId, Response, ResponseEnvelope,
        MAX_FRAME_BYTES, PROTOCOL_VERSION,
    };

    use super::{XpcClient, XpcClientError, XpcTransport, XpcTransportError};

    struct FakeTransport {
        response: Mutex<Vec<u8>>,
    }

    impl XpcTransport for FakeTransport {
        fn request(
            &self,
            _payload: &[u8],
            response: &mut [u8],
        ) -> Result<usize, XpcTransportError> {
            let bytes = self.response.lock().unwrap();
            let copied = bytes.len().min(response.len());
            response[..copied].copy_from_slice(&bytes[..copied]);
            Ok(bytes.len())
        }
    }

    fn request() -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::parse("018f0000-0000-7000-8000-000000000001").unwrap(),
            request: Request::Status,
        }
    }

    fn response(request_id: RequestId) -> Vec<u8> {
        encode_response(&ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Response::Status {
                active_leases: 0,
                mutation_active: false,
                recovery_required: false,
            },
        })
        .unwrap()
    }

    #[test]
    fn production_client_uses_one_bounded_payload_and_validates_request_id() {
        let request = request();
        let client = XpcClient::new(FakeTransport {
            response: Mutex::new(response(request.request_id.clone())),
        });
        let reply = client.request(&request).expect("bounded response");
        assert_eq!(reply.request_id, request.request_id);
        assert!(matches!(reply.response, Response::Status { .. }));
    }

    #[test]
    fn oversized_or_mismatched_native_response_fails_closed() {
        let request = request();
        let oversized = XpcClient::new(FakeTransport {
            response: Mutex::new(vec![0_u8; MAX_FRAME_BYTES + 1]),
        });
        assert_eq!(
            oversized.request(&request),
            Err(XpcClientError::InvalidPayload)
        );

        let mismatch = XpcClient::new(FakeTransport {
            response: Mutex::new(response(
                RequestId::parse("018f0000-0000-7000-8000-000000000002").unwrap(),
            )),
        });
        assert_eq!(
            mismatch.request(&request),
            Err(XpcClientError::ResponseRequestIdMismatch)
        );
    }
}
