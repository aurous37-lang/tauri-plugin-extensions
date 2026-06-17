// chrome.storage.local / chrome.storage.session shim.
//
// All IO routes through the Rust plugin, which decides persistence (local:
// JSON on disk; session: in-memory, see ARCHITECTURE.md subsystem F).
//
// The `get` argument shape Chrome accepts is ornate:
//   get()                        → returns everything
//   get(null)                    → returns everything
//   get("key")                   → returns {key: value}
//   get(["k1", "k2"])            → returns matching pairs
//   get({k1: defaultValue})      → returns pairs, using defaults for misses
//
// We normalise to `{keys: string[] | null, defaults: Record<string,unknown>}`
// on the Rust boundary so the Rust side doesn't have to parse the full
// union. The shim does the defaulting client-side.

import type { StorageArea, StorageChangeEvent } from "./types.js";
import { RuntimeCommands, RuntimeEvents } from "./types.js";
import { ChromeEvent } from "./events.js";
import { invokeWithRetry, getEventApi } from "./tauri.js";
import { getExtensionId } from "./config.js";
import { trace } from "./trace.js";

type GetArg =
  | null
  | undefined
  | string
  | string[]
  | Record<string, unknown>;

function normaliseGetArg(arg: GetArg): {
  keys: string[] | null;
  defaults: Record<string, unknown>;
} {
  if (arg == null) return { keys: null, defaults: {} };
  if (typeof arg === "string") return { keys: [arg], defaults: {} };
  if (Array.isArray(arg)) return { keys: arg.slice(), defaults: {} };
  if (typeof arg === "object") {
    const keys = Object.keys(arg);
    return { keys, defaults: { ...arg } };
  }
  return { keys: null, defaults: {} };
}

function arrayifyKeys(keys: string | string[]): string[] {
  return typeof keys === "string" ? [keys] : keys.slice();
}

// ---- per-area surface --------------------------------------------------

class StorageAreaImpl {
  readonly onChanged = new ChromeEvent<
    [changes: Record<string, { oldValue?: unknown; newValue?: unknown }>]
  >();

  constructor(private readonly area: StorageArea) {}

  get(arg?: GetArg, callback?: (items: Record<string, unknown>) => void): Promise<Record<string, unknown>> {
    const { keys, defaults } = normaliseGetArg(arg);
    const promise = invokeWithRetry<Record<string, unknown>>(
      RuntimeCommands.StorageGet,
      {
        extensionId: getExtensionId(),
        area: this.area,
        keys,
      },
    ).then((items) => {
      // Apply caller-provided defaults for missing keys.
      const out: Record<string, unknown> = { ...defaults };
      for (const [k, v] of Object.entries(items ?? {})) {
        out[k] = v;
      }
      return out;
    });
    if (callback) {
      promise.then(callback, (err) => {
        // eslint-disable-next-line no-console
        console.error("[chrome-shim] storage.get failed:", err);
        callback({});
      });
      return promise;
    }
    return promise;
  }

  set(items: Record<string, unknown>, callback?: () => void): Promise<void> {
    const promise = invokeWithRetry<void>(RuntimeCommands.StorageSet, {
      extensionId: getExtensionId(),
      area: this.area,
      items,
    });
    if (callback) {
      promise.then(
        () => callback(),
        (err) => {
          // eslint-disable-next-line no-console
          console.error("[chrome-shim] storage.set failed:", err);
          callback();
        },
      );
    }
    return promise;
  }

  remove(keys: string | string[], callback?: () => void): Promise<void> {
    const promise = invokeWithRetry<void>(RuntimeCommands.StorageRemove, {
      extensionId: getExtensionId(),
      area: this.area,
      keys: arrayifyKeys(keys),
    });
    if (callback) {
      promise.then(
        () => callback(),
        (err) => {
          // eslint-disable-next-line no-console
          console.error("[chrome-shim] storage.remove failed:", err);
          callback();
        },
      );
    }
    return promise;
  }

  clear(callback?: () => void): Promise<void> {
    const promise = invokeWithRetry<void>(RuntimeCommands.StorageClear, {
      extensionId: getExtensionId(),
      area: this.area,
    });
    if (callback) {
      promise.then(
        () => callback(),
        (err) => {
          // eslint-disable-next-line no-console
          console.error("[chrome-shim] storage.clear failed:", err);
          callback();
        },
      );
    }
    return promise;
  }

  /// Approximate byte usage — Chrome's real API queries actual stored size.
  /// The Rust side can back this with a real count later; stubbed for now.
  getBytesInUse(
    _keys?: string | string[] | null,
    callback?: (bytes: number) => void,
  ): Promise<number> {
    const p = Promise.resolve(0);
    if (callback) p.then(callback);
    return p;
  }
}

// ---- top-level chrome.storage object ----------------------------------

const localArea = new StorageAreaImpl("local");
const sessionArea = new StorageAreaImpl("session");
const crossAreaOnChanged = new ChromeEvent<
  [
    changes: Record<string, { oldValue?: unknown; newValue?: unknown }>,
    areaName: StorageArea,
  ]
>();

let changedWiringAttached = false;

async function ensureChangedWiring(): Promise<void> {
  if (changedWiringAttached) return;
  changedWiringAttached = true;
  const api = getEventApi();
  if (!api) {
    trace(
      "storage",
      "event API unavailable; onChanged will only fire for local writes",
    );
    return;
  }
  await api.listen<StorageChangeEvent>(
    RuntimeEvents.StorageChanged,
    ({ payload }) => {
      if (payload.extensionId !== getExtensionId()) return;
      const target = payload.area === "session" ? sessionArea : localArea;
      target.onChanged.emit(payload.changes);
      crossAreaOnChanged.emit(payload.changes, payload.area);
    },
  );
}

export function createStorage() {
  // Attach event wiring on first access so surfaces that never touch
  // storage don't pay for it.
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
        onChanged: new ChromeEvent<[changes: Record<string, unknown>]>(),
        getBytesInUse: () => Promise.resolve(0),
      };
    },
  };
}
