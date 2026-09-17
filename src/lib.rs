pub mod compat;
pub mod consumer;
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
use crate::records::state_root;

pub fn environment() -> BTreeMap<String, String> {
    env::vars().collect()
}

pub fn state_root_from_environment() -> Result<PathBuf> {
    state_root(&environment())
}
