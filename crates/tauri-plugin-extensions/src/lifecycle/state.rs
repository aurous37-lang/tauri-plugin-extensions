//! Explicit extension-lifecycle state machine.
//!
//! Every extension the plugin knows about is in exactly one of these states.
//! Transitions are driven by [`super::manager::LifecycleManager`] and are
//! the only way to mutate an extension's runtime status — the registry is a
//! read-only projection of the manager's state.
//!
//! ## Transitions (read left-to-right)
//!
//! ```text
//!     install_or_reload
//! ┌──────────────────────┐
//! │                      ▼
//! ∅ ───► Installed ──► Running ──► Stopping ──► Stopped
//!              ▲                                    │
//!              └────── (enable / reload) ───────────┘
//!                                                   │
//!                                                   ▼
//!                                             Uninstalling ──► ∅
//! ```
//!
//! ## Invariants
//!
//! - `Running` is the only state that may own a `BackgroundHandle`.
//! - `Uninstalling` is a transient state — an entry that enters
//!   `Uninstalling` is removed from the registry on success.
//! - `Stopped` has a typed `StopReason` so the UI / logs can explain why the
//!   extension is inactive (user-requested, crash, reload, shutdown).

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::runtime::background::BackgroundHandle;

/// Reason an extension entered the `Stopped` state. Surfaced over the
/// lifecycle event bus and to the host UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    /// Host called `unload` / `disable`.
    UserRequested,
    /// Background worker crashed or failed to spawn.
    Crashed {
        /// Human-readable crash message.
        message: String,
    },
    /// Application is exiting; every extension stops gracefully.
    Shutdown,
    /// Transitional stop emitted while a `reload` is in flight. The entry
    /// immediately restarts; callers should not treat this as a real stop.
    Reload,
    /// MV3 service-worker auto-termination after idle (v2 feature; reserved
    /// here so the state machine accommodates it without a schema change).
    Idle,
}

/// Current runtime state of a single extension. Owned by
/// [`super::manager::LifecycleEntry`].
///
/// Not `Serialize` — the `BackgroundHandle` doesn't cross IPC. See
/// [`StateSnapshot`] for the JSON-friendly projection.
#[derive(Debug)]
pub enum ExtensionState {
    /// Known to the plugin but not running. Typically a brief transient
    /// state between `install_or_reload` completing a successful manifest
    /// parse and the call to start the background worker.
    Installed {
        /// When the extension first became known to this process.
        installed_at: SystemTime,
    },
    /// Background worker is (or was) running. `bg_handle` is `None` when
    /// the extension declared no `background.service_worker` or when the
    /// platform backend refused to spawn (non-Wry runtime in tests).
    Running {
        /// When the most-recent start transition completed.
        started_at: SystemTime,
        /// Owned handle to the hidden WebView2 window. `shutdown()` is
        /// called on this during `stop` / `reload` / `uninstall`.
        bg_handle: Option<BackgroundHandle>,
    },
    /// Shutdown is in flight. No-op for further transitions until it
    /// completes and the state flips to `Stopped`.
    Stopping,
    /// Shut down cleanly. Can be re-started via `enable` / `reload`.
    Stopped {
        /// Typed reason — drives event payloads and logging.
        reason: StopReason,
        /// When the stop completed.
        stopped_at: SystemTime,
    },
    /// Teardown is in flight. The entry is removed from the registry on
    /// success; on failure it falls back to `Stopped { Crashed }`.
    Uninstalling,
}

impl ExtensionState {
    /// Short machine-friendly name used in events + logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Installed { .. } => "installed",
            Self::Running { .. } => "running",
            Self::Stopping => "stopping",
            Self::Stopped { .. } => "stopped",
            Self::Uninstalling => "uninstalling",
        }
    }

    /// Is a background worker currently running under this state?
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Is this a terminal state that accepts `start` / `enable`?
    pub fn can_start(&self) -> bool {
        matches!(self, Self::Installed { .. } | Self::Stopped { .. })
    }

    /// Is this a terminal state that accepts `stop` / `disable`?
    pub fn can_stop(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// Transition to `Stopping`, handing back the running
    /// [`BackgroundHandle`] (if any) so the caller can shut the webview
    /// down.
    ///
    /// Extraction and transition are one step on purpose: the original
    /// reload-race bug wrote `Stopping` over the `Running` variant before
    /// capturing it, silently dropping the handle — so `close()` was never
    /// dispatched and every respawn under the same `ext-bg-<id>` label hit
    /// `AlreadyExists` against a window that would never go away.
    pub fn begin_stopping(&mut self) -> Option<BackgroundHandle> {
        match std::mem::replace(self, Self::Stopping) {
            Self::Running {
                bg_handle: Some(handle),
                ..
            } => Some(handle),
            _ => None,
        }
    }
}

/// IPC-serializable snapshot of an [`ExtensionState`]. Excludes the
/// non-serializable `BackgroundHandle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StateSnapshot {
    /// See [`ExtensionState::Installed`].
    Installed {
        /// ISO-8601 timestamp.
        installed_at: String,
    },
    /// See [`ExtensionState::Running`].
    Running {
        /// ISO-8601 timestamp.
        started_at: String,
        /// Whether a background webview is currently active.
        bg_present: bool,
    },
    /// Transient.
    Stopping,
    /// See [`ExtensionState::Stopped`].
    Stopped {
        /// Typed reason.
        reason: StopReason,
        /// ISO-8601 timestamp.
        stopped_at: String,
    },
    /// Transient.
    Uninstalling,
}

impl From<&ExtensionState> for StateSnapshot {
    fn from(s: &ExtensionState) -> Self {
        match s {
            ExtensionState::Installed { installed_at } => Self::Installed {
                installed_at: iso(installed_at),
            },
            ExtensionState::Running {
                started_at,
                bg_handle,
            } => Self::Running {
                started_at: iso(started_at),
                bg_present: bg_handle.is_some(),
            },
            ExtensionState::Stopping => Self::Stopping,
            ExtensionState::Stopped { reason, stopped_at } => Self::Stopped {
                reason: reason.clone(),
                stopped_at: iso(stopped_at),
            },
            ExtensionState::Uninstalling => Self::Uninstalling,
        }
    }
}

/// Best-effort ISO-8601 formatter. Returns "unknown" if the SystemTime is
/// before the epoch or its epoch offset can't be represented.
fn iso(t: &SystemTime) -> String {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            // Manual formatter — avoids pulling chrono in just for this.
            let days = secs / 86_400;
            let rem = secs % 86_400;
            let h = rem / 3600;
            let m = (rem % 3600) / 60;
            let s = rem % 60;
            // Epoch days → Y/M/D using the civil-from-days algorithm
            // (https://howardhinnant.github.io/date_algorithms.html).
            let z = days + 719_468;
            let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
            let doe = (z - era * 146_097) as u64;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let y = yoe as i64 + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d_ = doy - (153 * mp + 2) / 5 + 1;
            let m_ = if mp < 10 { mp + 3 } else { mp - 9 };
            let y_ = if m_ <= 2 { y + 1 } else { y };
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                y_, m_, d_, h, m, s
            )
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_are_stable() {
        let t = SystemTime::UNIX_EPOCH;
        assert_eq!(ExtensionState::Installed { installed_at: t }.name(), "installed");
        assert_eq!(
            ExtensionState::Running {
                started_at: t,
                bg_handle: None
            }
            .name(),
            "running"
        );
        assert_eq!(ExtensionState::Stopping.name(), "stopping");
        assert_eq!(
            ExtensionState::Stopped {
                reason: StopReason::UserRequested,
                stopped_at: t
            }
            .name(),
            "stopped"
        );
        assert_eq!(ExtensionState::Uninstalling.name(), "uninstalling");
    }

    #[test]
    fn transitions_gates_are_correct() {
        let t = SystemTime::UNIX_EPOCH;
        let installed = ExtensionState::Installed { installed_at: t };
        let running = ExtensionState::Running {
            started_at: t,
            bg_handle: None,
        };
        let stopped = ExtensionState::Stopped {
            reason: StopReason::UserRequested,
            stopped_at: t,
        };

        assert!(installed.can_start());
        assert!(!installed.can_stop());
        assert!(!running.can_start());
        assert!(running.can_stop());
        assert!(stopped.can_start());
        assert!(!stopped.can_stop());
        assert!(!ExtensionState::Stopping.can_start());
        assert!(!ExtensionState::Uninstalling.can_stop());
    }
}
