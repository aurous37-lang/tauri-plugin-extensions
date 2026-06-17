// Stub implementations of chrome.* surfaces that the spike does not drive
// end-to-end but that loaded extensions are likely to feature-detect at
// boot. Each stub is:
//   - the correct shape (so feature detection passes),
//   - logs via trace() when invoked (so we can see which stubs Phantom
//     actually hits and prioritise),
//   - returns a plausible resolved promise / empty value.

import { ChromeEvent } from "./events.js";
import { traceStub } from "./trace.js";
import { invokeWithRetry } from "./tauri.js";
import { getExtensionId } from "./config.js";
import { RuntimeCommands } from "./types.js";

export function createAction() {
  return {
    setBadgeText(_details: { text: string; tabId?: number }): Promise<void> {
      traceStub("action", "setBadgeText", _details);
      return Promise.resolve();
    },
    setBadgeBackgroundColor(_details: {
      color: string | [number, number, number, number];
      tabId?: number;
    }): Promise<void> {
      traceStub("action", "setBadgeBackgroundColor", _details);
      return Promise.resolve();
    },
    setIcon(_details: {
      path?: string | Record<string, string>;
      imageData?: unknown;
      tabId?: number;
    }): Promise<void> {
      traceStub("action", "setIcon", _details);
      return Promise.resolve();
    },
    setTitle(_details: { title: string; tabId?: number }): Promise<void> {
      traceStub("action", "setTitle", _details);
      return Promise.resolve();
    },
    setPopup(_details: { popup: string; tabId?: number }): Promise<void> {
      traceStub("action", "setPopup", _details);
      return Promise.resolve();
    },
    getBadgeText(_details: { tabId?: number }): Promise<string> {
      traceStub("action", "getBadgeText", _details);
      return Promise.resolve("");
    },
    enable(_tabId?: number): Promise<void> {
      traceStub("action", "enable", _tabId);
      return Promise.resolve();
    },
    disable(_tabId?: number): Promise<void> {
      traceStub("action", "disable", _tabId);
      return Promise.resolve();
    },
    onClicked: new ChromeEvent<[tab: unknown]>(),
  };
}

/** One entry of `chrome.scripting.registerContentScripts`. */
interface RegisteredContentScript {
  id: string;
  matches?: string[];
  js?: string[];
  css?: string[];
  runAt?: "document_start" | "document_end" | "document_idle";
  world?: "ISOLATED" | "MAIN";
  allFrames?: boolean;
  persistAcrossSessions?: boolean;
}

export function createScripting() {
  return {
    // executeScript still a no-op: it injects into a tab by id, which needs a
    // chrome.tabs abstraction this runtime doesn't have. Recorded as a gap.
    executeScript(_injection: unknown): Promise<unknown[]> {
      traceStub("scripting", "executeScript", _injection);
      return Promise.resolve([]);
    },
    insertCSS(_injection: unknown): Promise<void> {
      traceStub("scripting", "insertCSS", _injection);
      return Promise.resolve();
    },
    removeCSS(_injection: unknown): Promise<void> {
      traceStub("scripting", "removeCSS", _injection);
      return Promise.resolve();
    },
    // registerContentScripts is REAL: wallets register their EVM inpage
    // provider this way (Phantom's evm*.js, world: "MAIN"). It routes to the
    // Rust DynamicScriptStore, which merges the registration into the
    // on_page_load injection flow so the script reaches future page loads.
    registerContentScripts(scripts: RegisteredContentScript[]): Promise<void> {
      return invokeWithRetry<void>(
        RuntimeCommands.ScriptingRegisterContentScripts,
        { extensionId: getExtensionId(), scripts },
      );
    },
    unregisterContentScripts(filter?: { ids?: string[] }): Promise<void> {
      return invokeWithRetry<void>(
        RuntimeCommands.ScriptingUnregisterContentScripts,
        { extensionId: getExtensionId(), ids: filter?.ids },
      );
    },
    getRegisteredContentScripts(
      filter?: { ids?: string[] },
    ): Promise<RegisteredContentScript[]> {
      return invokeWithRetry<RegisteredContentScript[]>(
        RuntimeCommands.ScriptingGetRegisteredContentScripts,
        { extensionId: getExtensionId(), ids: filter?.ids },
      );
    },
  };
}

export function createI18n() {
  return {
    getMessage(name: string, _substitutions?: unknown): string {
      // i18n is out of scope for the spike; returning the message name is
      // the honest "not translated" signal and avoids crashing UIs that
      // templateString the return value.
      return name;
    },
    getUILanguage(): string {
      return typeof navigator !== "undefined" ? navigator.language : "en-US";
    },
    getAcceptLanguages(callback?: (langs: string[]) => void): Promise<string[]> {
      const langs =
        typeof navigator !== "undefined"
          ? Array.from(navigator.languages ?? [navigator.language])
          : ["en-US"];
      const p = Promise.resolve(langs);
      if (callback) p.then(callback);
      return p;
    },
    detectLanguage(_text: string): Promise<{ isReliable: boolean; languages: unknown[] }> {
      traceStub("i18n", "detectLanguage");
      return Promise.resolve({ isReliable: false, languages: [] });
    },
  };
}

// ---------------------------------------------------------------------------
// Absent-namespace survival stubs.
//
// Wallet background service workers touch chrome.{management,tabs,alarms,
// offscreen,windows,webNavigation,idle,notifications} during init. Before
// these existed, the FIRST access threw a TypeError that aborted the whole
// worker (the BG source runs in one try/catch IIFE), so MetaMask died on
// `chrome.management.getSelf` and Rabby on `chrome.tabs.onActivated` /
// `chrome.alarms.getAll` (see docs/evm-injection-findings.md). These stubs
// keep the worker alive past those calls and record each as a gap via
// traceStub → window.__extApiGaps. They are deliberately benign no-ops: full
// implementations need a tab/window abstraction this runtime doesn't have.
// ---------------------------------------------------------------------------

/** Build a method that records the gap then resolves to a benign value. */
function gapMethod<T>(area: string, name: string, value: T) {
  return (...args: unknown[]): Promise<T> => {
    traceStub(area, name, ...args);
    return Promise.resolve(value);
  };
}

export function createManagement() {
  return {
    // MetaMask reads installType from getSelf during boot — return a plausible
    // unpacked-extension self-descriptor instead of throwing.
    getSelf: gapMethod("management", "getSelf", {
      id: "",
      name: "",
      enabled: true,
      installType: "development",
      version: "0",
      mayDisable: false,
    } as Record<string, unknown>),
    get: gapMethod("management", "get", {} as Record<string, unknown>),
    getAll: gapMethod("management", "getAll", [] as unknown[]),
    setEnabled: gapMethod("management", "setEnabled", undefined),
    uninstallSelf: gapMethod("management", "uninstallSelf", undefined),
    onInstalled: new ChromeEvent<[info: unknown]>(),
    onUninstalled: new ChromeEvent<[id: string]>(),
  };
}

export function createTabs() {
  return {
    query: gapMethod("tabs", "query", [] as unknown[]),
    get: gapMethod("tabs", "get", {} as Record<string, unknown>),
    getCurrent: gapMethod("tabs", "getCurrent", undefined),
    create: gapMethod("tabs", "create", {} as Record<string, unknown>),
    update: gapMethod("tabs", "update", {} as Record<string, unknown>),
    remove: gapMethod("tabs", "remove", undefined),
    reload: gapMethod("tabs", "reload", undefined),
    // BG → content push has no tab abstraction in v1; resolve undefined like a
    // message to a non-existent receiver rather than throwing.
    sendMessage: gapMethod("tabs", "sendMessage", undefined),
    onActivated: new ChromeEvent<[info: unknown]>(),
    onUpdated: new ChromeEvent<[tabId: number, change: unknown, tab: unknown]>(),
    onRemoved: new ChromeEvent<[tabId: number, info: unknown]>(),
    onCreated: new ChromeEvent<[tab: unknown]>(),
    onReplaced: new ChromeEvent<[added: number, removed: number]>(),
    TAB_ID_NONE: -1,
  };
}

export function createAlarms() {
  return {
    create: (...args: unknown[]): void => {
      traceStub("alarms", "create", ...args);
    },
    get: gapMethod("alarms", "get", undefined),
    getAll: gapMethod("alarms", "getAll", [] as unknown[]),
    clear: gapMethod("alarms", "clear", true),
    clearAll: gapMethod("alarms", "clearAll", true),
    onAlarm: new ChromeEvent<[alarm: unknown]>(),
  };
}

export function createOffscreen() {
  return {
    // Rabby creates an offscreen document for hardware-wallet bridging. We have
    // no offscreen-document host, so report "no document" and resolve creates.
    hasDocument: gapMethod("offscreen", "hasDocument", false),
    createDocument: gapMethod("offscreen", "createDocument", undefined),
    closeDocument: gapMethod("offscreen", "closeDocument", undefined),
  };
}

export function createWindows() {
  return {
    get: gapMethod("windows", "get", {} as Record<string, unknown>),
    getCurrent: gapMethod("windows", "getCurrent", {} as Record<string, unknown>),
    getLastFocused: gapMethod("windows", "getLastFocused", {} as Record<string, unknown>),
    getAll: gapMethod("windows", "getAll", [] as unknown[]),
    create: gapMethod("windows", "create", {} as Record<string, unknown>),
    update: gapMethod("windows", "update", {} as Record<string, unknown>),
    remove: gapMethod("windows", "remove", undefined),
    onCreated: new ChromeEvent<[win: unknown]>(),
    onRemoved: new ChromeEvent<[id: number]>(),
    onFocusChanged: new ChromeEvent<[id: number]>(),
    WINDOW_ID_NONE: -1,
    WINDOW_ID_CURRENT: -2,
  };
}

export function createWebNavigation() {
  return {
    getFrame: gapMethod("webNavigation", "getFrame", undefined),
    getAllFrames: gapMethod("webNavigation", "getAllFrames", [] as unknown[]),
    onBeforeNavigate: new ChromeEvent<[details: unknown]>(),
    onCommitted: new ChromeEvent<[details: unknown]>(),
    onDOMContentLoaded: new ChromeEvent<[details: unknown]>(),
    onCompleted: new ChromeEvent<[details: unknown]>(),
    onErrorOccurred: new ChromeEvent<[details: unknown]>(),
    onHistoryStateUpdated: new ChromeEvent<[details: unknown]>(),
  };
}

export function createIdle() {
  return {
    queryState: gapMethod("idle", "queryState", "active"),
    setDetectionInterval: (...args: unknown[]): void => {
      traceStub("idle", "setDetectionInterval", ...args);
    },
    getAutoLockDelay: gapMethod("idle", "getAutoLockDelay", 0),
    onStateChanged: new ChromeEvent<[state: string]>(),
  };
}

export function createNotifications() {
  return {
    create: gapMethod("notifications", "create", ""),
    update: gapMethod("notifications", "update", true),
    clear: gapMethod("notifications", "clear", true),
    getAll: gapMethod("notifications", "getAll", {} as Record<string, unknown>),
    getPermissionLevel: gapMethod("notifications", "getPermissionLevel", "granted"),
    onClicked: new ChromeEvent<[id: string]>(),
    onClosed: new ChromeEvent<[id: string, byUser: boolean]>(),
    onButtonClicked: new ChromeEvent<[id: string, idx: number]>(),
    onShowSettings: new ChromeEvent<[]>(),
  };
}

export function createWebRequest() {
  // webRequest events use a richer addListener(listener, filter, extraInfoSpec);
  // our ChromeEvent.addListener ignores the extra args (JS tolerates them), so
  // `chrome.webRequest.onBeforeSendHeaders.addListener(fn, filter, spec)` (which
  // Phantom calls at boot) doesn't throw. The listeners never fire — we don't
  // intercept network requests — which is fine for provider injection.
  const ev = () => new ChromeEvent<unknown[]>();
  return {
    onBeforeRequest: ev(),
    onBeforeSendHeaders: ev(),
    onSendHeaders: ev(),
    onHeadersReceived: ev(),
    onAuthRequired: ev(),
    onResponseStarted: ev(),
    onBeforeRedirect: ev(),
    onCompleted: ev(),
    onErrorOccurred: ev(),
    onActionIgnored: ev(),
    handlerBehaviorChanged: gapMethod("webRequest", "handlerBehaviorChanged", undefined),
  };
}

export function createIdentity() {
  return {
    // getRedirectURL is SYNCHRONOUS and returns a string — Phantom's EVM
    // service worker calls it at boot (and dies if it's absent). Mirror
    // Chrome's `https://<id>.chromiumapp.org/<path>` shape.
    getRedirectURL(path?: string): string {
      traceStub("identity", "getRedirectURL", path);
      const id = getExtensionId();
      const suffix = (path ?? "").replace(/^\//, "");
      return `https://${id}.chromiumapp.org/${suffix}`;
    },
    launchWebAuthFlow: gapMethod("identity", "launchWebAuthFlow", undefined),
    getAuthToken: gapMethod(
      "identity",
      "getAuthToken",
      { token: "" } as Record<string, unknown>,
    ),
    removeCachedAuthToken: gapMethod("identity", "removeCachedAuthToken", undefined),
    clearAllCachedAuthTokens: gapMethod("identity", "clearAllCachedAuthTokens", undefined),
    getProfileUserInfo: gapMethod(
      "identity",
      "getProfileUserInfo",
      { email: "", id: "" } as Record<string, unknown>,
    ),
    getAccounts: gapMethod("identity", "getAccounts", [] as unknown[]),
    onSignInChanged: new ChromeEvent<[account: unknown, signedIn: boolean]>(),
  };
}

export function createPermissions() {
  return {
    contains(_perm: { permissions?: string[]; origins?: string[] }): Promise<boolean> {
      // Spike: all declared manifest permissions are implicitly granted.
      return Promise.resolve(true);
    },
    request(_perm: { permissions?: string[]; origins?: string[] }): Promise<boolean> {
      traceStub("permissions", "request", _perm);
      return Promise.resolve(true);
    },
    getAll(): Promise<{ permissions: string[]; origins: string[] }> {
      return Promise.resolve({ permissions: [], origins: [] });
    },
    remove(_perm: { permissions?: string[]; origins?: string[] }): Promise<boolean> {
      traceStub("permissions", "remove", _perm);
      return Promise.resolve(true);
    },
    onAdded: new ChromeEvent<[perm: unknown]>(),
    onRemoved: new ChromeEvent<[perm: unknown]>(),
  };
}
