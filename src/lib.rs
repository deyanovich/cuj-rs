//! cuj — reference implementation of the cuj protocol
//! (1.0-draft.5 line; Rust).
//!
//! A note-jotting and journaling CLI whose annotation language is
//! Jot Syntax and whose storage is a metatheca vault with the
//! taxopsis / logopsis / semopsis retrieval extensions. cuj holds
//! no private index: reads are metatheca fact queries plus the
//! extension chains. See the protocol specification for normative
//! behavior.

pub mod app;
pub mod arbor;
pub mod binding;
pub mod client;
pub mod error;
pub mod ext;
pub mod facts;
pub mod import;
pub mod obsidian;
pub mod queries;
pub mod read;
pub mod reads;

pub use app::{App, Opts, RefArg};
pub use client::ClientConfig;
pub use error::{Error, Result};
