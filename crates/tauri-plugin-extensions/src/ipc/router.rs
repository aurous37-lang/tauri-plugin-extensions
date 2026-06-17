//! chrome.runtime message + port router.
//!
//! Connects the JS shim's invoke surface to the target webviews. The flow
//! for a content → background `sendMessage`:
//!
//! 1. The content shim invokes `extensions_runtime_send_message` with
//!    `{from, to, extensionId, payload, …}` and awaits the result.
//! 2. The command registers a pending request (`requestId → oneshot`) and
//!    `eval`s `window.__extEventDispatch("extensions://runtime/message", …)`
//!    into the extension's hidden BG webview (label `ext-bg-<id>`), where
//!    the dispatcher polyfill (see
//!    [`crate::runtime::EVENT_DISPATCH_POLYFILL_JS`]) fans it out to the
//!    shim's `onMessage` machinery.
//! 3. The BG shim's `sendResponse` invokes the same command with
//!    `{requestId, response, phase: "response"}`, which resolves the
//!    pending oneshot; the original invoke returns
//!    `{ok: true, response}` to the content caller.
//!
//! Ports (`chrome.runtime.connect`) work the same way, except the port id
//! is minted by the JS side and both ends share it: the router keeps a
//! `PortRoute` per port and delivers each `postMessage` to whichever end
//! did NOT send it (decided by the calling webview's label).
//!
//! Delivery is `eval`-based rather than Tauri-event-based on purpose: it
//! needs no event-plugin capability grants on the hidden BG windows, and it
//! reaches content surfaces on foreign origins (`file://`, remote HTTP)
//! where Tauri injects no IPC bridge.

use dashmap::DashMap;
use serde_json::{json, Value};
use tauri::Manager;
use tokio::sync::oneshot;

use crate::{registry::ExtensionId, runtime::bg_window_label, Error, Result};

/// Inbound `chrome.runtime` message for a surface. Mirrors `InboundMessage`
/// in `js-runtime/src/shared/types.ts`.
pub const EVENT_INBOUND_MESSAGE: &str = "extensions://runtime/message";
/// Inbound `chrome.runtime.connect` notification. Mirrors `InboundConnect`.
pub const EVENT_INBOUND_CONNECT: &str = "extensions://runtime/connect";
/// Message on an established port. Mirrors `PortInbound`.
pub const EVENT_PORT_MESSAGE: &str = "extensions://runtime/port_message";
/// Port-disconnect notification. Payload: `{ portId }`.
pub const EVENT_PORT_DISCONNECT: &str = "extensions://runtime/port_disconnect";

/// How long a `sendMessage` waits for the receiving surface's
/// `sendResponse` before failing the sender's promise. Chrome keeps the
/// channel open indefinitely; we bound it so a hung BG worker can't leak
/// pending invokes forever.
pub const SEND_MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// One end-to-end port: opened by `opener_label`, targeting
/// `target_label` (v1: always the extension's hidden BG webview).
#[derive(Debug, Clone)]
pub struct PortRoute {
    /// Extension the port belongs to.
    pub extension_id: ExtensionId,
    /// Label of the webview that called `chrome.runtime.connect`.
    pub opener_label: String,
    /// Label of the webview hosting the other end.
    pub target_label: String,
    /// `connect({name})`, or empty.
    pub name: String,
}

/// Router state, managed in Tauri state by `lib.rs::init`.
#[derive(Debug, Default)]
pub struct Router {
    pending: DashMap<String, oneshot::Sender<Value>>,
    ports: DashMap<String, PortRoute>,
}

impl Router {
    /// Fresh, empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending `sendMessage` request. The returned receiver
    /// resolves when [`Router::resolve_pending`] is called with the same id.
    pub fn register_pending(&self, request_id: String) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id, tx);
        rx
    }

    /// Resolve a pending request with the responder's payload. Returns
    /// `false` when the id was never registered (or already resolved) —
    /// harmless, but logged by callers since it usually means a duplicate
    /// `sendResponse`.
    pub fn resolve_pending(&self, request_id: &str, response: Value) -> bool {
        match self.pending.remove(request_id) {
            Some((_, tx)) => {
                // The receiver may have timed out and dropped; that's fine.
                let _ = tx.send(response);
                true
            }
            None => false,
        }
    }

    /// Drop a pending request without resolving it (sender-side timeout).
    pub fn abandon_pending(&self, request_id: &str) {
        self.pending.remove(request_id);
    }

    /// Record a port route under the JS-minted port id.
    pub fn register_port(&self, port_id: String, route: PortRoute) {
        self.ports.insert(port_id, route);
    }

    /// Look up a port route.
    pub fn port_route(&self, port_id: &str) -> Option<PortRoute> {
        self.ports.get(port_id).map(|r| r.clone())
    }

    /// Remove a port route, returning it so the caller can notify the peer.
    pub fn remove_port(&self, port_id: &str) -> Option<PortRoute> {
        self.ports.remove(port_id).map(|(_, r)| r)
    }

    /// The label of the port end that did NOT make the current call. A post
    /// from the opener goes to the target; a post from the target goes back
    /// to the opener. (A caller that is neither — host code relaying — is
    /// treated as the opener side.)
    pub fn peer_label(route: &PortRoute, caller_label: &str) -> String {
        if caller_label == route.target_label {
            route.opener_label.clone()
        } else {
            route.target_label.clone()
        }
    }
}

/// Eval a dispatcher call into the webview with the given label. The
/// polyfill guard (`window.__extEventDispatch &&`) makes a dispatch into a
/// not-yet-bootstrapped page a silent no-op rather than a ReferenceError.
pub fn dispatch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    label: &str,
    event: &str,
    payload: &Value,
) -> Result<()> {
    let webview = app
        .get_webview_window(label)
        .ok_or_else(|| Error::Ipc(format!("no webview '{label}' to dispatch {event} to")))?;
    let event_lit = serde_json::to_string(event)?;
    let payload_lit = serde_json::to_string(payload)?;
    let script =
        format!("window.__extEventDispatch && window.__extEventDispatch({event_lit}, {payload_lit});");
    webview
        .eval(&script)
        .map_err(|e| Error::Ipc(format!("dispatch eval into '{label}' failed: {e}")))
}

/// Send a `chrome.runtime` message to an extension's background surface and
/// await its `sendResponse`. The core of both the
/// `extensions_runtime_send_message` command and the host-facing
/// [`crate::send_message_to_background`] API.
pub async fn send_to_background<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    router: &Router,
    extension_id: &ExtensionId,
    payload: Value,
    sender: Value,
) -> Result<Value> {
    let label = bg_window_label(extension_id);
    if app.get_webview_window(&label).is_none() {
        return Err(Error::Ipc(format!(
            "extension '{extension_id}' has no background webview ('{label}') — \
             receiving end does not exist"
        )));
    }

    let request_id = format!("req-{}", uuid::Uuid::new_v4().simple());
    let rx = router.register_pending(request_id.clone());

    let event = json!({
        "requestId": request_id,
        "extensionId": extension_id.as_str(),
        "payload": payload,
        "sender": sender,
    });
    if let Err(e) = dispatch(app, &label, EVENT_INBOUND_MESSAGE, &event) {
        router.abandon_pending(&request_id);
        return Err(e);
    }

    match tokio::time::timeout(SEND_MESSAGE_TIMEOUT, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => {
            // Sender half dropped without resolving — router state was
            // cleared by a competing resolve; treat as no response.
            Err(Error::Ipc("response channel closed without a reply".into()))
        }
        Err(_) => {
            router.abandon_pending(&request_id);
            Err(Error::Ipc(format!(
                "background of '{extension_id}' did not respond within {}s",
                SEND_MESSAGE_TIMEOUT.as_secs()
            )))
        }
    }
}
