//! familiar-core: the always-on agent daemon. Hosted by the Tauri desktop app or the headless `familiard` binary.

pub mod browser;
pub mod claude;
pub mod codex;
pub mod config;
pub mod daemon;
pub mod db;
pub mod mcp;
pub mod models;
pub mod permissions;
pub mod reviewer;
pub mod runner;
pub mod skills;
pub mod storage;
pub mod telegram;
pub mod text;
pub mod tools;
pub mod workspace;

pub use config::Config;
pub use daemon::{Signal, run, run_with_signals};
