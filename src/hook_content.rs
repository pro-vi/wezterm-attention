//! What a hook event was able to say about the text that provoked it.
//!
//! Its own module because extracting content from a provider's payload has
//! nothing to do with launching a consumer executable. It used to live in
//! `consumer`, which made `providers` depend on the delivery subsystem to name
//! the result of a parse, and `protocol` reach the delivery subsystem through
//! `providers`. `consumer` re-exports the name, so the old path still resolves.

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
