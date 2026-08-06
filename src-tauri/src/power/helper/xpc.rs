use std::fmt;

use jarvis_power_core::protocol::{
    decode_response, encode_request, ProtocolError, RequestEnvelope, ResponseEnvelope,
    MAX_FRAME_BYTES,
};

const REQUEST_TIMEOUT_MS: u32 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XpcTransportError {
    Unavailable,
    Deadline,
    InvalidPayload,
}

pub(crate) trait XpcTransport: Send + Sync {
    fn request(&self, payload: &[u8], response: &mut [u8]) -> Result<usize, XpcTransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XpcClientError {
    Transport(XpcTransportError),
    InvalidPayload,
    Protocol(ProtocolError),
    ResponseRequestIdMismatch,
}

impl fmt::Display for XpcClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => {
                write!(formatter, "production XPC transport failed: {error:?}")
            }
            Self::InvalidPayload => formatter.write_str("production XPC payload is invalid"),
            Self::Protocol(error) => write!(formatter, "production XPC protocol failed: {error}"),
            Self::ResponseRequestIdMismatch => {
                formatter.write_str("production XPC response request id does not match")
            }
        }
    }
}

impl std::error::Error for XpcClientError {}

pub(crate) struct XpcClient<T> {
    transport: T,
}

impl<T> XpcClient<T>
where
    T: XpcTransport,
{
    pub(crate) fn new(transport: T) -> Self {
        Self { transport }
    }

    pub(crate) fn request(
        &self,
        request: &RequestEnvelope,
    ) -> Result<ResponseEnvelope, XpcClientError> {
        let payload = encode_request(request).map_err(XpcClientError::Protocol)?;
        if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
            return Err(XpcClientError::InvalidPayload);
        }
        let mut response = [0_u8; MAX_FRAME_BYTES];
        let response_length = self
            .transport
            .request(&payload, &mut response)
            .map_err(XpcClientError::Transport)?;
        if response_length == 0 || response_length > response.len() {
            return Err(XpcClientError::InvalidPayload);
        }
        let response =
            decode_response(&response[..response_length]).map_err(XpcClientError::Protocol)?;
        if response.request_id != request.request_id {
            return Err(XpcClientError::ResponseRequestIdMismatch);
        }
        Ok(response)
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct NativeXpcTransport;

#[cfg(target_os = "macos")]
impl XpcTransport for NativeXpcTransport {
    fn request(&self, payload: &[u8], response: &mut [u8]) -> Result<usize, XpcTransportError> {
        let mut response_length = 0_usize;
        // SAFETY: both byte slices are valid for the supplied lengths, the
        // output length is a live pointer, and the native bridge copies data
        // synchronously before returning.
        let status = unsafe {
            jarvis_power_helper_request(
                payload.as_ptr(),
                payload.len(),
                response.as_mut_ptr(),
                response.len(),
                &mut response_length,
                REQUEST_TIMEOUT_MS,
            )
        };
        match status {
            0 => Ok(response_length),
            2 => Err(XpcTransportError::Deadline),
            3 => Err(XpcTransportError::InvalidPayload),
            _ => Err(XpcTransportError::Unavailable),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct NativeXpcTransport;

#[cfg(not(target_os = "macos"))]
impl XpcTransport for NativeXpcTransport {
    fn request(&self, _payload: &[u8], _response: &mut [u8]) -> Result<usize, XpcTransportError> {
        Err(XpcTransportError::Unavailable)
    }
}

const _: NativeXpcTransport = NativeXpcTransport;
const _: fn(NativeXpcTransport) -> XpcClient<NativeXpcTransport> = XpcClient::new;
const _: fn(
    &XpcClient<NativeXpcTransport>,
    &RequestEnvelope,
) -> Result<ResponseEnvelope, XpcClientError> = XpcClient::request;

#[cfg(target_os = "macos")]
extern "C" {
    fn jarvis_power_helper_request(
        request: *const u8,
        request_length: usize,
        response: *mut u8,
        response_capacity: usize,
        response_length: *mut usize,
        timeout_ms: u32,
    ) -> i32;
}

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
