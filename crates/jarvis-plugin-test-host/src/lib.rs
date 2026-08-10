#![forbid(unsafe_code)]

pub mod ui;

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use jarvis_plugin_protocol::process::{
    CommandRequest, PluginFrame, PluginHello, PLUGIN_PROCESS_PROTOCOL,
};
use serde_json::Value;

pub const MAX_QUEUED_COMMANDS: usize = 256;

const FIXTURE_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedCommand {
    pub sequence: u64,
    pub request: CommandRequest,
}

pub struct TestHost {
    plugin_id: String,
    package_digest: String,
    activation_generation: u64,
    registered: bool,
    next_sequence: u64,
    commands: VecDeque<QueuedCommand>,
    lifecycle_frames: Vec<PluginFrame>,
}

impl TestHost {
    pub fn new(plugin_id: impl Into<String>, activation_generation: u64) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            package_digest: FIXTURE_DIGEST.to_string(),
            activation_generation,
            registered: false,
            next_sequence: 0,
            commands: VecDeque::new(),
            lifecycle_frames: Vec::new(),
        }
    }

    pub fn register_fixture(&mut self, activation_generation: u64) -> Result<(), ContractError> {
        let plugin_id = self.plugin_id.clone();
        self.register_plugin_fixture(&plugin_id, activation_generation)
    }

    pub fn register_plugin_fixture(
        &mut self,
        plugin_id: &str,
        activation_generation: u64,
    ) -> Result<(), ContractError> {
        self.register(&PluginHello {
            protocol_version: PLUGIN_PROCESS_PROTOCOL,
            plugin_id: plugin_id.to_string(),
            pid: 42,
            package_digest: self.package_digest.clone(),
            activation_generation,
        })
    }

    pub fn register(&mut self, hello: &PluginHello) -> Result<(), ContractError> {
        if hello.protocol_version != PLUGIN_PROCESS_PROTOCOL {
            return Err(ContractError::IncompatibleProtocol);
        }
        if hello.activation_generation != self.activation_generation {
            return Err(ContractError::StaleActivationGeneration);
        }
        if hello.plugin_id != self.plugin_id {
            return Err(ContractError::IdentityMismatch);
        }
        if hello.package_digest != self.package_digest {
            return Err(ContractError::PackageDigestMismatch);
        }
        self.registered = true;
        Ok(())
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    pub fn package_digest(&self) -> &str {
        &self.package_digest
    }

    pub fn queue_command(
        &mut self,
        command: impl Into<String>,
        args: Value,
    ) -> Result<u64, ContractError> {
        if !self.registered {
            return Err(ContractError::NotRegistered);
        }
        if self.commands.len() >= MAX_QUEUED_COMMANDS {
            return Err(ContractError::CommandQueueFull);
        }
        self.next_sequence += 1;
        let sequence = self.next_sequence;
        let request = CommandRequest::new(
            self.plugin_id.clone(),
            self.package_digest.clone(),
            self.activation_generation,
            format!("test-{sequence}"),
            command,
            args,
        )
        .map_err(|_| ContractError::InvalidRequest)?;
        self.commands.push_back(QueuedCommand { sequence, request });
        Ok(sequence)
    }

    pub fn commands_after(&self, sequence: u64) -> Vec<QueuedCommand> {
        self.commands
            .iter()
            .filter(|command| command.sequence > sequence)
            .cloned()
            .collect()
    }

    pub fn record_lifecycle(&mut self, frame: PluginFrame) -> Result<(), ContractError> {
        if !self.registered {
            return Err(ContractError::NotRegistered);
        }
        let (plugin_id, package_digest, activation_generation) = match &frame {
            PluginFrame::ActivationResponse(response) => (
                response.plugin_id.as_str(),
                response.package_digest.as_str(),
                response.activation_generation,
            ),
            PluginFrame::Heartbeat(heartbeat) => (
                heartbeat.plugin_id.as_str(),
                heartbeat.package_digest.as_str(),
                heartbeat.activation_generation,
            ),
            PluginFrame::ShutdownAck(ack) => (
                ack.plugin_id.as_str(),
                ack.package_digest.as_str(),
                ack.activation_generation,
            ),
            _ => return Err(ContractError::InvalidLifecycleFrame),
        };
        if plugin_id != self.plugin_id || activation_generation != self.activation_generation {
            return Err(ContractError::IdentityMismatch);
        }
        if package_digest != self.package_digest {
            return Err(ContractError::PackageDigestMismatch);
        }
        self.lifecycle_frames.push(frame);
        Ok(())
    }

    pub fn lifecycle_frames(&self) -> &[PluginFrame] {
        &self.lifecycle_frames
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractError {
    IncompatibleProtocol,
    StaleActivationGeneration,
    IdentityMismatch,
    PackageDigestMismatch,
    NotRegistered,
    CommandQueueFull,
    InvalidRequest,
    InvalidLifecycleFrame,
}

impl ContractError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::IncompatibleProtocol => "plugin_protocol_incompatible",
            Self::StaleActivationGeneration => "stale_activation_generation",
            Self::IdentityMismatch => "plugin_identity_mismatch",
            Self::PackageDigestMismatch => "package_digest_mismatch",
            Self::NotRegistered => "plugin_not_registered",
            Self::CommandQueueFull => "command_queue_full",
            Self::InvalidRequest => "invalid_command_request",
            Self::InvalidLifecycleFrame => "invalid_lifecycle_frame",
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Error for ContractError {}
