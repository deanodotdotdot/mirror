//! Shared Claude subscription contracts. No credential store or network client.

mod contracts;
mod history;
pub mod profile;

pub use contracts::*;
pub use history::*;
pub use profile::ClaudeOperation;

#[cfg(test)]
mod tests;
