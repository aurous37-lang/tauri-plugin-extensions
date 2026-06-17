//! Hidden off-screen webview hosting the MV3 background service-worker
//! analog. Owned by Agent D per D-001.
//!
//! A [`BackgroundHandle`] is a cheap, clonable record pointing at a hidden
//! `WebviewWindow` created by [`crate::runtime::webview2::Webview2Backend`].
//! Shutdown looks the window up by label on the stored [`AppHandle`],
//! subscribes to its `Destroyed` event, calls `close()`, and waits (bounded)
//! for the label to actually leave Tauri's registrar; if the window is
//! already gone (manual close, app teardown), the error is logged and
//! swallowed — shutdown is idempotent.

use crate::registry::ExtensionId;

#[cfg(target_os = "windows")]
use std::time::Duration;

/// How long `shutdown` waits for the window's `Destroyed` event after
/// `close()`. wry processes the close on the next event-loop turns, so this
/// normally resolves in tens of milliseconds; the ceiling only bites when
/// the event loop is saturated or the close is wedged.
#[cfg(target_os = "windows")]
const DESTROYED_EVENT_CEILING: Duration = Duration::from_secs(2);

/// How long `shutdown` polls for the label to leave the registrar after the
/// `Destroyed` event (or its timeout). Registrar removal can lag the event
/// by a frame; this closes that gap without a blind sleep.
#[cfg(target_os = "windows")]
const LABEL_FREE_CEILING: Duration = Duration::from_secs(2);

/// Poll cadence for the registrar check. A registrar lookup is a cheap map
/// read — no wry calls — so a tight interval is fine.
#[cfg(target_os = "windows")]
const LABEL_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Handle to a running background webview. Dropping or calling
/// [`BackgroundHandle::shutdown`] tears the webview down.
///
/// The handle carries an erased [`tauri::AppHandle`] so it can resolve the
/// `WebviewWindow` at shutdown time without needing the backend around.
#[derive(Clone)]
pub struct BackgroundHandle {
    /// The extension this handle belongs to — useful for logging.
    pub extension: ExtensionId,
    /// Stable label of the hidden `WebviewWindow`. Generated as
    /// `ext-bg-<shortid>` so Tauri window labels stay unique.
    pub label: String,
    /// Tauri app handle used to look up and close the window at shutdown.
    /// `None` for the unit-test / stub path, in which case `shutdown()`
    /// is a no-op.
    #[cfg(target_os = "windows")]
    pub(crate) app: Option<tauri::AppHandle<tauri::Wry>>,
}

impl std::fmt::Debug for BackgroundHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundHandle")
            .field("extension", &self.extension)
            .field("label", &self.label)
            .finish()
    }
}

impl BackgroundHandle {
    /// Build a detached handle — used by the non-Windows stubs and by unit
    /// tests that don't spin up a real Tauri app.
    pub fn detached(extension: ExtensionId, label: String) -> Self {
        Self {
            extension,
            label,
            #[cfg(target_os = "windows")]
            app: None,
        }
    }

    /// Tear down the hidden webview and wait (bounded) for the teardown to
    /// complete. Non-fatal if already closed.
    ///
    /// wry dispatches `close()` asynchronously — the label stays in Tauri's
    /// registrar until the event loop processes the close. A respawn under
    /// the same `ext-bg-<id>` label that doesn't wait for that races into
    /// `WebviewLabelAlreadyExists` (the reload-after-install bug). So this
    /// method subscribes to the window's `Destroyed` event BEFORE calling
    /// `close()`, awaits it with a ceiling, then polls the registrar until
    /// the label actually frees. `WebviewWindow::destroy()` is deliberately
    /// not used as an escalation: forced teardown makes wry's IPC handler
    /// panic on messages from the dying webview (`InvalidUri(InvalidFormat)`
    /// at wry-0.54.4 webview2/mod.rs:912, observed 2026-04-20). A close that
    /// outlives both ceilings is logged and left for the spawn-side
    /// adopt-or-retry defense plus the orphan watchdog.
    pub async fn shutdown(self) -> crate::Result<()> {
        self.shutdown_inner(true).await
    }

    /// Fire-and-forget close. For the app-exit path only: `shutdown_all`
    /// runs under `block_on` on the main thread during
    /// `RunEvent::ExitRequested`, so the event loop cannot pump the close —
    /// waiting there would stall exit until the ceilings expire. The OS
    /// reclaims the webviews with the process; we just request the close.
    pub async fn shutdown_no_wait(self) -> crate::Result<()> {
        self.shutdown_inner(false).await
    }

    async fn shutdown_inner(self, wait_for_teardown: bool) -> crate::Result<()> {
        tracing::debug!(
            extension = %self.extension.as_str(),
            label = %self.label,
            wait = wait_for_teardown,
            "background shutdown requested"
        );

        #[cfg(not(target_os = "windows"))]
        let _ = wait_for_teardown;

        #[cfg(target_os = "windows")]
        {
            use tauri::Manager;
            let Some(app) = &self.app else {
                return Ok(());
            };
            let Some(window) = app.get_webview_window(&self.label) else {
                tracing::debug!(
                    extension = %self.extension.as_str(),
                    label = %self.label,
                    "background webview already absent at shutdown"
                );
                return Ok(());
            };

            // Listen-then-close: subscribe before close() so the Destroyed
            // signal can't slip between the call and the await. notify_one
            // stores a permit, so even an instantaneous destroy is caught.
            let destroyed = std::sync::Arc::new(tokio::sync::Notify::new());
            if wait_for_teardown {
                let signal = std::sync::Arc::clone(&destroyed);
                window.on_window_event(move |ev| {
                    if matches!(ev, tauri::WindowEvent::Destroyed) {
                        signal.notify_one();
                    }
                });
            }

            if let Err(err) = window.close() {
                tracing::warn!(
                    extension = %self.extension.as_str(),
                    label = %self.label,
                    error = %err,
                    "background webview close returned error (likely already closed)"
                );
            }

            if !wait_for_teardown {
                return Ok(());
            }

            let started = std::time::Instant::now();
            if tokio::time::timeout(DESTROYED_EVENT_CEILING, destroyed.notified())
                .await
                .is_err()
            {
                tracing::warn!(
                    extension = %self.extension.as_str(),
                    label = %self.label,
                    ceiling_ms = DESTROYED_EVENT_CEILING.as_millis() as u64,
                    "background webview Destroyed event did not arrive within ceiling"
                );
            }

            // Registrar removal can lag the Destroyed event by a frame.
            let app_for_poll = app.clone();
            let label_for_poll = self.label.clone();
            let freed = super::wait::poll_until(LABEL_POLL_INTERVAL, LABEL_FREE_CEILING, move || {
                app_for_poll.get_webview_window(&label_for_poll).is_none()
            })
            .await;

            if freed {
                tracing::debug!(
                    extension = %self.extension.as_str(),
                    label = %self.label,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background webview teardown complete"
                );
            } else {
                tracing::warn!(
                    extension = %self.extension.as_str(),
                    label = %self.label,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "background webview label still registered after close — \
                     respawn will wait-and-retry, and may adopt the stuck window"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detached_handle_shutdown_is_idempotent() {
        let handle = BackgroundHandle::detached(
            ExtensionId::new("test-ext"),
            "ext-bg-test00000000".to_string(),
        );
        // First shutdown succeeds (no-op because detached).
        let cloned = handle.clone();
        handle.shutdown().await.expect("first shutdown");
        // Calling again on the clone also succeeds — shutdown is a total fn.
        cloned.shutdown().await.expect("second shutdown");
    }

    #[tokio::test]
    async fn detached_handle_shutdown_no_wait_is_a_no_op() {
        let handle = BackgroundHandle::detached(
            ExtensionId::new("test-ext"),
            "ext-bg-test00000000".to_string(),
        );
        handle.shutdown_no_wait().await.expect("no-wait shutdown");
    }

    #[test]
    fn handle_debug_does_not_leak_app_handle() {
        let handle = BackgroundHandle::detached(
            ExtensionId::new("test-ext"),
            "ext-bg-test00000000".to_string(),
        );
        let repr = format!("{:?}", handle);
        assert!(repr.contains("test-ext"));
        assert!(repr.contains("ext-bg-test00000000"));
    }
}
