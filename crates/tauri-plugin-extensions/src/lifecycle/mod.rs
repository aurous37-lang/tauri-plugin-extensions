//! Extension-lifecycle service.
//!
//! Owns the canonical state of every loaded extension across this process:
//! stable identity, typed state machine, per-entry async serialization,
//! persistent inventory on disk, orphan reconciliation at boot, and
//! graceful shutdown on app exit.
//!
//! Public API:
//!
//! - [`manager::LifecycleManager`] — the single mutator.
//! - [`state::ExtensionState`] / [`state::StateSnapshot`] / [`state::StopReason`].
//! - [`events::LifecycleEvent`] — Tauri-event payload emitted on every transition.
//! - [`store::StateStore`] (+ [`store::FsStateStore`] / [`store::MemoryStateStore`]).
//!
//! See `docs/ARCHITECTURE.md` for how this module slots into the plugin's
//! subsystem map.

pub mod events;
pub mod manager;
pub mod state;
pub mod store;

pub use events::{LifecycleEvent, EVENT_NAME};
pub use manager::{
    check_invariants, derive_extension_id, Diagnostics, EntryFacts, InvariantViolation,
    LifecycleEntry, LifecycleManager, LifecycleSummary,
};
pub use state::{ExtensionState, StateSnapshot, StopReason};
pub use store::{FsStateStore, MemoryStateStore, PersistedEntry, StateStore, STATE_SCHEMA_VERSION};
