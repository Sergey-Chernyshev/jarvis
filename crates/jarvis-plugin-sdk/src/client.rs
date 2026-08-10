use jarvis_plugin_protocol::process::{PluginFrame, PluginHello, PLUGIN_PROCESS_PROTOCOL};

use crate::{PluginEnvironment, SdkError};

pub trait Transport {
    fn send(&mut self, frame: &PluginFrame) -> Result<(), String>;
    fn receive(&mut self) -> Result<PluginFrame, String>;
}

pub struct PluginClient<T> {
    environment: PluginEnvironment,
    transport: T,
}

impl<T: Transport> PluginClient<T> {
    pub fn new(environment: PluginEnvironment, transport: T) -> Self {
        Self {
            environment,
            transport,
        }
    }

    pub fn send_hello(&mut self, pid: u32) -> Result<(), SdkError> {
        let hello = PluginHello {
            protocol_version: PLUGIN_PROCESS_PROTOCOL,
            plugin_id: self.environment.plugin_id.clone(),
            pid,
            package_digest: self.environment.package_digest.clone(),
            activation_generation: self.environment.activation_generation,
        };
        self.transport
            .send(&PluginFrame::PluginHello(hello))
            .map_err(|_| SdkError::Transport)
    }

    pub fn receive(&mut self) -> Result<PluginFrame, SdkError> {
        self.transport.receive().map_err(|_| SdkError::Transport)
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}
