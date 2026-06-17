// Module-Import MV3 fixture — ES-module background service worker.
//
// The top-level `import` below is a *parse-time* construct: a classic-script
// host (the pre-2026-06-10 bootstrap) throws SyntaxError before any line runs.
// If `__fixtureResult.brand` comes back set, the host parsed and executed this
// as a real module AND resolved the relative `./chunk-brand.js` specifier
// against the extension root on the resource origin.
import { BRAND, add } from "./chunk-brand.js";

globalThis.__fixtureResult = {
  kind: "module-import",
  brand: BRAND,
  sum: add(2, 40),
};

// Also answer a runtime ping so the BG message round-trip can assert liveness
// the same way the noop fixture does.
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && msg.kind === "ping") {
    sendResponse({ ok: true, kind: "pong", brand: BRAND, sum: add(2, 40) });
    return false;
  }
  sendResponse({ ok: false, reason: "unknown kind" });
  return false;
});
