// Thin wrapper around Tauri v2's internal invoke + event bus.
//
// Why a wrapper? Two reasons:
//   1. Tauri v2 moved the invoke function from `window.__TAURI__.invoke` to
//      `window.__TAURI_INTERNALS__.invoke`. The older symbol was kept alive
//      as a deprecated alias for a while but is gone on recent v2 builds; we
//      target current v2 and only fall back if both are absent.
//   2. The hidden background webview may start evaluating scripts a tick
//      before the plugin has finished mounting its invoke handler. A thin
//      retry (once) keeps the BG bootstrap race-free without hiding real
//      programming errors behind infinite retry loops.
//
// The event API is only used from the background surface today, but we
// expose it from both so popups (future) pick it up for free.

import type { InvokeFn, TauriEventApi } from "./types.js";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke?: InvokeFn;
    };
    // Kept as a fallback for older v2 builds.
    __TAURI__?: {
      invoke?: InvokeFn;
      event?: TauriEventApi;
    };
    __TAURI_EVENT__?: TauriEventApi;
  }
}

/// Fetches the Tauri invoke function, preferring the v2 internal path.
/// Throws if neither symbol exists (which indicates the script ran outside
/// a Tauri webview — a bug at integration time, not a runtime condition).
export function getInvoke(): InvokeFn {
  const g = globalThis as unknown as Window;
  const fn =
    g.__TAURI_INTERNALS__?.invoke ??
    g.__TAURI__?.invoke ??
    undefined;
  if (typeof fn !== "function") {
    throw new Error(
      "tauri-plugin-extensions: Tauri invoke bridge unavailable; script injected outside a Tauri webview?",
    );
  }
  return fn;
}

/// Returns the Tauri event listen() function, or `null` if it's not wired
/// up in this surface. The event API is optional: the content world does
/// not need it (invokes drive everything), but the background does.
export function getEventApi(): TauriEventApi | null {
  const g = globalThis as unknown as Window;
  if (g.__TAURI_EVENT__?.listen) return g.__TAURI_EVENT__;
  if (g.__TAURI__?.event?.listen) return g.__TAURI__.event;
  return null;
}

/// Invoke with a single retry-after-microtask. The background webview starts
/// evaluating scripts before the Rust plugin's state is necessarily mounted;
/// on the first retry we give the event loop a chance to catch up. After
/// that, errors propagate so real bugs surface normally.
export async function invokeWithRetry<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const invoke = getInvoke();
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    // One retry after a tick.
    await new Promise<void>((resolve) => queueMicrotask(resolve));
    try {
      return await invoke<T>(cmd, args);
    } catch {
      throw err;
    }
  }
}
