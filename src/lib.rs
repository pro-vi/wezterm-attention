//! # wezterm-attention
//!
//! Records what a pane's agent is doing, against a pane identity that survives
//! detach, reattach and multiple mux sockets. The WezTerm plugin reads those
//! records to tint a tab; other programs read them through the `attention` CLI.
//!
//! ## What is supported
//!
//! The stable way to use this project is the `attention` command and its JSON
//! envelopes, which `docs/consumer-guide.md` specifies. A Rust caller that links
//! the library instead depends on these, and only these:
//!
//! - [`query`] for reading facts: [`query::read_bindings`],
//!   [`query::read_pane_facts`], and the row and facet types they return. A
//!   socket-scoped listing is supported as a CLI query; no suffix-free Rust
//!   wrapper for it exists, so none is promised here. The published tab order
//!   is the same: `attention tabs` is the supported way to read it, and
//!   [`query::read_tab_publications`] is how that command does it, not a
//!   promise to a linking caller.
//! - [`lifecycle`] for applying a provider event, and [`lifecycle::outcome`] for
//!   what that event was admitted as and what it persisted.
//! - [`consumer`] for building and delivering a hook envelope.
//! - [`identity`] for pane addresses, and [`protocol`] for the manifest and the
//!   record validators.
//! - [`claim_launch`], [`claim_launch_at_tty`], [`publish_current`],
//!   [`publish_realm`], [`environment`] and [`state_root_from_environment`] here
//!   at the root.
//!
//! A supported operation's signature is supported with it. [`claim_launch`] and
//! [`publish_current`] take [`wezterm::RuntimePorts`], so that type, the
//! [`wezterm::Clock`], [`wezterm::TtyWriter`] and [`wezterm::PaneLister`] traits
//! it holds, and [`wezterm::PaneRow`] in the last of those are supported too.
//! [`lifecycle::outcome::AdmittedHook`] likewise carries
//! [`providers::ProviderAction`], [`observations::Actor`] and
//! [`observations::NativeCorrelation`] in its public fields. A declaration
//! whose operations need types it disowns is not usable, so those are named
//! here rather than left to inference.
//!
//! ## What is not
//!
//! Everything else reachable from this crate is implementation. [`records`] in
//! particular exposes storage mechanics -- locking, atomic replacement, path
//! construction, durable deletion -- because this crate's own tests drive them,
//! not because a consumer should. [`maintenance`] and
//! [`hook_content`] are the same, as are the members of [`wezterm`],
//! [`providers`] and [`observations`] not named above: public because nothing
//! has yet made them private, not because their shapes are promised.
//!
//! The `_with_ports` variants take injectable readers, clocks and pane listers.
//! They exist so tests can substitute them and are unstable for that reason; the
//! variants without a suffix are the supported spelling.
//!
//! These may change without a major version. `docs/accepted-limitations.md`
//! records why the boundary is documented rather than enforced, and what it
//! would take to enforce it.

pub mod consumer;
pub mod hook_content;
pub mod identity;
mod launch;
pub mod lifecycle;
pub mod maintenance;
pub mod observations;
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
use std::path::PathBuf;

use crate::protocol::Result;
use crate::records::{NOT_UTF8, STATE_ROOT_VARIABLES, state_root};

/// The process environment, keeping only variables whose name and value are
/// both UTF-8. `env::vars` panics on the first variable that is not, which
/// would stop every command -- hooks included -- before it could run; no
/// variable this crate reads is expected to hold anything but text. The two
/// that can name the state root are kept as [`NOT_UTF8`], which the root
/// refuses, where dropping one would send every write to another root
/// without a word.
pub fn environment() -> BTreeMap<String, String> {
    env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            match value.into_string() {
                Ok(value) => Some((name, value)),
                Err(_) if STATE_ROOT_VARIABLES.contains(&name.as_str()) => {
                    Some((name, NOT_UTF8.to_owned()))
                }
                Err(_) => None,
            }
        })
        .collect()
}

pub fn state_root_from_environment() -> Result<PathBuf> {
    state_root(&environment())
}
