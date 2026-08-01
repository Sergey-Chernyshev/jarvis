use std::error::Error;
#[cfg(feature = "power-helper-dev")]
use std::ffi::OsStr;
use std::fmt;
use std::io;

use jarvis_power_core::protocol::{ProtocolError, Request, ResponseEnvelope};

#[cfg(feature = "power-helper-dev")]
use super::dev_uds::DevUdsClient;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelperTrust {
    DevelopmentOnly,
}

impl HelperTrust {
    /// Development transport evidence is deliberately non-authoritative.
    ///
    /// A same-UID local process can create an indistinguishable Unix listener,
    /// so this value must never unlock production helper behavior.
    pub(crate) const fn authorizes_production(self) -> bool {
        false
    }
}

const _: () = assert!(!HelperTrust::DevelopmentOnly.authorizes_production());

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HelperReply {
    pub(crate) response: ResponseEnvelope,
    pub(crate) trust: HelperTrust,
}

#[derive(Debug)]
pub(crate) enum HelperClientError {
    Deadline,
    InvalidFrame,
    PeerRejected,
    Io(io::Error),
    Protocol(ProtocolError),
    RandomnessUnavailable,
    RequestIdUnavailable,
    ResponseRequestIdMismatch,
}

impl fmt::Display for HelperClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => formatter.write_str("power-helper development I/O deadline expired"),
            Self::InvalidFrame => formatter.write_str("power-helper development frame is invalid"),
            Self::PeerRejected => {
                formatter.write_str("power-helper development peer identity is inconsistent")
            }
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
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) trait HelperClient {
    fn send(&self, request: Request) -> Result<HelperReply, HelperClientError>;

    fn trust(&self) -> HelperTrust;
}

#[cfg(feature = "power-helper-dev")]
pub(crate) fn select_for_runtime_value(value: Option<&OsStr>) -> Option<DevUdsClient> {
    (value == Some(OsStr::new("1"))).then(|| DevUdsClient::new(crate::util::jarvis_dir()))
}

pub(super) fn map_io_error(error: io::Error) -> HelperClientError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HelperClientError::Deadline,
        io::ErrorKind::UnexpectedEof => HelperClientError::InvalidFrame,
        _ => HelperClientError::Io(error),
    }
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
