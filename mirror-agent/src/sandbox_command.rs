//! Native sandbox lifecycle and the CLI's raw process adapter.

use std::error::Error;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use mirror_sandbox::{SandboxProvider, SandboxStream, SandboxTransport, SandboxTransportError};
use mirror_sandbox_microsandbox::{sandbox_name, Msb, MsbFailure};
use uuid::Uuid;

use crate::sandbox_config::{quarantine_path, SandboxConfig};

#[derive(Parser)]
#[command(name = "mirror-agent sandbox")]
enum SandboxCommand {
    /// Prepare the shared VM before extension startup.
    Prepare { config: String, workspace: PathBuf },
    /// Stream a process; keep its completion evidence outside the guest mount.
    Exec {
        config: String,
        workspace: PathBuf,
        outcome: PathBuf,
        command: String,
    },
    /// Stop the VM and prevent reuse after uncertain execution.
    Quarantine { id: Uuid },
    /// Stop the VM, retaining its disk for the next worker.
    Stop { id: Uuid },
    /// Delete the VM; leave workspace deletion to the CLI.
    Delete { id: Uuid },
}

/// Run `mirror-agent sandbox …`; missing VMs are already stopped or deleted.
pub async fn run() -> Result<(), Box<dyn Error>> {
    let command = SandboxCommand::parse_from(
        std::iter::once("mirror-agent sandbox".to_string()).chain(std::env::args().skip(2)),
    );
    let result = match command {
        SandboxCommand::Prepare { config, workspace } => {
            let config: SandboxConfig = serde_json::from_str(&config)?;
            return Ok(config.start(config.id, &workspace).await?.prepare().await?);
        }
        SandboxCommand::Exec {
            config,
            workspace,
            outcome,
            command,
        } => {
            let result = stream(&config, &workspace, command)
                .await
                .map_err(|error| error.to_string());
            std::fs::write(outcome, serde_json::to_vec(&result)?)?;
            // Tokio's stdin reader may still be blocked after a guest exits early.
            std::process::exit(0);
        }
        SandboxCommand::Quarantine { id } => {
            let path = quarantine_path(id)?;
            std::fs::create_dir_all(path.parent().ok_or("quarantine path has no parent")?)?;
            let recorded = std::fs::write(path, b"quarantined\n");
            let stopped = Msb::locate().await?.stop(&sandbox_name(id)).await;
            recorded?;
            stopped
        }
        SandboxCommand::Stop { id } => Msb::locate().await?.stop(&sandbox_name(id)).await,
        SandboxCommand::Delete { id } => Msb::locate().await?.remove(&sandbox_name(id)).await,
    };
    match result {
        Ok(()) | Err(MsbFailure::SandboxMissing) => Ok(()),
        Err(failure) => Err(failure.into()),
    }
}

async fn stream(config: &str, workspace: &Path, command: String) -> Result<i32, Box<dyn Error>> {
    let config: SandboxConfig = serde_json::from_str(config)?;
    let provider = config.provider(workspace).await?;
    // Only preparation may resume a VM; a process launcher must never revive it.
    let name = sandbox_name(config.id);
    let result = provider
        .stream(
            &name,
            SandboxStream {
                command,
                workdir: Some(
                    workspace
                        .to_str()
                        .ok_or("workspace is not UTF-8")?
                        .to_string(),
                ),
                timeout: Duration::from_secs(24 * 60 * 60),
                stdin: Some(Box::pin(tokio::io::stdin())),
                stdout: Box::pin(tokio::io::stdout()),
                stderr: Some(Box::pin(tokio::io::stderr())),
            },
        )
        .await;
    if matches!(result, Err(SandboxTransportError::OutcomeUnknown(_))) {
        provider.quarantine(&name).await?;
    }
    Ok(result?.exit_code)
}
