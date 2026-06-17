// chrome.runtime shim.
//
// What we implement for the spike:
//   - id, getURL, getManifest
//   - sendMessage / onMessage (promise-style MV3)
//   - connect / onConnect (Port abstraction multiplexed over IPC)
//   - onInstalled / onStartup (background fires onInstalled synthetically)
//
// What we do NOT implement (these would crash Phantom if it hit them, so
// record them as known-gaps):
//   - chrome.runtime.getPlatformInfo  — stubbed, returns Windows constants
//   - chrome.runtime.getURL for non-extension://  — we return our tauri://
//     scheme; extensions that URL.parse() the result get something valid
//   - chrome.runtime.reload / requestUpdateCheck  — no-ops
//   - chrome.runtime.lastError  — always null; we use promise rejection
//
// The onMessage contract is load-bearing. Chrome's rule is:
//   listener returns `true` → sendResponse can be called async (we keep the
//       channel open).
//   listener returns a Promise → we await the promise and use its value as
//       the response.
//   listener returns any other value → response is whatever the listener
//       passed to sendResponse synchronously, OR undefined.
// Only the first listener that "claims" the message gets to respond. If no
// listener claims it, the sender's promise resolves to `undefined`. This
// mirrors the behaviour wallet extensions (Phantom, MetaMask) rely on.

import type {
  InboundConnect,
  InboundMessage,
  MessageSender,
  PortInbound,
  SendMessageResult,
  Surface,
} from "./types.js";
import { RuntimeCommands, RuntimeEvents } from "./types.js";
import { ChromeEvent } from "./events.js";
import { invokeWithRetry, getEventApi } from "./tauri.js";
import {
  getExtensionId,
  getFrameId,
  getManifest,
  getResourceBase,
  getSurface,
} from "./config.js";
import { trace, traceStub } from "./trace.js";

// ---- Port (chrome.runtime.Port) ----------------------------------------

export interface PortLike {
  name: string;
  sender?: MessageSender;
  onMessage: ChromeEvent<[message: unknown, port: PortLike]>;
  onDisconnect: ChromeEvent<[port: PortLike]>;
  postMessage(message: unknown): void;
  disconnect(): void;
}

interface PortInternal extends PortLike {
  _portId: string;
  _handleInbound(message: unknown): void;
  _handleRemoteDisconnect(): void;
}

const ports = new Map<string, PortInternal>();

function newPortId(): string {
  // UUID v4-ish without bringing in a dep. Good enough for in-process
  // port ids.
  const rnd = () =>
    Math.floor(Math.random() * 0x10000)
      .toString(16)
      .padStart(4, "0");
  return `${rnd()}${rnd()}-${rnd()}-${rnd()}-${rnd()}-${rnd()}${rnd()}${rnd()}`;
}

function makePort(
  portId: string,
  name: string,
  sender?: MessageSender,
): PortInternal {
  // Buffer port messages that arrive before the controller (which receives the
  // port via onConnect) registers its port.onMessage listener — the wallet
  // provider handshake posts its first requests immediately on connect.
  const onMessage = new ChromeEvent<[message: unknown, port: PortLike]>(true);
  const onDisconnect = new ChromeEvent<[port: PortLike]>();

  let disconnected = false;

  const port: PortInternal = {
    _portId: portId,
    name,
    sender,
    onMessage,
    onDisconnect,
    postMessage(message: unknown) {
      if (disconnected) {
        trace("runtime", "postMessage on disconnected port", portId);
        return;
      }
      void invokeWithRetry(RuntimeCommands.RuntimePortPost, {
        portId,
        payload: message,
      }).catch((err) => {
        // eslint-disable-next-line no-console
        console.error("[chrome-shim] port.postMessage failed:", err);
      });
    },
    disconnect() {
      if (disconnected) return;
      disconnected = true;
      void invokeWithRetry(RuntimeCommands.RuntimePortDisconnect, {
        portId,
      }).catch(() => {
        /* already gone is fine */
      });
      ports.delete(portId);
      onDisconnect.emit(port);
    },
    _handleInbound(message: unknown) {
      if (disconnected) return;
      onMessage.emit(message, port);
    },
    _handleRemoteDisconnect() {
      if (disconnected) return;
      disconnected = true;
      ports.delete(portId);
      onDisconnect.emit(port);
    },
  };

  ports.set(portId, port);
  return port;
}

// ---- Event wiring ------------------------------------------------------

/// Lazily attach event listeners to the Tauri event bus. We only do this
/// from the background surface today because the content surface drives
/// everything through promise-returning invokes (the Rust side buffers
/// responses and returns them directly). When a port is opened from the
/// content surface we DO need event delivery, so `ensureEventWiring` is
/// also called on the first connect() or first onConnect.addListener.
let eventWiringAttached = false;
let eventWiringPromise: Promise<void> | null = null;

async function ensureEventWiring(): Promise<void> {
  if (eventWiringAttached) return;
  if (eventWiringPromise) return eventWiringPromise;
  eventWiringPromise = (async () => {
    const api = getEventApi();
    if (!api) {
      trace(
        "runtime",
        "Tauri event API not available; port/message delivery will be poll-only",
      );
      eventWiringAttached = true;
      return;
    }

    await api.listen<InboundMessage>(
      RuntimeEvents.InboundMessage,
      ({ payload }) => handleInboundMessage(payload),
    );
    await api.listen<InboundConnect>(
      RuntimeEvents.InboundConnect,
      ({ payload }) => handleInboundConnect(payload),
    );
    await api.listen<PortInbound>(
      RuntimeEvents.PortInbound,
      ({ payload }) => handlePortInbound(payload),
    );
    await api.listen<{ portId: string }>(
      RuntimeEvents.PortDisconnect,
      ({ payload }) => handlePortDisconnect(payload),
    );

    eventWiringAttached = true;
    trace("runtime", "event wiring attached");
  })();
  return eventWiringPromise;
}

/** Diagnostic counters (read by the minimal host's BG introspection) so a
 * headless content↔BG round-trip failure can be localized to a hop. */
function bumpInbound(kind: "message" | "connect" | "portInbound"): void {
  try {
    const g = globalThis as unknown as { __extInboundCount?: Record<string, number> };
    if (!g.__extInboundCount) g.__extInboundCount = { message: 0, connect: 0, portInbound: 0 };
    g.__extInboundCount[kind] = (g.__extInboundCount[kind] ?? 0) + 1;
  } catch {
    /* diagnostics only */
  }
}

function handleInboundMessage(msg: InboundMessage): void {
  bumpInbound("message");
  // Filter by extension id. The bus is shared across extensions; we only
  // fire listeners for our own.
  if (msg.extensionId !== getExtensionId()) return;
  let claimed = false;
  let responded = false;

  const sendResponse = (response?: unknown): void => {
    if (responded) return;
    responded = true;
    void invokeWithRetry(RuntimeCommands.RuntimeSendMessage, {
      requestId: msg.requestId,
      response,
      phase: "response",
    }).catch((err) => {
      // eslint-disable-next-line no-console
      console.error("[chrome-shim] sendResponse dispatch failed:", err);
    });
  };

  // Chrome's contract: iterate listeners, first one to return true or a
  // Promise claims the message (keeping the channel open for async
  // sendResponse). We route via emit() and inspect the return values in
  // order.
  const returns = onMessage.emit(msg.payload, msg.sender, sendResponse);
  for (const result of returns) {
    if (result === true) {
      claimed = true;
    } else if (result && typeof (result as Promise<unknown>).then === "function") {
      claimed = true;
      (result as Promise<unknown>).then(sendResponse, (err) => {
        sendResponse({ __error: String(err) });
      });
      break;
    }
  }

  // If no listener claimed the message and no one called sendResponse
  // synchronously, we still notify the sender so its promise resolves to
  // undefined rather than hanging.
  if (!claimed && !responded) {
    sendResponse(undefined);
  }
}

function handleInboundConnect(conn: InboundConnect): void {
  bumpInbound("connect");
  if (conn.extensionId !== getExtensionId()) return;
  const port = makePort(conn.portId, conn.name, conn.sender);
  onConnect.emit(port);
}

function handlePortInbound(msg: PortInbound): void {
  bumpInbound("portInbound");
  const port = ports.get(msg.portId);
  if (!port) return;
  port._handleInbound(msg.payload);
}

function handlePortDisconnect(msg: { portId: string }): void {
  const port = ports.get(msg.portId);
  if (!port) return;
  port._handleRemoteDisconnect();
}

// ---- Public runtime surface -------------------------------------------

const onMessage = new ChromeEvent<
  [message: unknown, sender: MessageSender, sendResponse: (r?: unknown) => void]
>();
// Buffer-until-first-listener: these "wake the worker" events may be emitted
// before the extension's controller (loaded asynchronously via importScripts /
// dynamic import) registers its listeners. Chrome delivers them once the worker
// is ready; we replay them to the first listener so the provider handshake and
// install/startup hooks aren't dropped.
const onConnect = new ChromeEvent<[port: PortLike]>(true);
const onInstalled = new ChromeEvent<[details: { reason: string }]>(true);
const onStartup = new ChromeEvent<[]>(true);
// External (cross-extension / web) messaging is out of scope, but the event
// objects must exist so `onConnectExternal.addListener(...)` (MetaMask does this
// at boot) doesn't throw on an undefined event. They simply never fire.
const onConnectExternal = new ChromeEvent<[port: PortLike]>();
const onMessageExternal = new ChromeEvent<
  [message: unknown, sender: MessageSender, sendResponse: (r?: unknown) => void]
>();

/** Diagnostic: count outbound connect/sendMessage from this surface, so a
 * content→BG round-trip failure can be bisected (does the content script even
 * call out?). Mirror of `bumpInbound`. Read via the page/BG introspection. */
function bumpOutbound(kind: "sendMessage" | "connect"): void {
  try {
    const g = globalThis as unknown as { __extOutboundCount?: Record<string, number> };
    if (!g.__extOutboundCount) g.__extOutboundCount = { sendMessage: 0, connect: 0 };
    g.__extOutboundCount[kind] = (g.__extOutboundCount[kind] ?? 0) + 1;
  } catch {
    /* diagnostics only */
  }
}

function getURL(path: string): string {
  const id = getExtensionId();
  const cleaned = path.startsWith("/") ? path.slice(1) : path;
  // `<resourceBase>/<id>/<path>` — resourceBase is the platform-specific origin
  // the Rust host serves web_accessible_resources from (configure() supplies
  // it). A content script's `<script src=getURL("inpage.js")>` resolves here.
  return `${getResourceBase()}/${id}/${cleaned}`;
}

async function sendMessage(
  ...args: unknown[]
): Promise<unknown> {
  // Chrome's signature overload handling:
  //   sendMessage(message)
  //   sendMessage(message, options)
  //   sendMessage(message, callback)
  //   sendMessage(message, options, callback)
  //   sendMessage(extensionId, message)
  //   sendMessage(extensionId, message, options)
  //   sendMessage(extensionId, message, options, callback)
  //
  // For the spike we accept either "intra-extension" or the explicit
  // extension-id form. The Rust side only cares about the payload + target
  // surface.
  let extensionId = getExtensionId();
  let message: unknown;
  let options: Record<string, unknown> | undefined;
  let callback: ((r: unknown) => void) | undefined;

  if (typeof args[0] === "string" && args.length > 1) {
    extensionId = args[0] as string;
    message = args[1];
    if (typeof args[2] === "function") callback = args[2] as typeof callback;
    else {
      options = args[2] as Record<string, unknown> | undefined;
      if (typeof args[3] === "function") callback = args[3] as typeof callback;
    }
  } else {
    message = args[0];
    if (typeof args[1] === "function") callback = args[1] as typeof callback;
    else {
      options = args[1] as Record<string, unknown> | undefined;
      if (typeof args[2] === "function") callback = args[2] as typeof callback;
    }
  }

  const target: Surface = getSurface() === "background" ? "content" : "background";
  bumpOutbound("sendMessage");

  await ensureEventWiring();

  const promise = invokeWithRetry<SendMessageResult>(
    RuntimeCommands.RuntimeSendMessage,
    {
      from: getSurface(),
      to: target,
      extensionId,
      frameId: getFrameId(),
      payload: message,
      options: options ?? {},
    },
  ).then((result) => {
    if (!result.ok) {
      throw new Error(result.error ?? "sendMessage failed");
    }
    return result.response;
  });

  if (callback) {
    promise.then(
      (r) => callback!(r),
      (err) => {
        // In classic Chrome, callback errors surface via chrome.runtime.lastError.
        // eslint-disable-next-line no-console
        console.error("[chrome-shim] sendMessage (callback) rejected:", err);
        callback!(undefined);
      },
    );
    return undefined;
  }

  return promise;
}

function connect(connectInfo?: { name?: string; includeTlsChannelId?: boolean }): PortLike {
  const portId = newPortId();
  const name = connectInfo?.name ?? "";
  bumpOutbound("connect");
  void ensureEventWiring();
  const port = makePort(portId, name);
  void invokeWithRetry(RuntimeCommands.RuntimeConnect, {
    portId,
    from: getSurface(),
    to: getSurface() === "background" ? "content" : "background",
    extensionId: getExtensionId(),
    name,
  }).catch((err) => {
    // eslint-disable-next-line no-console
    console.error("[chrome-shim] connect failed:", err);
    port._handleRemoteDisconnect();
  });
  return port;
}

function getPlatformInfo(): Promise<{ os: string; arch: string; nacl_arch: string }> {
  // Windows-only spike per D-003. Values shaped like Chrome's.
  return Promise.resolve({ os: "win", arch: "x86-64", nacl_arch: "x86-64" });
}

function reload(): void {
  traceStub("runtime", "reload()");
}

function requestUpdateCheck(): Promise<{ status: string }> {
  traceStub("runtime", "requestUpdateCheck()");
  return Promise.resolve({ status: "no_update" });
}

function setUninstallURL(_url?: string): Promise<void> {
  // No browser-managed uninstall flow in this host; accept + resolve so
  // worker init chains that `await chrome.runtime.setUninstallURL(...)` (Rabby
  // does, in its controller boot) don't reject and abort.
  traceStub("runtime", "setUninstallURL()");
  return Promise.resolve();
}

function openOptionsPage(): Promise<void> {
  traceStub("runtime", "openOptionsPage()");
  return Promise.resolve();
}

export function createRuntime() {
  return {
    get id(): string {
      return getExtensionId();
    },
    getURL,
    getManifest,
    sendMessage,
    connect,
    onMessage,
    onConnect,
    onInstalled,
    onStartup,
    onConnectExternal,
    onMessageExternal,
    getPlatformInfo,
    reload,
    requestUpdateCheck,
    setUninstallURL,
    openOptionsPage,
    lastError: undefined as undefined | { message: string },
  };
}

/// Internal: the BG bootstrap uses this to fire the synthetic onInstalled.
export function fireOnInstalled(reason: "install" | "update" | "chrome_update"): void {
  onInstalled.emit({ reason });
}

/// Internal: the BG bootstrap fires this after the extension's background
/// module has been evaluated.
export function fireOnStartup(): void {
  onStartup.emit();
}

/// Internal for the content bootstrap: proactively attach event wiring
/// even if no listeners have been registered yet, so late-arriving port
/// messages do not drop.
export function attachRuntimeEvents(): Promise<void> {
  return ensureEventWiring();
}
