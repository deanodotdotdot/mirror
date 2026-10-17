//! Claude subscription contracts and Mirror-owned credential authority.

pub mod auth;
mod contracts;
mod history;
pub mod metadata;
mod private_files;
pub mod profile;

pub use contracts::*;
pub use history::*;
pub use profile::ClaudeOperation;

#[cfg(test)]
mod tests;
