//! Regression tests for the chrome.runtime message/port routing layer.
//!
//! The live end-to-end path (content script → invoke → Router → eval into
//! the BG webview → shim onMessage → sendResponse → response-phase invoke →
//! pending-request resolution) is exercised by the minimal host's
//! `bg_round_trip` acceptance step against the noop-mv3 fixture. This file
//! pins the pure pieces the live path is built from:
//!
//! - the event-name contract shared with the JS shim (`js-runtime/src/shared/types.ts`),
//! - the BG window-label derivation used to address the hidden webview,
//! - the `Router`'s pending-request and port-route bookkeeping,
//! - the dispatcher polyfill that gives every surface a `__TAURI_EVENT__`.

use serde_json::json;
use tauri_plugin_extensions::{
    ipc::router::{
        PortRoute, Router, EVENT_INBOUND_CONNECT, EVENT_INBOUND_MESSAGE, EVENT_PORT_DISCONNECT,
        EVENT_PORT_MESSAGE,
    },
    registry::ExtensionId,
    runtime::{bg_window_label, EVENT_DISPATCH_POLYFILL_JS},
};

// ---------------------------------------------------------------------------
// Event-name contract — must match RuntimeEvents in the JS shim verbatim.
// ---------------------------------------------------------------------------

#[test]
fn event_names_match_the_js_shim_contract() {
    assert_eq!(EVENT_INBOUND_MESSAGE, "extensions://runtime/message");
    assert_eq!(EVENT_INBOUND_CONNECT, "extensions://runtime/connect");
    assert_eq!(EVENT_PORT_MESSAGE, "extensions://runtime/port_message");
    assert_eq!(EVENT_PORT_DISCONNECT, "extensions://runtime/port_disconnect");
}

// ---------------------------------------------------------------------------
// BG window label — the address every background-bound dispatch resolves.
// ---------------------------------------------------------------------------

#[test]
fn bg_window_label_uses_first_twelve_chars_of_id() {
    let id = ExtensionId::new("bfnaelmomeimhlpmgjnjophhpkkoljpa");
    assert_eq!(bg_window_label(&id), "ext-bg-bfnaelmomeim");
}

#[test]
fn bg_window_label_short_id_is_not_padded() {
    let id = ExtensionId::new("abc");
    assert_eq!(bg_window_label(&id), "ext-bg-abc");
}

// ---------------------------------------------------------------------------
// Pending-request bookkeeping (sendMessage request → response correlation).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_request_resolves_with_the_response_payload() {
    let router = Router::new();
    let rx = router.register_pending("req-1".to_string());
    assert!(router.resolve_pending("req-1", json!({"kind": "pong"})));
    let got = rx.await.expect("response delivered");
    assert_eq!(got, json!({"kind": "pong"}));
}

#[test]
fn resolving_an_unknown_request_reports_false() {
    let router = Router::new();
    assert!(!router.resolve_pending("never-registered", json!(null)));
}

#[tokio::test]
async fn dropped_pending_request_does_not_poison_later_ones() {
    let router = Router::new();
    // Receiver dropped (e.g. sender-side timeout fired).
    drop(router.register_pending("req-timeout".to_string()));
    // Late response: resolve returns true (entry existed) but must not panic.
    let _ = router.resolve_pending("req-timeout", json!(1));
    // A fresh request under a different id still works.
    let rx = router.register_pending("req-2".to_string());
    assert!(router.resolve_pending("req-2", json!(2)));
    assert_eq!(rx.await.unwrap(), json!(2));
}

// ---------------------------------------------------------------------------
// Port-route bookkeeping (connect / postMessage / disconnect).
// ---------------------------------------------------------------------------

fn route() -> PortRoute {
    PortRoute {
        extension_id: ExtensionId::new("ext-a"),
        opener_label: "main".to_string(),
        target_label: "ext-bg-exta".to_string(),
        name: "wallet".to_string(),
    }
}

#[test]
fn port_route_round_trips_through_the_router() {
    let router = Router::new();
    router.register_port("port-1".to_string(), route());
    let got = router.port_route("port-1").expect("route present");
    assert_eq!(got.opener_label, "main");
    assert_eq!(got.target_label, "ext-bg-exta");
    assert!(router.remove_port("port-1").is_some());
    assert!(router.port_route("port-1").is_none());
}

#[test]
fn peer_label_is_relative_to_the_caller() {
    let r = route();
    // The opener posts → deliver to the target (BG).
    assert_eq!(Router::peer_label(&r, "main"), "ext-bg-exta");
    // The BG posts → deliver back to the opener.
    assert_eq!(Router::peer_label(&r, "ext-bg-exta"), "main");
}

// ---------------------------------------------------------------------------
// Dispatcher polyfill — the JS the Rust side injects into every surface so
// `getEventApi()` in the shim finds a working `__TAURI_EVENT__`.
// ---------------------------------------------------------------------------

#[test]
fn polyfill_defines_dispatch_entry_point_and_event_api() {
    assert!(EVENT_DISPATCH_POLYFILL_JS.contains("__extEventDispatch"));
    assert!(EVENT_DISPATCH_POLYFILL_JS.contains("__TAURI_EVENT__"));
    // Handlers receive the Tauri-event envelope shape ({ payload }).
    assert!(EVENT_DISPATCH_POLYFILL_JS.contains("payload"));
    // Idempotent — double-injection must be a no-op.
    assert!(EVENT_DISPATCH_POLYFILL_JS.contains("if (window.__extEventDispatch) return;"));
}
