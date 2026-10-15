//! Connections to sandboxd in a running microVM through `msb`.
use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use mirror_sandbox::run_sandbox_command;
use mirror_sandbox::SandboxStream;
use mirror_sandbox::SandboxStreamOutcome;
use mirror_sandbox::SandboxTransport;
use mirror_sandbox::SandboxTransportError;
use mirror_sandbox::SandboxdStream;
use mirror_sandbox::SANDBOXD_PATH;
use mirror_sandbox::SANDBOXD_REFRESH_FILE_CACHE_VARIABLE;
use mirror_sandbox::SANDBOXD_WORKSPACE_VARIABLE;

use crate::MicrosandboxProvider;
use crate::MsbExecStream;
use crate::MsbFailure;
use crate::SandboxStatus;

impl MicrosandboxProvider {
    /// `msb exec` on a stopped sandbox boots a throwaway VM rather than
    /// failing, so every command first confirms the sandbox is running.
    async fn require_running(&self, name: &str) -> Result<(), SandboxTransportError> {
        self.require_not_quarantined()
            .map_err(SandboxTransportError::Failed)?;
        let inspection = self
            .inspect(name)
            .await
            .map_err(|failure| match failure {
                MsbFailure::SandboxMissing => gone(name),
                MsbFailure::TimedOut(detail) => SandboxTransportError::NotReady(detail),
                failure @ (MsbFailure::SandboxExists | MsbFailure::Failed(_)) => {
                    SandboxTransportError::Failed(failure.to_string())
                }
            })?
            .ok_or_else(|| gone(name))?;
        match inspection.status {
            SandboxStatus::Running => self
                .confirm_isolation(name, &inspection)
                .map_err(SandboxTransportError::Failed),
            SandboxStatus::Stopped | SandboxStatus::Crashed | SandboxStatus::Paused => {
                Err(SandboxTransportError::Suspended(format!(
                    "sandbox {name} is {:?}",
                    inspection.status
                )))
            }
            status => Err(SandboxTransportError::NotReady(format!(
                "sandbox {name} is {status:?}"
            ))),
        }
    }
}

#[async_trait]
impl SandboxTransport for MicrosandboxProvider {
    async fn connect(
        &self,
        name: &str,
        workspace: &str,
    ) -> Result<SandboxdStream, SandboxTransportError> {
        // `sandboxd` runs as root in the guest, so its tools are confined only
        // by the workspace it is given.
        if Path::new(workspace) != self.config.workspace {
            return Err(SandboxTransportError::Failed(format!(
                "sandbox {name} mounts {:?}, not the workspace {workspace:?}",
                self.config.workspace
            )));
        }
        self.require_running(name).await?;
        let process = self
            .msb
            .command()
            .args(["exec", "--quiet", "--stream", "--env"])
            .arg(format!("{SANDBOXD_WORKSPACE_VARIABLE}={workspace}"))
            .arg("--env")
            .arg(format!("{SANDBOXD_REFRESH_FILE_CACHE_VARIABLE}=1"))
            .args([name, "--", SANDBOXD_PATH, "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Launcher SIGTERM must reach the RPC client, not sever its connection.
            .process_group(0)
            .spawn()
            .map_err(|error| {
                SandboxTransportError::Failed(format!(
                    "msb exec into {name} could not start: {error}"
                ))
            })?;
        Ok(SandboxdStream::split(MsbExecStream::new(process)?))
    }

    async fn stream(
        &self,
        name: &str,
        request: SandboxStream<'_>,
    ) -> Result<SandboxStreamOutcome, SandboxTransportError> {
        let workspace = self
            .config
            .workspace
            .to_str()
            .ok_or_else(|| SandboxTransportError::Failed("workspace is not UTF-8".into()))?;
        run_sandbox_command(self.connect(name, workspace), request).await
    }
}

fn gone(name: &str) -> SandboxTransportError {
    SandboxTransportError::Gone(format!("sandbox {name} does not exist"))
}

#[cfg(test)]
#[path = "microsandbox_transport_test.rs"]
mod tests;
