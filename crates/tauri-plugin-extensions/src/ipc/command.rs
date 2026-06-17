//! Command-side payloads for the IPC bus.
//!
//! Kept separate from [`super::bus`] so the Tauri-command boundary types
//! (serialized across the JS↔Rust invoke gap) are isolated from the
//! in-process channel internals.

use serde::{Deserialize, Serialize};

use crate::registry::ExtensionId;

/// Which surface of an extension a port belongs to. Declared by the JS shim
/// when it calls `extensions_runtime_register_port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSurface {
    /// Hidden background webview hosting `background.service_worker`.
    Background,
    /// Content script injected into a page.
    ContentScript,
    /// Browser-action popup (not rendered in v1, but the shim may still
    /// register a port).
    Popup,
    /// Options page.
    Options,
}

/// Registration record, kept in the registry so Agent D's backend can look up
/// which port belongs to which extension / surface when routing messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    /// Extension this port belongs to.
    pub extension_id: ExtensionId,
    /// Which surface the port is on.
    pub surface: PortSurface,
    /// Name passed to `chrome.runtime.connect({name})`, if any. `None` for
    /// `chrome.runtime.sendMessage` — those don't name their ephemeral port.
    pub connector_name: Option<String>,
}
