//! `chrome.storage.local` / `chrome.storage.session` backend.
//!
//! `local` persists to `app_data_dir/extensions/<ext_id>/storage-local.json`.
//! `session` is in-memory only, cleared when the extension is unloaded.
//!
//! `sync` is explicitly out of scope for v1.

pub mod local;
pub mod session;

pub use local::{LocalStorage, LocalStorageManager};
pub use session::{SessionStorage, SessionStorageManager};

use serde::{Deserialize, Serialize};

/// Which storage area an operation targets. Matches Chrome's
/// `chrome.storage.local` / `chrome.storage.session`. `sync` is intentionally
/// absent — see `docs/ARCHITECTURE.md` F.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageArea {
    /// `chrome.storage.local` — disk-backed, per-extension JSON.
    Local,
    /// `chrome.storage.session` — in-memory, cleared on unload.
    Session,
}
