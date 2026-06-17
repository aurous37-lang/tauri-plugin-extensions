// Content-world bootstrap.
//
// Script injection order on Agent C's loader:
//   1. `content-bootstrap.js` (this file's build output) at document_start.
//   2. `globalThis.__tauri_ext_configure({extensionId, manifest, frameId, ...})`
//      is called by the Rust side immediately after eval.
//   3. The extension's own content_scripts[].js files are injected after.
//
// The shim therefore needs to set up `window.chrome` synchronously at eval
// time, and accept the configure callback to finish populating id/manifest.
// Extensions that read `chrome.runtime.id` before configure() arrives see
// the placeholder "__EXT_ID__" — documented gap; in practice Phantom
// defers id reads until first use.

import { configure, setDefaultSurface } from "../shared/config.js";
import { installChrome } from "../shared/install.js";
import { attachRuntimeEvents } from "../shared/runtime.js";
import type { ConfigureOptions } from "../shared/types.js";
import { RuntimeCommands } from "../shared/types.js";
import { invokeWithRetry } from "../shared/tauri.js";
import { trace } from "../shared/trace.js";

declare global {
  interface Window {
    __tauri_ext_configure?: (opts: ConfigureOptions & { trace?: boolean }) => void;
    __tauri_ext_content_bootstrapped?: boolean;
  }
}

// Guard against accidental double-injection (e.g. frame reloads racing with
// a webview navigation the Rust side didn't see coming). Second eval is a
// no-op that preserves the first config.
const w = globalThis as unknown as Window;
if (w.__tauri_ext_content_bootstrapped) {
  // eslint-disable-next-line no-console
  console.debug("[chrome-shim] content bootstrap invoked twice; skipping");
} else {
  w.__tauri_ext_content_bootstrapped = true;
  setDefaultSurface("content");
  installChrome();

  // Expose the configure hook synchronously so Rust can call it at eval time.
  w.__tauri_ext_configure = (opts) => {
    configure(opts);
    trace(
      "content",
      `configured ext=${opts.extensionId} frame=${opts.frameId ?? 0}`,
    );
    // Tell the Rust side this content surface is ready — the loader uses
    // this to decide when to inject the extension's content_scripts[].
    void invokeWithRetry(RuntimeCommands.ContentReady, {
      extensionId: opts.extensionId,
      frameId: opts.frameId ?? 0,
    }).catch(() => {
      /* loader may fire this without caring; non-fatal */
    });
    // Start listening for inbound messages/ports.
    void attachRuntimeEvents();
  };
}
