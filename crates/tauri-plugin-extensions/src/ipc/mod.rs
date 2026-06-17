//! In-process IPC bus routing `chrome.runtime` traffic between content
//! scripts, background, and popup surfaces.
//!
//! Owned by Agent C. The bus is a `DashMap<PortId, Sender<BusMessage>>`;
//! `send` awaits a reply over a `oneshot`, `post` is fire-and-forget.

pub mod bus;
pub mod command;
pub mod router;

pub use bus::{Bus, BusMessage};
pub use command::{PortInfo, PortSurface};
pub use router::{PortRoute, Router};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a `chrome.runtime` port — one per connected surface
/// (content script, background, popup). Minted on registration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortId(pub String);

impl PortId {
    /// Mint a fresh port id.
    pub fn new() -> Self {
        Self(format!("port-{}", Uuid::new_v4().simple()))
    }

    /// Borrow the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PortId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Envelope shape retained for compatibility with earlier stubs that imported
/// `ipc::Message`. New code should use [`BusMessage`] directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Extension the message originates from or targets.
    pub extension_id: String,
    /// Arbitrary JSON payload — the extension's own message format.
    pub payload: serde_json::Value,
}
