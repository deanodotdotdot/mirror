//! Read mounted-workspace context inside the guest, never on its host.

use std::io;
use std::path::Path;

use mirror_agent::system_prompt::{gather_environment_info, load_workspace_context};

/// Print bounded instructions and skills from an existing workspace.
pub fn print_workspace_context(workspace: &Path) -> io::Result<()> {
    let context = load_workspace_context(workspace, workspace)?;
    let environment = gather_environment_info(workspace);
    serde_json::to_writer(io::stdout().lock(), &(environment, context)).map_err(io::Error::other)
}
