//! In-memory message router for `chrome.runtime` traffic.
//!
//! Every registered [`super::PortId`] owns an `mpsc::UnboundedSender` held in
//! a [`DashMap`]. Sending a message looks the recipient up, forwards the
//! message, and (for request/response) awaits a `oneshot` reply.
//!
//! No cross-process concerns — every surface runs in the host process; the
//! bus only shuttles `serde_json::Value` between Tokio tasks.

use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::{Error, Result};

use super::{command::PortInfo, PortId};

/// A message traversing the bus.
#[derive(Debug)]
pub struct BusMessage {
    /// Port the message originates from.
    pub from: PortId,
    /// Port the message is addressed to.
    pub to: PortId,
    /// JSON payload.
    pub payload: Value,
    /// Reply channel. `Some` for `sendMessage`-style request/response;
    /// `None` for `postMessage`-style fire-and-forget.
    pub reply: Option<oneshot::Sender<Value>>,
}

/// In-process message bus. Shared via `Arc` through Tauri state.
#[derive(Debug, Default)]
pub struct Bus {
    senders: DashMap<PortId, mpsc::UnboundedSender<BusMessage>>,
    metadata: DashMap<PortId, PortInfo>,
}

impl Bus {
    /// Fresh, empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a port. Returns the receiver half; the caller (Agent D's
    /// backend shim runner) keeps draining it and forwards messages into the
    /// appropriate surface via `WebviewWindow::eval`.
    pub fn register_port(
        &self,
        id: PortId,
        info: PortInfo,
    ) -> mpsc::UnboundedReceiver<BusMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.senders.insert(id.clone(), tx);
        self.metadata.insert(id, info);
        rx
    }

    /// Remove a port. Subsequent `send` / `post` calls targeting it return
    /// [`Error::Ipc`].
    pub fn unregister_port(&self, id: &PortId) {
        self.senders.remove(id);
        self.metadata.remove(id);
    }

    /// Request/response send. Awaits the recipient's `postMessage(reply)`.
    pub async fn send(&self, from: PortId, to: PortId, payload: Value) -> Result<Value> {
        let tx = self
            .senders
            .get(&to)
            .map(|s| s.clone())
            .ok_or_else(|| Error::Ipc(format!("no such port: {to}")))?;

        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(BusMessage {
            from,
            to: to.clone(),
            payload,
            reply: Some(reply_tx),
        })
        .map_err(|_| Error::Ipc(format!("port '{to}' is closed")))?;

        reply_rx
            .await
            .map_err(|_| Error::Ipc(format!("port '{to}' dropped before replying")))
    }

    /// Fire-and-forget post. Returns immediately after enqueueing.
    pub fn post(&self, from: PortId, to: PortId, payload: Value) -> Result<()> {
        let tx = self
            .senders
            .get(&to)
            .map(|s| s.clone())
            .ok_or_else(|| Error::Ipc(format!("no such port: {to}")))?;
        tx.send(BusMessage {
            from,
            to: to.clone(),
            payload,
            reply: None,
        })
        .map_err(|_| Error::Ipc(format!("port '{to}' is closed")))?;
        Ok(())
    }

    /// Lookup metadata for a registered port. Returns `None` if the port has
    /// been unregistered or never existed.
    pub fn info(&self, id: &PortId) -> Option<PortInfo> {
        self.metadata.get(id).map(|r| r.clone())
    }
}

// Tests live in `tests/c_bus_storage.rs` — see note in `registry.rs`.
