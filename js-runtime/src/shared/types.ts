// Shared types used by both the content-world and background-world bootstraps.
//
// These describe the shape of messages we exchange with the Rust plugin via
// Tauri invoke, plus the minimal `chrome.*` surface the spike implements.
//
// The cross-surface contract is deliberately small: a few typed envelope
// shapes for IPC and a union of the storage areas we honor (`local` +
// `session`; `sync` is out of scope for v1 per ARCHITECTURE.md).

/// Name of the Tauri plugin as registered in the Rust crate (`lib.rs` →
/// `PLUGIN_NAME`). Command invocations use the `plugin:<name>|<command>`
/// path format that Tauri v2 requires.
export const PLUGIN_NAME = "extensions" as const;

/// Tauri v2 command identifiers. Centralised so the Rust side's command
/// names can be renamed in one spot if Agent C finalises a different shape.
export const RuntimeCommands = {
  ContentReady: `plugin:${PLUGIN_NAME}|extensions_content_ready`,
  ScriptingRegisterContentScripts: `plugin:${PLUGIN_NAME}|extensions_scripting_register_content_scripts`,
  ScriptingUnregisterContentScripts: `plugin:${PLUGIN_NAME}|extensions_scripting_unregister_content_scripts`,
  ScriptingGetRegisteredContentScripts: `plugin:${PLUGIN_NAME}|extensions_scripting_get_registered_content_scripts`,
  RuntimeSendMessage: `plugin:${PLUGIN_NAME}|extensions_runtime_send_message`,
  RuntimeConnect: `plugin:${PLUGIN_NAME}|extensions_runtime_connect`,
  RuntimePortPost: `plugin:${PLUGIN_NAME}|extensions_runtime_port_post`,
  RuntimePortDisconnect: `plugin:${PLUGIN_NAME}|extensions_runtime_port_disconnect`,
  StorageGet: `plugin:${PLUGIN_NAME}|extensions_storage_get`,
  StorageSet: `plugin:${PLUGIN_NAME}|extensions_storage_set`,
  StorageRemove: `plugin:${PLUGIN_NAME}|extensions_storage_remove`,
  StorageClear: `plugin:${PLUGIN_NAME}|extensions_storage_clear`,
} as const;

/// Tauri v2 event identifiers we subscribe to. The Rust side emits these
/// via `AppHandle::emit` after routing messages or storage mutations.
export const RuntimeEvents = {
  /// Inbound chrome.runtime message for this surface. Payload: `InboundMessage`.
  InboundMessage: `${PLUGIN_NAME}://runtime/message`,
  /// Inbound chrome.runtime.connect from another surface. Payload: `InboundConnect`.
  InboundConnect: `${PLUGIN_NAME}://runtime/connect`,
  /// Inbound message on an existing port. Payload: `PortInbound`.
  PortInbound: `${PLUGIN_NAME}://runtime/port_message`,
  /// Port-disconnect notification. Payload: `{ portId, reason? }`.
  PortDisconnect: `${PLUGIN_NAME}://runtime/port_disconnect`,
  /// storage.onChanged fan-out. Payload: `StorageChangeEvent`.
  StorageChanged: `${PLUGIN_NAME}://storage/changed`,
} as const;

export type StorageArea = "local" | "session";

/// Which surface is talking. Used to route `sendMessage` and `connect`
/// calls through the Rust bus.
export type Surface = "content" | "background" | "popup";

export interface ConfigureOptions {
  /// The `chrome.runtime.id` of the owning extension.
  extensionId: string;
  /// Parsed manifest snapshot (a shallow copy of manifest.json).
  manifest: Record<string, unknown>;
  /// Which world this bootstrap runs in. Supplied by the Rust host at
  /// configure time; `background-bootstrap.js` hard-codes it internally.
  surface: Surface;
  /// Stable frame id for content scripts; 0 for the top frame.
  /// Undefined in background/popup surfaces.
  frameId?: number;
  /// Origin (scheme + authority, no trailing slash) at which the host serves
  /// this extension's `web_accessible_resources`. `chrome.runtime.getURL`
  /// builds `<resourceBase>/<extensionId>/<path>` from it. Supplied by the
  /// Rust host (platform-specific); falls back to a dead scheme if absent.
  resourceBase?: string;
}

/// Runtime-only sender metadata passed to `onMessage` listeners. This is
/// the subset of Chrome's `MessageSender` the spike populates.
export interface MessageSender {
  id?: string;
  origin?: string;
  url?: string;
  frameId?: number;
  tab?: { id: number; url?: string };
}

/// Payload the Rust side dispatches to `runtime://message`.
export interface InboundMessage {
  /// Unique request id; echoed back on `sendResponse`.
  requestId: string;
  /// Extension id this message belongs to. Multi-extension routing dispatches
  /// per-id, so the shim filters by this before firing listeners.
  extensionId: string;
  /// Serialised message body (already structured-clone safe).
  payload: unknown;
  sender: MessageSender;
}

export interface InboundConnect {
  portId: string;
  extensionId: string;
  name: string;
  sender: MessageSender;
}

export interface PortInbound {
  portId: string;
  payload: unknown;
}

export interface StorageChangeEvent {
  extensionId: string;
  area: StorageArea;
  changes: Record<string, { oldValue?: unknown; newValue?: unknown }>;
}

/// Return envelope for `sendMessage` once a response is available.
export interface SendMessageResult {
  ok: boolean;
  response?: unknown;
  error?: string;
}

/// Minimal shape of `window.__TAURI_INTERNALS__.invoke` as exposed by Tauri v2.
export type InvokeFn = <T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
) => Promise<T>;

/// Minimal shape of the Tauri v2 event bus used by the shim.
export interface TauriEventApi {
  listen: <T = unknown>(
    event: string,
    handler: (e: { payload: T }) => void,
  ) => Promise<() => void>;
}
