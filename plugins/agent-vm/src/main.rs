use std::sync::Arc;

use jarvis_agent_vm_plugin::host::{HostApi, HostClient, UnixSocketTransport};
use jarvis_agent_vm_plugin::plugin::{public_error, Dispatcher, PluginEnvironment};
use jarvis_agent_vm_plugin::run_executor::SystemTurnExecutor;
use jarvis_agent_vm_plugin::run_store::RunStore;
use jarvis_agent_vm_plugin::run_supervisor::RunSupervisor;
use jarvis_agent_vm_plugin::runner::SystemRunner;
use jarvis_agent_vm_plugin::runtime_paths::RuntimePaths;
use jarvis_agent_vm_plugin::service::{AgentVmService, Toolchain};

fn main() {
    if let Err(error) = run() {
        eprintln!("[agent-vm] {}", public_error(&error));
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let environment = PluginEnvironment::from_current()?;
    let paths = RuntimePaths::from_socket(&environment.socket)?;
    paths.create_private_dirs()?;

    let host = HostClient::new(
        UnixSocketTransport::new(environment.socket),
        environment.token,
        environment.protocol_version,
    );
    host.register(std::process::id())?;

    let tools = Toolchain::discover()?;
    let service =
        AgentVmService::with_system_bootstrap(SystemRunner, paths.clone(), tools.clone())?;
    let executor = Arc::new(SystemTurnExecutor::new(tools.limactl, paths.command_env()));
    let supervisor = RunSupervisor::new(host.clone(), RunStore::new(paths.runs_root), executor);
    let mut dispatcher = Dispatcher::with_supervisor(service, host.clone(), supervisor.clone());
    dispatcher.refresh_inventory()?;
    supervisor.recover()?;

    let mut after = 0;
    loop {
        let batch = host.poll(after)?;
        for event in batch.events {
            dispatcher.process(event)?;
        }
        after = after.max(batch.next_seq);
    }
}
