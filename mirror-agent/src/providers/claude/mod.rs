//! Shared Claude subscription contracts. No credential store or network client.

mod contracts;
mod history;
mod oauth;
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
