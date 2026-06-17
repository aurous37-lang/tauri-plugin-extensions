// Per-surface configuration populated by the Rust host at script injection
// time. The bootstrap exposes `globalThis.__tauri_ext_configure(opts)` for
// the Rust side to call synchronously after eval; the shim reads from the
// config store after that point.
//
// Two policies here:
//
//   1. We intentionally do NOT block chrome.* calls waiting for configure.
//      Some extensions (Phantom included) touch `chrome.runtime.id` during
//      the synchronous import of their content script. If configure hasn't
//      landed yet, we return a placeholder ("__EXT_ID__") so the extension
//      can read it; it almost always just stashes it for later use. The
//      Rust side is responsible for always calling configure immediately
//      after eval, and the placeholder is a belt-and-braces guard.
//
//   2. configure() is idempotent per-surface: calling it a second time
//      with a different extensionId is a bug (one bootstrap per extension
//      per world). We log and keep the first value rather than crash.

import type { ConfigureOptions, Surface } from "./types.js";
import { setTrace } from "./trace.js";

interface InternalConfig {
  extensionId: string;
  manifest: Record<string, unknown>;
  surface: Surface;
  frameId: number;
  resourceBase: string;
  ready: boolean;
}

const state: InternalConfig = {
  extensionId: "__EXT_ID__",
  manifest: {},
  surface: "content",
  frameId: 0,
  // Dead default until configure() supplies the real, platform-specific
  // resource origin. A getURL() before configure points at nothing — same
  // contract as the extensionId placeholder above.
  resourceBase: "tauri://extension-resource",
  ready: false,
};

const readyResolvers: Array<() => void> = [];

export function configure(opts: ConfigureOptions & { trace?: boolean }): void {
  if (state.ready) {
    if (state.extensionId !== opts.extensionId) {
      // eslint-disable-next-line no-console
      console.warn(
        "[chrome-shim] configure() called twice with different extensionId; ignoring second call",
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

/// Force-set the surface before configure() arrives. Used by the background
/// bootstrap, which knows unconditionally that it is the BG surface.
export function setDefaultSurface(surface: Surface): void {
  if (!state.ready) state.surface = surface;
}

export function getExtensionId(): string {
  return state.extensionId;
}

export function getManifest(): Record<string, unknown> {
  return state.manifest;
}

export function getSurface(): Surface {
  return state.surface;
}

export function getFrameId(): number {
  return state.frameId;
}

export function getResourceBase(): string {
  return state.resourceBase;
}

export function whenReady(): Promise<void> {
  if (state.ready) return Promise.resolve();
  return new Promise((resolve) => {
    readyResolvers.push(resolve);
  });
}
