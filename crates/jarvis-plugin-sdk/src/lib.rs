#![forbid(unsafe_code)]

mod client;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use jarvis_plugin_protocol::process::PLUGIN_PROCESS_PROTOCOL;

pub use client::{PluginClient, Transport};

const REQUIRED_KEYS: [&str; 6] = [
    "JARVIS_PLUGIN_ID",
    "JARVIS_PLUGIN_TOKEN",
    "JARVIS_PLUGIN_PROTOCOL",
    "JARVIS_PLUGIN_PACKAGE_DIGEST",
    "JARVIS_PLUGIN_ACTIVATION_GENERATION",
    "JARVIS_SOCKET",
];

#[derive(Clone, PartialEq, Eq)]
pub struct PluginEnvironment {
    pub plugin_id: String,
    token: String,
    pub protocol_version: u32,
    pub package_digest: String,
    pub activation_generation: u64,
    pub socket: PathBuf,
}

impl PluginEnvironment {
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, SdkError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let values = pairs
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
            .collect::<BTreeMap<_, _>>();

        for key in REQUIRED_KEYS {
            if values.get(key).map_or(true, |value| value.is_empty()) {
                return Err(SdkError::MissingEnvironment(key));
            }
        }

        let protocol_version = parse_number::<u32>(&values, "JARVIS_PLUGIN_PROTOCOL")?;
        if protocol_version != PLUGIN_PROCESS_PROTOCOL {
            return Err(SdkError::IncompatibleProtocol {
                received: protocol_version,
                supported: PLUGIN_PROCESS_PROTOCOL,
            });
        }
        let activation_generation =
            parse_number::<u64>(&values, "JARVIS_PLUGIN_ACTIVATION_GENERATION")?;

        Ok(Self {
            plugin_id: values["JARVIS_PLUGIN_ID"].clone(),
            token: values["JARVIS_PLUGIN_TOKEN"].clone(),
            protocol_version,
            package_digest: values["JARVIS_PLUGIN_PACKAGE_DIGEST"].clone(),
            activation_generation,
            socket: PathBuf::from(&values["JARVIS_SOCKET"]),
        })
    }

    pub fn from_process() -> Result<Self, SdkError> {
        let mut values = Vec::with_capacity(REQUIRED_KEYS.len());
        for key in REQUIRED_KEYS {
            let value = std::env::var(key).map_err(|_| SdkError::MissingEnvironment(key))?;
            values.push((key, value));
        }
        Self::from_pairs(values)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn assert_hello_identity(
        &self,
        plugin_id: &str,
        activation_generation: u64,
    ) -> Result<(), SdkError> {
        if plugin_id != self.plugin_id || activation_generation != self.activation_generation {
            return Err(SdkError::IdentityMismatch);
        }
        Ok(())
    }
}

impl fmt::Debug for PluginEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginEnvironment")
            .field("plugin_id", &self.plugin_id)
            .field("token", &"<redacted>")
            .field("protocol_version", &self.protocol_version)
            .field("package_digest", &self.package_digest)
            .field("activation_generation", &self.activation_generation)
            .field("socket", &self.socket)
            .finish()
    }
}

fn parse_number<T>(values: &BTreeMap<String, String>, key: &'static str) -> Result<T, SdkError>
where
    T: std::str::FromStr,
{
    values[key]
        .parse::<T>()
        .map_err(|_| SdkError::InvalidEnvironment(key))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdkError {
    MissingEnvironment(&'static str),
    InvalidEnvironment(&'static str),
    IncompatibleProtocol { received: u32, supported: u32 },
    IdentityMismatch,
    Transport,
}

impl SdkError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingEnvironment(_) => "plugin_environment_missing",
            Self::InvalidEnvironment(_) => "plugin_environment_invalid",
            Self::IncompatibleProtocol { .. } => "plugin_protocol_incompatible",
            Self::IdentityMismatch => "plugin_identity_mismatch",
            Self::Transport => "plugin_transport",
        }
    }
}

impl fmt::Display for SdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment(key) => {
                write!(
                    formatter,
                    "required plugin environment value is missing: {key}"
                )
            }
            Self::InvalidEnvironment(key) => {
                write!(formatter, "plugin environment value is invalid: {key}")
            }
            Self::IncompatibleProtocol {
                received,
                supported,
            } => write!(
                formatter,
                "plugin protocol {received} is incompatible; supported protocol is {supported}"
            ),
            Self::IdentityMismatch => formatter.write_str("plugin hello identity does not match"),
            Self::Transport => formatter.write_str("plugin transport failed"),
        }
    }
}

impl Error for SdkError {}
