//! Core logic for Kamal GUI: project registry, git mirrors/worktrees,
//! 1Password secrets, kamal invocation, container status and run history.
//!
//! Shared by the Tauri app and the `kgui` CLI.

pub mod build_info;
pub mod cli;
pub mod config;
pub mod env;
pub mod git;
pub mod github;
pub mod history;
pub mod images;
pub mod kamal;
pub mod link;
pub mod project;
pub mod runner;
pub mod secrets;
pub mod settings;
pub mod status;
