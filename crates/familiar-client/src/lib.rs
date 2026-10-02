//! Typed async client for the Familiar HTTP API, mirroring `apps/web/src/lib`.
//!
//! No UI dependency. Everything async runs on tokio (reqwest needs it); the
//! caller supplies the runtime. See [`Client`], [`types`], [`stream`].

mod cache;
mod client;
mod error;
pub mod stream;
pub mod types;

pub use client::{Client, WakingCallback};
pub use error::ApiError;
pub use stream::{Coalescer, Delta, DeltaKind, LiveEvent, Notice, SseMessage, SseParser, resync_wins};
pub use types::*;
