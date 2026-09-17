//! What a hook event was admitted as, and what it managed to persist.
//!
//! Owned by `lifecycle` because `lifecycle` decides these. They were defined in
//! `consumer`, so the engine imported its own result vocabulary from the module
//! that delivers it, and the dependency arrow pointed the wrong way. `consumer`
//! re-exports them, so the old paths still resolve.

use serde::Serialize;

use crate::identity::PaneAddress;
use crate::observations::{Actor, NativeCorrelation};
use crate::providers::ProviderAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Persistence {
    NotRequested,
    Confirmed,
    Rejected,
    Unconfirmed,
}

#[derive(Clone, Debug, Serialize)]
pub struct HookPersistence {
    pub native_state: Persistence,
    pub activity: Persistence,
    pub compatibility: Persistence,
    pub lifecycle: Persistence,
}

#[derive(Clone, Debug, Serialize)]
pub struct HookScope {
    pub address: PaneAddress,
    pub launch_id: String,
    pub target: BindingTarget,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingTarget {
    Binding { binding_id: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct AdmittedHook {
    pub action: ProviderAction,
    pub scope: HookScope,
    pub provider: String,
    pub provider_session_id: String,
    pub source_event: String,
    pub actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation: Option<NativeCorrelation>,
}
