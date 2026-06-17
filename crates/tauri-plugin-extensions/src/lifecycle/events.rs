//! Lifecycle events emitted over the Tauri event bus.
//!
//! Any state transition that an observer could care about is a [`LifecycleEvent`].
//! The host UI subscribes to `extensions://lifecycle/changed` to drive its
//! rendering — this is how enterprise tooling reacts to extension activity
//! without polling the registry.
//!
//! Event payload is a single [`LifecycleEvent`] value. The matrix of which
//! variant goes out for which transition is:
//!
//! | Transition                    | Variant        |
//! |-------------------------------|----------------|
//! | first install                 | `Installed`    |
//! | start / enable                | `Started`      |
//! | stop / disable (non-reload)   | `Stopped`      |
//! | reload (atomic stop+start)    | `Reloaded`     |
//! | uninstall                     | `Uninstalled`  |
//! | boot-time orphan reap         | `OrphanReaped` |

use serde::{Deserialize, Serialize};

use crate::registry::ExtensionId;

use super::{manager::InvariantViolation, state::StopReason};

/// Name of the Tauri event this module emits. Subscribe with
/// `listen('extensions://lifecycle/changed', …)` on the JS side.
pub const EVENT_NAME: &str = "extensions://lifecycle/changed";

/// Single flat variant set. Each carries the extension id plus whatever
/// extra context makes the event useful without a follow-up query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// Extension moved from unknown → `Installed` for the first time this
    /// process.
    Installed {
        /// Extension id (stable).
        id: ExtensionId,
        /// Manifest name.
        name: String,
        /// Manifest version, if present.
        version: Option<String>,
    },
    /// Extension transitioned into `Running`. `bg_present` is `true` if a
    /// hidden BG webview actually spawned; `false` when the manifest had no
    /// `background.service_worker` or the platform backend refused.
    Started {
        /// Extension id.
        id: ExtensionId,
        /// Whether a BG worker came up.
        bg_present: bool,
    },
    /// Extension transitioned into `Stopped`. Not emitted when the stop is
    /// part of an atomic `reload` — see `Reloaded` for that case.
    Stopped {
        /// Extension id.
        id: ExtensionId,
        /// Typed reason.
        reason: StopReason,
    },
    /// Extension was reloaded — stop + start, atomic from the observer's
    /// perspective. No intermediate `Stopped` event fires during this path.
    Reloaded {
        /// Extension id.
        id: ExtensionId,
        /// Whether the BG worker is up after the reload.
        bg_present: bool,
    },
    /// Extension was fully removed from the registry.
    Uninstalled {
        /// Extension id.
        id: ExtensionId,
    },
    /// Extension enabled (restart from a disabled `Stopped { UserRequested }`).
    Enabled {
        /// Extension id.
        id: ExtensionId,
    },
    /// Extension disabled (graceful stop; entry remains installed).
    Disabled {
        /// Extension id.
        id: ExtensionId,
    },
    /// A hidden `ext-bg-*` webview window was found at boot that no
    /// lifecycle entry owns — typically a zombie from a previous dev run
    /// before the lifecycle manager landed. The reaper closed it.
    OrphanReaped {
        /// Window label that got reaped.
        label: String,
    },
    /// The background watchdog found something wrong during its periodic
    /// sweep — either orphan `ext-bg-*` windows it had to reap, or invariant
    /// violations in the lifecycle state, or both. Emitted only when there
    /// is something to report; a clean sweep is silent.
    WatchdogAlert {
        /// Count of orphan windows the watchdog closed during this sweep.
        reaped: usize,
        /// Invariants that failed during this sweep. Empty if only orphan
        /// reaping triggered the alert.
        violations: Vec<InvariantViolation>,
    },
}
