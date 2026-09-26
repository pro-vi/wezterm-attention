//! # wezterm-attention
//!
//! Records what a pane's agent is doing, against a pane identity that survives
//! detach, reattach and multiple mux sockets. The WezTerm plugin reads those
//! records to tint a tab; other programs read them through the `attention` CLI.
//!
//! ## What is supported
//!
//! The supported interface of this project is the `attention` command -- its
//! JSON envelopes, diagnostic codes and exit codes, which
//! `docs/consumer-guide.md` specifies -- and the WezTerm plugin's documented
//! Lua API. The Rust items this crate exports are not supported for use outside
//! this repository and may change in any release. They are public because the
//! `attention` binary and this repository's integration tests link the library.
//! `docs/accepted-limitations.md` records why that is documented rather than
//! enforced.

pub mod consumer;
pub mod hook_content;
pub mod identity;
mod launch;
pub mod lifecycle;
pub mod maintenance;
pub mod observations;
mod presence;
pub mod protocol;
pub mod providers;
pub mod query;
pub mod records;
pub mod wezterm;

pub use launch::{
    ApplyResult, PublishReport, claim_launch, claim_launch_at_tty, publish_current, publish_realm,
};

use std::collections::BTreeMap;
use std::env;
use std::os::unix::ffi::OsStrExt;

use crate::records::{NOT_UTF8, STATE_ROOT_VARIABLES};

/// The process environment, keeping only variables whose name and value are
/// both UTF-8. `env::vars` panics on the first variable that is not, which
/// would stop every command -- hooks included -- before it could run; no
/// variable this crate reads is expected to hold anything but text. A
/// variable that can name the state root is kept as [`NOT_UTF8`], which the
/// root refuses, where dropping it would send every write to another root
/// without a word. A relative `XDG_STATE_HOME` names no root, so one that is
/// not UTF-8 is dropped as a relative one that is would be ignored.
pub fn environment() -> BTreeMap<String, String> {
    env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            match value.into_string() {
                Ok(value) => Some((name, value)),
                Err(value)
                    if STATE_ROOT_VARIABLES.contains(&name.as_str())
                        && (name != "XDG_STATE_HOME" || value.as_bytes().starts_with(b"/")) =>
                {
                    Some((name, NOT_UTF8.to_owned()))
                }
                Err(_) => None,
            }
        })
        .collect()
}
