// Background-world bootstrap.
//
// Runs inside the hidden per-extension WebviewWindow (D-001). The Rust
// loader evaluates this file first, then calls `__tauri_ext_configure`
// with the extension id + manifest, then concatenates the extension's
// `background.service_worker` source and evaluates that. The extension
// code runs against the `globalThis.chrome` we install here.
//
// Two background-specific responsibilities on top of the shared surface:
//
//   1. Fire `chrome.runtime.onInstalled` synthetically after a microtask
//      so listeners registered during the extension's top-level evaluation
//      still see the event. Chrome fires this natively on genuine first
//      install; for the spike, every boot is treated as install (the real
//      semantics need a persisted "ever-installed" flag Agent C can add
//      later — see D-005 Phantom onboarding).
//
//   2. Eagerly attach Tauri event listeners so inbound messages from
//      content scripts get routed even before the extension's background
//      module adds its onMessage listener.
//
// The surface is otherwise identical to the content surface.

import { configure, setDefaultSurface } from "../shared/config.js";
import { installChrome } from "../shared/install.js";
import {
  attachRuntimeEvents,
  fireOnInstalled,
  fireOnStartup,
} from "../shared/runtime.js";
import type { ConfigureOptions } from "../shared/types.js";
import { trace } from "../shared/trace.js";

declare global {
  interface Window {
    __tauri_ext_configure?: (opts: ConfigureOptions & { trace?: boolean }) => void;
    __tauri_ext_background_bootstrapped?: boolean;
  }
}

const w = globalThis as unknown as Window;

if (w.__tauri_ext_background_bootstrapped) {
  // eslint-disable-next-line no-console
  console.debug("[chrome-shim] background bootstrap invoked twice; skipping");
} else {
  w.__tauri_ext_background_bootstrapped = true;
  setDefaultSurface("background");
  installChrome();

  w.__tauri_ext_configure = (opts) => {
    // Force-surface to background regardless of caller input; the BG
    // webview is always BG.
    configure({ ...opts, surface: "background" });
    trace("background", `configured ext=${opts.extensionId}`);

    // Attach the event bus first so the extension's top-level listeners
    // (registered inside its background module, which runs after this
    // file) receive early traffic buffered by the Rust side.
    void attachRuntimeEvents().then(() => {
      // Defer the synthetic onInstalled + onStartup by one microtask so
      // the extension's background module (concatenated after us) has a
      // chance to register listeners.
      queueMicrotask(() => {
        try {
          // First-boot reason is "install" for the spike. A future
          // improvement: read a persisted flag set by the Rust side.
          fireOnInstalled("install");
          fireOnStartup();
          trace("background", "fired synthetic onInstalled + onStartup");
        } catch (err) {
          // eslint-disable-next-line no-console
          console.error("[chrome-shim] synthetic onInstalled failed:", err);
        }
      });
    });
  };
}
