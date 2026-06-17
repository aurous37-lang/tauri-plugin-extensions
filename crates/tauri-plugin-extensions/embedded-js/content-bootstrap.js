"use strict";
var __TauriExtContent = (() => {
  // src/shared/trace.ts
  var enabled = false;
  function setTrace(on) {
    enabled = on;
  }
  function trace(area, msg, ...data) {
    if (!enabled) return;
    console.log(`[chrome-shim:${area}]`, msg, ...data);
  }
  function recordGap(area, method) {
    try {
      const g = globalThis;
      if (!g.__extApiGaps) g.__extApiGaps = [];
      const entry = `${area}.${method}`;
      const buf = g.__extApiGaps;
      if (buf[buf.length - 1] !== entry) buf.push(entry);
      if (buf.length > 100) buf.shift();
    } catch {
    }
  }
  function traceStub(area, method, ...args) {
    recordGap(area, method);
    if (!enabled) return;
    console.warn(`[chrome-shim:${area}] stub: ${method}`, ...args);
  }

  // src/shared/config.ts
  var state = {
    extensionId: "__EXT_ID__",
    manifest: {},
    surface: "content",
    frameId: 0,
    // Dead default until configure() supplies the real, platform-specific
    // resource origin. A getURL() before configure points at nothing — same
    // contract as the extensionId placeholder above.
    resourceBase: "tauri://extension-resource",
    ready: false
  };
  var readyResolvers = [];
  function configure(opts) {
    if (state.ready) {
      if (state.extensionId !== opts.extensionId) {
        console.warn(
          "[chrome-shim] configure() called twice with different extensionId; ignoring second call"
        );
      }
      return;
    }
    state.extensionId = opts.extensionId;
    state.manifest = opts.manifest ?? {};
    state.surface = opts.surface;
    state.frameId = opts.frameId ?? 0;
    if (opts.resourceBase) state.resourceBase = opts.resourceBase;
    state.ready = true;
    if (opts.trace) setTrace(true);
    for (const resolve of readyResolvers.splice(0)) resolve();
  }
  function setDefaultSurface(surface) {
    if (!state.ready) state.surface = surface;
  }
  function getExtensionId() {
    return state.extensionId;
  }
  function getManifest() {
    return state.manifest;
  }
  function getSurface() {
    return state.surface;
  }
  function getFrameId() {
    return state.frameId;
  }
  function getResourceBase() {
    return state.resourceBase;
  }

  // src/shared/types.ts
  var PLUGIN_NAME = "extensions";
  var RuntimeCommands = {
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
    StorageClear: `plugin:${PLUGIN_NAME}|extensions_storage_clear`
  };
  var RuntimeEvents = {
    /// Inbound chrome.runtime message for this surface. Payload: `InboundMessage`.
    InboundMessage: `${PLUGIN_NAME}://runtime/message`,
    /// Inbound chrome.runtime.connect from another surface. Payload: `InboundConnect`.
    InboundConnect: `${PLUGIN_NAME}://runtime/connect`,
    /// Inbound message on an existing port. Payload: `PortInbound`.
    PortInbound: `${PLUGIN_NAME}://runtime/port_message`,
    /// Port-disconnect notification. Payload: `{ portId, reason? }`.
    PortDisconnect: `${PLUGIN_NAME}://runtime/port_disconnect`,
    /// storage.onChanged fan-out. Payload: `StorageChangeEvent`.
    StorageChanged: `${PLUGIN_NAME}://storage/changed`
  };

  // src/shared/events.ts
  var ChromeEvent = class {
    #listeners = [];
    // When buffering is on, events emitted while there are NO listeners are
    // queued and replayed to the first listener that registers. This emulates
    // Chrome's service-worker semantics, where an event (e.g. onConnect, a port
    // message) starts the worker and is delivered AFTER its top-level listeners
    // register — in this document host the extension's controller may register
    // its listeners asynchronously (after an importScripts'd controller boots),
    // so an early connect/message would otherwise be dropped on the floor and the
    // wallet's provider handshake would hang. Off by default (most events should
    // drop when unhandled, matching Chrome).
    #bufferWhenEmpty;
    #buffer = [];
    constructor(bufferWhenEmpty = false) {
      this.#bufferWhenEmpty = bufferWhenEmpty;
    }
    addListener(cb) {
      if (typeof cb !== "function") {
        throw new TypeError("addListener: callback must be a function");
      }
      if (!this.#listeners.includes(cb)) {
        this.#listeners.push(cb);
      }
      if (this.#buffer.length > 0) {
        const queued = this.#buffer;
        this.#buffer = [];
        for (const args of queued) {
          try {
            cb(...args);
          } catch (err) {
            console.error("[chrome-shim] buffered listener threw:", err);
          }
        }
      }
    }
    removeListener(cb) {
      const idx = this.#listeners.indexOf(cb);
      if (idx >= 0) this.#listeners.splice(idx, 1);
    }
    hasListener(cb) {
      return this.#listeners.includes(cb);
    }
    hasListeners() {
      return this.#listeners.length > 0;
    }
    /// Fire all listeners; returns the array of (non-undefined) return values
    /// so callers can decide whether any listener claimed the message (the
    /// Chrome contract for `onMessage`: returning `true` keeps the channel
    /// open; returning a promise resolves to `sendResponse`).
    emit(...args) {
      const results = [];
      const snapshot = this.#listeners.slice();
      if (snapshot.length === 0 && this.#bufferWhenEmpty) {
        this.#buffer.push(args);
        return results;
      }
      for (const listener of snapshot) {
        try {
          results.push(listener(...args));
        } catch (err) {
          console.error("[chrome-shim] listener threw:", err);
        }
      }
      return results;
    }
  };

  // src/shared/tauri.ts
  function getInvoke() {
    const g = globalThis;
    const fn = g.__TAURI_INTERNALS__?.invoke ?? g.__TAURI__?.invoke ?? void 0;
    if (typeof fn !== "function") {
      throw new Error(
        "tauri-plugin-extensions: Tauri invoke bridge unavailable; script injected outside a Tauri webview?"
      );
    }
    return fn;
  }
  function getEventApi() {
    const g = globalThis;
    if (g.__TAURI_EVENT__?.listen) return g.__TAURI_EVENT__;
    if (g.__TAURI__?.event?.listen) return g.__TAURI__.event;
    return null;
  }
  async function invokeWithRetry(cmd, args) {
    const invoke = getInvoke();
    try {
      return await invoke(cmd, args);
    } catch (err) {
      await new Promise((resolve) => queueMicrotask(resolve));
      try {
        return await invoke(cmd, args);
      } catch {
        throw err;
      }
    }
  }

  // src/shared/runtime.ts
  var ports = /* @__PURE__ */ new Map();
  function newPortId() {
    const rnd = () => Math.floor(Math.random() * 65536).toString(16).padStart(4, "0");
    return `${rnd()}${rnd()}-${rnd()}-${rnd()}-${rnd()}-${rnd()}${rnd()}${rnd()}`;
  }
  function makePort(portId, name, sender) {
    const onMessage2 = new ChromeEvent(true);
    const onDisconnect = new ChromeEvent();
    let disconnected = false;
    const port = {
      _portId: portId,
      name,
      sender,
      onMessage: onMessage2,
      onDisconnect,
      postMessage(message) {
        if (disconnected) {
          trace("runtime", "postMessage on disconnected port", portId);
          return;
        }
        void invokeWithRetry(RuntimeCommands.RuntimePortPost, {
          portId,
          payload: message
        }).catch((err) => {
          console.error("[chrome-shim] port.postMessage failed:", err);
        });
      },
      disconnect() {
        if (disconnected) return;
        disconnected = true;
        void invokeWithRetry(RuntimeCommands.RuntimePortDisconnect, {
          portId
        }).catch(() => {
        });
        ports.delete(portId);
        onDisconnect.emit(port);
      },
      _handleInbound(message) {
        if (disconnected) return;
        onMessage2.emit(message, port);
      },
      _handleRemoteDisconnect() {
        if (disconnected) return;
        disconnected = true;
        ports.delete(portId);
        onDisconnect.emit(port);
      }
    };
    ports.set(portId, port);
    return port;
  }
  var eventWiringAttached = false;
  var eventWiringPromise = null;
  async function ensureEventWiring() {
    if (eventWiringAttached) return;
    if (eventWiringPromise) return eventWiringPromise;
    eventWiringPromise = (async () => {
      const api = getEventApi();
      if (!api) {
        trace(
          "runtime",
          "Tauri event API not available; port/message delivery will be poll-only"
        );
        eventWiringAttached = true;
        return;
      }
      await api.listen(
        RuntimeEvents.InboundMessage,
        ({ payload }) => handleInboundMessage(payload)
      );
      await api.listen(
        RuntimeEvents.InboundConnect,
        ({ payload }) => handleInboundConnect(payload)
      );
      await api.listen(
        RuntimeEvents.PortInbound,
        ({ payload }) => handlePortInbound(payload)
      );
      await api.listen(
        RuntimeEvents.PortDisconnect,
        ({ payload }) => handlePortDisconnect(payload)
      );
      eventWiringAttached = true;
      trace("runtime", "event wiring attached");
    })();
    return eventWiringPromise;
  }
  function bumpInbound(kind) {
    try {
      const g = globalThis;
      if (!g.__extInboundCount) g.__extInboundCount = { message: 0, connect: 0, portInbound: 0 };
      g.__extInboundCount[kind] = (g.__extInboundCount[kind] ?? 0) + 1;
    } catch {
    }
  }
  function handleInboundMessage(msg) {
    bumpInbound("message");
    if (msg.extensionId !== getExtensionId()) return;
    let claimed = false;
    let responded = false;
    const sendResponse = (response) => {
      if (responded) return;
      responded = true;
      void invokeWithRetry(RuntimeCommands.RuntimeSendMessage, {
        requestId: msg.requestId,
        response,
        phase: "response"
      }).catch((err) => {
        console.error("[chrome-shim] sendResponse dispatch failed:", err);
      });
    };
    const returns = onMessage.emit(msg.payload, msg.sender, sendResponse);
    for (const result of returns) {
      if (result === true) {
        claimed = true;
      } else if (result && typeof result.then === "function") {
        claimed = true;
        result.then(sendResponse, (err) => {
          sendResponse({ __error: String(err) });
        });
        break;
      }
    }
    if (!claimed && !responded) {
      sendResponse(void 0);
    }
  }
  function handleInboundConnect(conn) {
    bumpInbound("connect");
    if (conn.extensionId !== getExtensionId()) return;
    const port = makePort(conn.portId, conn.name, conn.sender);
    onConnect.emit(port);
  }
  function handlePortInbound(msg) {
    bumpInbound("portInbound");
    const port = ports.get(msg.portId);
    if (!port) return;
    port._handleInbound(msg.payload);
  }
  function handlePortDisconnect(msg) {
    const port = ports.get(msg.portId);
    if (!port) return;
    port._handleRemoteDisconnect();
  }
  var onMessage = new ChromeEvent();
  var onConnect = new ChromeEvent(true);
  var onInstalled = new ChromeEvent(true);
  var onStartup = new ChromeEvent(true);
  var onConnectExternal = new ChromeEvent();
  var onMessageExternal = new ChromeEvent();
  function bumpOutbound(kind) {
    try {
      const g = globalThis;
      if (!g.__extOutboundCount) g.__extOutboundCount = { sendMessage: 0, connect: 0 };
      g.__extOutboundCount[kind] = (g.__extOutboundCount[kind] ?? 0) + 1;
    } catch {
    }
  }
  function getURL(path) {
    const id = getExtensionId();
    const cleaned = path.startsWith("/") ? path.slice(1) : path;
    return `${getResourceBase()}/${id}/${cleaned}`;
  }
  async function sendMessage(...args) {
    let extensionId = getExtensionId();
    let message;
    let options;
    let callback;
    if (typeof args[0] === "string" && args.length > 1) {
      extensionId = args[0];
      message = args[1];
      if (typeof args[2] === "function") callback = args[2];
      else {
        options = args[2];
        if (typeof args[3] === "function") callback = args[3];
      }
    } else {
      message = args[0];
      if (typeof args[1] === "function") callback = args[1];
      else {
        options = args[1];
        if (typeof args[2] === "function") callback = args[2];
      }
    }
    const target = getSurface() === "background" ? "content" : "background";
    bumpOutbound("sendMessage");
    await ensureEventWiring();
    const promise = invokeWithRetry(
      RuntimeCommands.RuntimeSendMessage,
      {
        from: getSurface(),
        to: target,
        extensionId,
        frameId: getFrameId(),
        payload: message,
        options: options ?? {}
      }
    ).then((result) => {
      if (!result.ok) {
        throw new Error(result.error ?? "sendMessage failed");
      }
      return result.response;
    });
    if (callback) {
      promise.then(
        (r) => callback(r),
        (err) => {
          console.error("[chrome-shim] sendMessage (callback) rejected:", err);
          callback(void 0);
        }
      );
      return void 0;
    }
    return promise;
  }
  function connect(connectInfo) {
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
      name
    }).catch((err) => {
      console.error("[chrome-shim] connect failed:", err);
      port._handleRemoteDisconnect();
    });
    return port;
  }
  function getPlatformInfo() {
    return Promise.resolve({ os: "win", arch: "x86-64", nacl_arch: "x86-64" });
  }
  function reload() {
    traceStub("runtime", "reload()");
  }
  function requestUpdateCheck() {
    traceStub("runtime", "requestUpdateCheck()");
    return Promise.resolve({ status: "no_update" });
  }
  function setUninstallURL(_url) {
    traceStub("runtime", "setUninstallURL()");
    return Promise.resolve();
  }
  function openOptionsPage() {
    traceStub("runtime", "openOptionsPage()");
    return Promise.resolve();
  }
  function createRuntime() {
    return {
      get id() {
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
      lastError: void 0
    };
  }
  function attachRuntimeEvents() {
    return ensureEventWiring();
  }

  // src/shared/storage.ts
  function normaliseGetArg(arg) {
    if (arg == null) return { keys: null, defaults: {} };
    if (typeof arg === "string") return { keys: [arg], defaults: {} };
    if (Array.isArray(arg)) return { keys: arg.slice(), defaults: {} };
    if (typeof arg === "object") {
      const keys = Object.keys(arg);
      return { keys, defaults: { ...arg } };
    }
    return { keys: null, defaults: {} };
  }
  function arrayifyKeys(keys) {
    return typeof keys === "string" ? [keys] : keys.slice();
  }
  var StorageAreaImpl = class {
    constructor(area) {
      this.area = area;
    }
    onChanged = new ChromeEvent();
    get(arg, callback) {
      const { keys, defaults } = normaliseGetArg(arg);
      const promise = invokeWithRetry(
        RuntimeCommands.StorageGet,
        {
          extensionId: getExtensionId(),
          area: this.area,
          keys
        }
      ).then((items) => {
        const out = { ...defaults };
        for (const [k, v] of Object.entries(items ?? {})) {
          out[k] = v;
        }
        return out;
      });
      if (callback) {
        promise.then(callback, (err) => {
          console.error("[chrome-shim] storage.get failed:", err);
          callback({});
        });
        return promise;
      }
      return promise;
    }
    set(items, callback) {
      const promise = invokeWithRetry(RuntimeCommands.StorageSet, {
        extensionId: getExtensionId(),
        area: this.area,
        items
      });
      if (callback) {
        promise.then(
          () => callback(),
          (err) => {
            console.error("[chrome-shim] storage.set failed:", err);
            callback();
          }
        );
      }
      return promise;
    }
    remove(keys, callback) {
      const promise = invokeWithRetry(RuntimeCommands.StorageRemove, {
        extensionId: getExtensionId(),
        area: this.area,
        keys: arrayifyKeys(keys)
      });
      if (callback) {
        promise.then(
          () => callback(),
          (err) => {
            console.error("[chrome-shim] storage.remove failed:", err);
            callback();
          }
        );
      }
      return promise;
    }
    clear(callback) {
      const promise = invokeWithRetry(RuntimeCommands.StorageClear, {
        extensionId: getExtensionId(),
        area: this.area
      });
      if (callback) {
        promise.then(
          () => callback(),
          (err) => {
            console.error("[chrome-shim] storage.clear failed:", err);
            callback();
          }
        );
      }
      return promise;
    }
    /// Approximate byte usage — Chrome's real API queries actual stored size.
    /// The Rust side can back this with a real count later; stubbed for now.
    getBytesInUse(_keys, callback) {
      const p = Promise.resolve(0);
      if (callback) p.then(callback);
      return p;
    }
  };
  var localArea = new StorageAreaImpl("local");
  var sessionArea = new StorageAreaImpl("session");
  var crossAreaOnChanged = new ChromeEvent();
  var changedWiringAttached = false;
  async function ensureChangedWiring() {
    if (changedWiringAttached) return;
    changedWiringAttached = true;
    const api = getEventApi();
    if (!api) {
      trace(
        "storage",
        "event API unavailable; onChanged will only fire for local writes"
      );
      return;
    }
    await api.listen(
      RuntimeEvents.StorageChanged,
      ({ payload }) => {
        if (payload.extensionId !== getExtensionId()) return;
        const target = payload.area === "session" ? sessionArea : localArea;
        target.onChanged.emit(payload.changes);
        crossAreaOnChanged.emit(payload.changes, payload.area);
      }
    );
  }
  function createStorage() {
    void ensureChangedWiring();
    return {
      local: localArea,
      session: sessionArea,
      onChanged: crossAreaOnChanged,
      /// `chrome.storage.sync` is out of scope but extensions often feature-
      /// detect it. We expose a degenerate object that behaves like local
      /// but errors on write, so a feature-detect `'sync' in chrome.storage`
      /// returns true without accidentally persisting unsynced data.
      get sync() {
        return {
          get: () => Promise.resolve({}),
          set: () => Promise.reject(new Error("chrome.storage.sync not supported")),
          remove: () => Promise.reject(new Error("chrome.storage.sync not supported")),
          clear: () => Promise.reject(new Error("chrome.storage.sync not supported")),
          onChanged: new ChromeEvent(),
          getBytesInUse: () => Promise.resolve(0)
        };
      }
    };
  }

  // src/shared/stubs.ts
  function createAction() {
    return {
      setBadgeText(_details) {
        traceStub("action", "setBadgeText", _details);
        return Promise.resolve();
      },
      setBadgeBackgroundColor(_details) {
        traceStub("action", "setBadgeBackgroundColor", _details);
        return Promise.resolve();
      },
      setIcon(_details) {
        traceStub("action", "setIcon", _details);
        return Promise.resolve();
      },
      setTitle(_details) {
        traceStub("action", "setTitle", _details);
        return Promise.resolve();
      },
      setPopup(_details) {
        traceStub("action", "setPopup", _details);
        return Promise.resolve();
      },
      getBadgeText(_details) {
        traceStub("action", "getBadgeText", _details);
        return Promise.resolve("");
      },
      enable(_tabId) {
        traceStub("action", "enable", _tabId);
        return Promise.resolve();
      },
      disable(_tabId) {
        traceStub("action", "disable", _tabId);
        return Promise.resolve();
      },
      onClicked: new ChromeEvent()
    };
  }
  function createScripting() {
    return {
      // executeScript still a no-op: it injects into a tab by id, which needs a
      // chrome.tabs abstraction this runtime doesn't have. Recorded as a gap.
      executeScript(_injection) {
        traceStub("scripting", "executeScript", _injection);
        return Promise.resolve([]);
      },
      insertCSS(_injection) {
        traceStub("scripting", "insertCSS", _injection);
        return Promise.resolve();
      },
      removeCSS(_injection) {
        traceStub("scripting", "removeCSS", _injection);
        return Promise.resolve();
      },
      // registerContentScripts is REAL: wallets register their EVM inpage
      // provider this way (Phantom's evm*.js, world: "MAIN"). It routes to the
      // Rust DynamicScriptStore, which merges the registration into the
      // on_page_load injection flow so the script reaches future page loads.
      registerContentScripts(scripts) {
        return invokeWithRetry(
          RuntimeCommands.ScriptingRegisterContentScripts,
          { extensionId: getExtensionId(), scripts }
        );
      },
      unregisterContentScripts(filter) {
        return invokeWithRetry(
          RuntimeCommands.ScriptingUnregisterContentScripts,
          { extensionId: getExtensionId(), ids: filter?.ids }
        );
      },
      getRegisteredContentScripts(filter) {
        return invokeWithRetry(
          RuntimeCommands.ScriptingGetRegisteredContentScripts,
          { extensionId: getExtensionId(), ids: filter?.ids }
        );
      }
    };
  }
  function createI18n() {
    return {
      getMessage(name, _substitutions) {
        return name;
      },
      getUILanguage() {
        return typeof navigator !== "undefined" ? navigator.language : "en-US";
      },
      getAcceptLanguages(callback) {
        const langs = typeof navigator !== "undefined" ? Array.from(navigator.languages ?? [navigator.language]) : ["en-US"];
        const p = Promise.resolve(langs);
        if (callback) p.then(callback);
        return p;
      },
      detectLanguage(_text) {
        traceStub("i18n", "detectLanguage");
        return Promise.resolve({ isReliable: false, languages: [] });
      }
    };
  }
  function gapMethod(area, name, value) {
    return (...args) => {
      traceStub(area, name, ...args);
      return Promise.resolve(value);
    };
  }
  function createManagement() {
    return {
      // MetaMask reads installType from getSelf during boot — return a plausible
      // unpacked-extension self-descriptor instead of throwing.
      getSelf: gapMethod("management", "getSelf", {
        id: "",
        name: "",
        enabled: true,
        installType: "development",
        version: "0",
        mayDisable: false
      }),
      get: gapMethod("management", "get", {}),
      getAll: gapMethod("management", "getAll", []),
      setEnabled: gapMethod("management", "setEnabled", void 0),
      uninstallSelf: gapMethod("management", "uninstallSelf", void 0),
      onInstalled: new ChromeEvent(),
      onUninstalled: new ChromeEvent()
    };
  }
  function createTabs() {
    return {
      query: gapMethod("tabs", "query", []),
      get: gapMethod("tabs", "get", {}),
      getCurrent: gapMethod("tabs", "getCurrent", void 0),
      create: gapMethod("tabs", "create", {}),
      update: gapMethod("tabs", "update", {}),
      remove: gapMethod("tabs", "remove", void 0),
      reload: gapMethod("tabs", "reload", void 0),
      // BG → content push has no tab abstraction in v1; resolve undefined like a
      // message to a non-existent receiver rather than throwing.
      sendMessage: gapMethod("tabs", "sendMessage", void 0),
      onActivated: new ChromeEvent(),
      onUpdated: new ChromeEvent(),
      onRemoved: new ChromeEvent(),
      onCreated: new ChromeEvent(),
      onReplaced: new ChromeEvent(),
      TAB_ID_NONE: -1
    };
  }
  function createAlarms() {
    return {
      create: (...args) => {
        traceStub("alarms", "create", ...args);
      },
      get: gapMethod("alarms", "get", void 0),
      getAll: gapMethod("alarms", "getAll", []),
      clear: gapMethod("alarms", "clear", true),
      clearAll: gapMethod("alarms", "clearAll", true),
      onAlarm: new ChromeEvent()
    };
  }
  function createOffscreen() {
    return {
      // Rabby creates an offscreen document for hardware-wallet bridging. We have
      // no offscreen-document host, so report "no document" and resolve creates.
      hasDocument: gapMethod("offscreen", "hasDocument", false),
      createDocument: gapMethod("offscreen", "createDocument", void 0),
      closeDocument: gapMethod("offscreen", "closeDocument", void 0)
    };
  }
  function createWindows() {
    return {
      get: gapMethod("windows", "get", {}),
      getCurrent: gapMethod("windows", "getCurrent", {}),
      getLastFocused: gapMethod("windows", "getLastFocused", {}),
      getAll: gapMethod("windows", "getAll", []),
      create: gapMethod("windows", "create", {}),
      update: gapMethod("windows", "update", {}),
      remove: gapMethod("windows", "remove", void 0),
      onCreated: new ChromeEvent(),
      onRemoved: new ChromeEvent(),
      onFocusChanged: new ChromeEvent(),
      WINDOW_ID_NONE: -1,
      WINDOW_ID_CURRENT: -2
    };
  }
  function createWebNavigation() {
    return {
      getFrame: gapMethod("webNavigation", "getFrame", void 0),
      getAllFrames: gapMethod("webNavigation", "getAllFrames", []),
      onBeforeNavigate: new ChromeEvent(),
      onCommitted: new ChromeEvent(),
      onDOMContentLoaded: new ChromeEvent(),
      onCompleted: new ChromeEvent(),
      onErrorOccurred: new ChromeEvent(),
      onHistoryStateUpdated: new ChromeEvent()
    };
  }
  function createIdle() {
    return {
      queryState: gapMethod("idle", "queryState", "active"),
      setDetectionInterval: (...args) => {
        traceStub("idle", "setDetectionInterval", ...args);
      },
      getAutoLockDelay: gapMethod("idle", "getAutoLockDelay", 0),
      onStateChanged: new ChromeEvent()
    };
  }
  function createNotifications() {
    return {
      create: gapMethod("notifications", "create", ""),
      update: gapMethod("notifications", "update", true),
      clear: gapMethod("notifications", "clear", true),
      getAll: gapMethod("notifications", "getAll", {}),
      getPermissionLevel: gapMethod("notifications", "getPermissionLevel", "granted"),
      onClicked: new ChromeEvent(),
      onClosed: new ChromeEvent(),
      onButtonClicked: new ChromeEvent(),
      onShowSettings: new ChromeEvent()
    };
  }
  function createWebRequest() {
    const ev = () => new ChromeEvent();
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
      handlerBehaviorChanged: gapMethod("webRequest", "handlerBehaviorChanged", void 0)
    };
  }
  function createIdentity() {
    return {
      // getRedirectURL is SYNCHRONOUS and returns a string — Phantom's EVM
      // service worker calls it at boot (and dies if it's absent). Mirror
      // Chrome's `https://<id>.chromiumapp.org/<path>` shape.
      getRedirectURL(path) {
        traceStub("identity", "getRedirectURL", path);
        const id = getExtensionId();
        const suffix = (path ?? "").replace(/^\//, "");
        return `https://${id}.chromiumapp.org/${suffix}`;
      },
      launchWebAuthFlow: gapMethod("identity", "launchWebAuthFlow", void 0),
      getAuthToken: gapMethod(
        "identity",
        "getAuthToken",
        { token: "" }
      ),
      removeCachedAuthToken: gapMethod("identity", "removeCachedAuthToken", void 0),
      clearAllCachedAuthTokens: gapMethod("identity", "clearAllCachedAuthTokens", void 0),
      getProfileUserInfo: gapMethod(
        "identity",
        "getProfileUserInfo",
        { email: "", id: "" }
      ),
      getAccounts: gapMethod("identity", "getAccounts", []),
      onSignInChanged: new ChromeEvent()
    };
  }
  function createPermissions() {
    return {
      contains(_perm) {
        return Promise.resolve(true);
      },
      request(_perm) {
        traceStub("permissions", "request", _perm);
        return Promise.resolve(true);
      },
      getAll() {
        return Promise.resolve({ permissions: [], origins: [] });
      },
      remove(_perm) {
        traceStub("permissions", "remove", _perm);
        return Promise.resolve(true);
      },
      onAdded: new ChromeEvent(),
      onRemoved: new ChromeEvent()
    };
  }

  // src/shared/install.ts
  function installChrome() {
    const runtime = createRuntime();
    const storage = createStorage();
    const action = createAction();
    const scripting = createScripting();
    const i18n = createI18n();
    const permissions = createPermissions();
    const management = createManagement();
    const tabs = createTabs();
    const alarms = createAlarms();
    const offscreen = createOffscreen();
    const windows = createWindows();
    const webNavigation = createWebNavigation();
    const idle = createIdle();
    const notifications = createNotifications();
    const identity = createIdentity();
    const webRequest = createWebRequest();
    const extension = {
      getURL: (path) => runtime.getURL(path),
      getBackgroundPage: () => null,
      isAllowedIncognitoAccess: () => Promise.resolve(false),
      inIncognitoContext: false
    };
    const ns = {
      runtime,
      storage,
      action,
      scripting,
      i18n,
      permissions,
      management,
      tabs,
      alarms,
      offscreen,
      windows,
      webNavigation,
      idle,
      notifications,
      identity,
      webRequest,
      extension
    };
    const g = globalThis;
    if (!g.chrome || typeof g.chrome !== "object") {
      g.chrome = ns;
    } else {
      Object.assign(g.chrome, ns);
    }
    if (!g.browser) {
      g.browser = g.chrome;
    }
    return ns;
  }

  // src/content/bootstrap.ts
  var w = globalThis;
  if (w.__tauri_ext_content_bootstrapped) {
    console.debug("[chrome-shim] content bootstrap invoked twice; skipping");
  } else {
    w.__tauri_ext_content_bootstrapped = true;
    setDefaultSurface("content");
    installChrome();
    w.__tauri_ext_configure = (opts) => {
      configure(opts);
      trace(
        "content",
        `configured ext=${opts.extensionId} frame=${opts.frameId ?? 0}`
      );
      void invokeWithRetry(RuntimeCommands.ContentReady, {
        extensionId: opts.extensionId,
        frameId: opts.frameId ?? 0
      }).catch(() => {
      });
      void attachRuntimeEvents();
    };
  }
})();
