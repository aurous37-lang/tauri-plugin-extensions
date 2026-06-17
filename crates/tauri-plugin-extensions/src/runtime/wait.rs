//! Condition-based waiting for webview teardown / spawn sequencing.
//!
//! wry dispatches `WebviewWindow::close()` asynchronously — the window label
//! stays in Tauri's registrar for some time after the Rust call returns.
//! Any code that closes a window and immediately rebuilds under the same
//! label races that teardown and hits `WebviewLabelAlreadyExists`. These
//! helpers replace the time-based sleeps that raced (and the `destroy()` +
//! poll experiment that tripped wry's IPC-handler `InvalidUri` panic on
//! 2026-04-20) with bounded condition polls.
//!
//! Pure async logic — no Tauri runtime required — so the semantics are
//! covered by `tests/bg_reload_race.rs` despite the Win 11 26200 DLL-load
//! issue that keeps live `AppHandle`s out of integration-test binaries.

use std::time::Duration;

/// Poll `condition` every `interval` until it returns `true` or `ceiling`
/// elapses. Returns `true` as soon as the condition holds (the condition is
/// always probed at least once, even with a zero ceiling) and `false` if the
/// ceiling expires first.
///
/// Uses `tokio::time::sleep`, so tests can drive it with a paused clock.
pub async fn poll_until(
    interval: Duration,
    ceiling: Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + ceiling;
    loop {
        if condition() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Classify a Tauri error as "this window/webview label is still registered".
///
/// `WebviewWindowBuilder::build` registers both a window and a webview under
/// the same label; depending on which registrar trips first it surfaces
/// either variant. Both mean the previous incarnation's teardown has not
/// finished (or an orphan owns the label) and a retry after waiting is the
/// right response — unlike every other build error, which is terminal.
pub fn is_label_already_exists(err: &tauri::Error) -> bool {
    matches!(
        err,
        tauri::Error::WindowLabelAlreadyExists(_) | tauri::Error::WebviewLabelAlreadyExists(_)
    )
}
