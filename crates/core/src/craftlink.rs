//! Read-only view of an external CraftLink binding. No native model or transport.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub object: String,
    pub profile: String,
    pub applied: u64,
    pub status: String,
}
