//! Claude subscription contracts and Mirror-owned credential authority.

pub mod auth;
mod contracts;
mod history;
pub mod metadata;
mod oauth;
mod private_files;
pub mod profile;
mod stream;
mod wire;

pub use contracts::*;
pub use history::*;
pub use oauth::*;
pub use profile::ClaudeOperation;
pub use wire::{claude_model_capabilities, ClaudeModelCapabilities};

#[cfg(test)]
mod tests;

pub(crate) mod session;
pub use session::ClaudeSession;
