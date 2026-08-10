use std::error::Error;
#[cfg(feature = "power-helper-dev")]
use std::ffi::OsStr;
use std::fmt;
#[cfg(feature = "power-helper-dev")]
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use jarvis_power_core::protocol::{
    ProtocolError, Request, RequestEnvelope, RequestId, ResponseEnvelope, PROTOCOL_VERSION,
};

#[cfg(feature = "power-helper-dev")]
use super::dev_uds::DevUdsClient;
use super::xpc::{NativeXpcTransport, XpcClient, XpcClientError, XpcTransportError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelperTrust {
    ProductionAttested,
    DevelopmentOnly,
}

impl HelperTrust {
    /// Development transport evidence is deliberately non-authoritative.
    ///
    /// A same-UID local process can create an indistinguishable Unix listener,
    /// so this value must never unlock production helper behavior.
    pub(crate) const fn authorizes_production(self) -> bool {
        matches!(self, Self::ProductionAttested)
    }
}

const _: () = assert!(!HelperTrust::DevelopmentOnly.authorizes_production());
const _: () = assert!(HelperTrust::ProductionAttested.authorizes_production());

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HelperReply {
    pub(crate) response: ResponseEnvelope,
    pub(crate) trust: HelperTrust,
}

#[derive(Debug)]
pub(crate) enum HelperClientError {
    Unavailable,
    Deadline,
    InvalidFrame,
    #[cfg(feature = "power-helper-dev")]
    PeerRejected,
    #[cfg(feature = "power-helper-dev")]
    Io(io::Error),
    Protocol(ProtocolError),
    RandomnessUnavailable,
    RequestIdUnavailable,
    ResponseRequestIdMismatch,
}

impl fmt::Display for HelperClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("power-helper is unavailable"),
            Self::Deadline => formatter.write_str("power-helper I/O deadline expired"),
            Self::InvalidFrame => formatter.write_str("power-helper frame is invalid"),
            #[cfg(feature = "power-helper-dev")]
            Self::PeerRejected => {
                formatter.write_str("power-helper development peer identity is inconsistent")
            }
            #[cfg(feature = "power-helper-dev")]
            Self::Io(error) => write!(formatter, "power-helper development I/O failed: {error}"),
            Self::Protocol(error) => write!(formatter, "power-helper protocol failed: {error}"),
            Self::RandomnessUnavailable => {
                formatter.write_str("secure randomness is unavailable for the request id")
            }
            Self::RequestIdUnavailable => {
                formatter.write_str("a UUIDv7 request id could not be generated")
            }
            Self::ResponseRequestIdMismatch => {
                formatter.write_str("power-helper response request id does not match")
            }
        }
    }
}

impl Error for HelperClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(feature = "power-helper-dev")]
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) trait HelperClient: Send + Sync {
    fn send(&self, request: Request) -> Result<HelperReply, HelperClientError>;

    fn trust(&self) -> HelperTrust;
}

pub(crate) struct ProductionXpcClient {
    client: XpcClient<NativeXpcTransport>,
}

impl ProductionXpcClient {
    pub(crate) fn new() -> Self {
        Self {
            client: XpcClient::new(NativeXpcTransport),
        }
    }
}

impl HelperClient for ProductionXpcClient {
    fn send(&self, request: Request) -> Result<HelperReply, HelperClientError> {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: next_request_id()?,
            request,
        };
        let response = self.client.request(&request).map_err(map_xpc_error)?;
        Ok(HelperReply {
            response,
            trust: self.trust(),
        })
    }

    fn trust(&self) -> HelperTrust {
        HelperTrust::ProductionAttested
    }
}

fn map_xpc_error(error: XpcClientError) -> HelperClientError {
    match error {
        XpcClientError::Transport(XpcTransportError::Unavailable) => HelperClientError::Unavailable,
        XpcClientError::Transport(XpcTransportError::Deadline) => HelperClientError::Deadline,
        XpcClientError::Transport(XpcTransportError::InvalidPayload)
        | XpcClientError::InvalidPayload => HelperClientError::InvalidFrame,
        XpcClientError::Protocol(error) => HelperClientError::Protocol(error),
        XpcClientError::ResponseRequestIdMismatch => HelperClientError::ResponseRequestIdMismatch,
    }
}

#[cfg(feature = "power-helper-dev")]
pub(crate) fn select_for_runtime_value(value: Option<&OsStr>) -> Option<DevUdsClient> {
    (value == Some(OsStr::new("1"))).then(|| DevUdsClient::new(crate::util::jarvis_dir()))
}

#[cfg(feature = "power-helper-dev")]
pub(super) fn map_io_error(error: io::Error) -> HelperClientError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HelperClientError::Deadline,
        io::ErrorKind::UnexpectedEof => HelperClientError::InvalidFrame,
        _ => HelperClientError::Io(error),
    }
}

pub(super) fn next_request_id() -> Result<RequestId, HelperClientError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HelperClientError::RequestIdUnavailable)?
        .as_millis();
    let milliseconds =
        u64::try_from(milliseconds).map_err(|_| HelperClientError::RequestIdUnavailable)?;
    if milliseconds > 0x0000_ffff_ffff_ffff {
        return Err(HelperClientError::RequestIdUnavailable);
    }

    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| HelperClientError::RandomnessUnavailable)?;
    let timestamp = milliseconds.to_be_bytes();
    bytes[..6].copy_from_slice(&timestamp[2..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    RequestId::parse(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    ))
    .map_err(|_| HelperClientError::RequestIdUnavailable)
}

#[cfg(all(test, feature = "power-helper-dev"))]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    use super::{select_for_runtime_value, HelperClient, HelperTrust};

    #[test]
    fn app_selection_requires_compile_feature_plus_exact_runtime_flag() {
        assert_eq!(
            select_for_runtime_value(Some(OsStr::new("1")))
                .unwrap()
                .trust(),
            HelperTrust::DevelopmentOnly
        );
        for rejected in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("1 ")),
            Some(OsStr::new("true")),
            Some(OsStr::new("0")),
        ] {
            assert!(select_for_runtime_value(rejected).is_none());
        }
        let non_unicode = OsString::from_vec(vec![b'1', 0xff]);
        assert!(select_for_runtime_value(Some(&non_unicode)).is_none());
        assert!(!HelperTrust::DevelopmentOnly.authorizes_production());
    }
}
