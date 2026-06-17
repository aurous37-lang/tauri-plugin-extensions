//! Regression tests for the BG-webview reload race.
//!
//! The bug: `install_or_reload` on a `Running` entry calls
//! `BackgroundHandle::shutdown` → `window.close()`, which wry dispatches
//! asynchronously — the `ext-bg-<id>` label stays in Tauri's registrar for
//! several frames after the Rust call returns. The immediately-following
//! `Backend::spawn_background` then hits `WebviewLabelAlreadyExists`,
//! `try_spawn` degrades the error to `None`, and the entry ends up
//! `Running { bg_handle: None }` — the extension reloads without a BG.
//!
//! The fix has two halves:
//! 1. `BackgroundHandle::shutdown` subscribes to `WindowEvent::Destroyed`
//!    BEFORE calling `close()`, awaits the signal with a ceiling, then polls
//!    the registrar until the label actually frees (`poll_until`).
//! 2. `Webview2Backend::spawn_background` defends in depth: it waits for a
//!    lingering label to free before building, and classifies the
//!    AlreadyExists error (`is_label_already_exists`) to retry once after a
//!    second wait.
//!
//! Both halves hinge on the two pure helpers exercised here. The live
//! shutdown → respawn sequence itself cannot run in this binary — anything
//! touching a real `AppHandle` trips the Win 11 26200
//! `STATUS_ENTRYPOINT_NOT_FOUND` DLL-load issue (see
//! `tests/lifecycle_regression.rs` module docs) — so the end-to-end repro
//! lives in the minimal host's auto-acceptance flow: steps
//! `reload_unpacked` + `ping_background_after_reload` in
//! `examples/minimal-host/dist/main.js`.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime};

use tauri_plugin_extensions::{
    lifecycle::ExtensionState,
    registry::ExtensionId,
    runtime::{
        background::BackgroundHandle,
        wait::{is_label_already_exists, poll_until},
    },
};

// ---------------------------------------------------------------------------
// poll_until — the condition-based wait both fix halves are built on.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn poll_until_returns_true_without_sleeping_when_condition_already_holds() {
    let polls = Arc::new(AtomicU32::new(0));
    let p = Arc::clone(&polls);
    let freed = poll_until(
        Duration::from_millis(15),
        Duration::from_secs(1),
        move || {
            p.fetch_add(1, Ordering::SeqCst);
            true
        },
    )
    .await;
    assert!(freed);
    // Condition was true on the first probe — no interval sleeps needed.
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn poll_until_observes_condition_that_flips_after_a_few_intervals() {
    // Models wry's deferred close: the label stays live for the first few
    // polls, then frees. start_paused auto-advances tokio's clock so the
    // test runs instantly while still exercising the sleep path.
    let polls = Arc::new(AtomicU32::new(0));
    let p = Arc::clone(&polls);
    let freed = poll_until(
        Duration::from_millis(15),
        Duration::from_secs(1),
        move || p.fetch_add(1, Ordering::SeqCst) >= 4,
    )
    .await;
    assert!(freed, "condition flipped within the ceiling but poll_until missed it");
}

#[tokio::test(start_paused = true)]
async fn poll_until_gives_up_at_the_ceiling_when_condition_never_holds() {
    let freed = poll_until(
        Duration::from_millis(15),
        Duration::from_millis(200),
        || false,
    )
    .await;
    assert!(!freed, "poll_until must report failure for a stuck window");
}

#[tokio::test(start_paused = true)]
async fn poll_until_respects_a_zero_ceiling() {
    // Degenerate config: a zero ceiling means exactly one probe, no sleeps.
    let polls = Arc::new(AtomicU32::new(0));
    let p = Arc::clone(&polls);
    let freed = poll_until(Duration::from_millis(15), Duration::ZERO, move || {
        p.fetch_add(1, Ordering::SeqCst);
        false
    })
    .await;
    assert!(!freed);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// ExtensionState::begin_stopping — the handle hand-off every stop path uses.
//
// The original bug that left BG windows alive forever: `internal_stop` wrote
// `entry.state = Stopping` BEFORE `mem::replace`-ing the state, dropping the
// `Running` variant (and its BackgroundHandle) on the floor — so `close()`
// was never dispatched on any reload/disable/shutdown. The live symptom was
// `AlreadyExists` on every respawn and permanently orphaned `ext-bg-*`
// windows. `begin_stopping` makes the extraction a single atomic step.
// ---------------------------------------------------------------------------

#[test]
fn begin_stopping_extracts_the_running_handle() {
    let handle = BackgroundHandle::detached(
        ExtensionId::new("ext-a"),
        "ext-bg-aaaa".to_string(),
    );
    let mut state = ExtensionState::Running {
        started_at: SystemTime::now(),
        bg_handle: Some(handle),
    };
    let taken = state.begin_stopping();
    assert!(taken.is_some(), "the Running handle must be handed off, not dropped");
    assert_eq!(taken.unwrap().label, "ext-bg-aaaa");
    assert_eq!(state.name(), "stopping");
}

#[test]
fn begin_stopping_on_running_without_handle_yields_none() {
    let mut state = ExtensionState::Running {
        started_at: SystemTime::now(),
        bg_handle: None,
    };
    assert!(state.begin_stopping().is_none());
    assert_eq!(state.name(), "stopping");
}

#[test]
fn begin_stopping_on_non_running_states_yields_none() {
    let mut installed = ExtensionState::Installed {
        installed_at: SystemTime::now(),
    };
    assert!(installed.begin_stopping().is_none());
    assert_eq!(installed.name(), "stopping");
}

// ---------------------------------------------------------------------------
// is_label_already_exists — the retry-trigger classification.
// ---------------------------------------------------------------------------

#[test]
fn webview_label_already_exists_is_classified() {
    let err = tauri::Error::WebviewLabelAlreadyExists("ext-bg-abcdef012345".into());
    assert!(is_label_already_exists(&err));
}

#[test]
fn window_label_already_exists_is_classified() {
    // WebviewWindowBuilder registers both a window and a webview under the
    // same label; depending on which registrar trips first, build() can
    // surface either variant. Both mean "label not yet free".
    let err = tauri::Error::WindowLabelAlreadyExists("ext-bg-abcdef012345".into());
    assert!(is_label_already_exists(&err));
}

#[test]
fn unrelated_tauri_errors_are_not_classified() {
    let err = tauri::Error::CannotReparentWebviewWindow;
    assert!(!is_label_already_exists(&err));
}
