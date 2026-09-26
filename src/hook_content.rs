//! What a hook event was able to say about the text that provoked it.
//!
//! Its own module because extracting content from a provider's payload has
//! nothing to do with launching a consumer executable: `providers` names the
//! result of a parse here without depending on the delivery subsystem.

use serde::Serialize;

// Deliberately no Debug: content must not enter diagnostics accidentally.
#[derive(Serialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum HookContent {
    NotRequested,
    Available { text: String },
    Absent,
    Unsupported,
    Invalid,
    TooLarge,
}
