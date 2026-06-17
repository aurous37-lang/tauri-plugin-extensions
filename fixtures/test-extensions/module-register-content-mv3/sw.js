// Module-Register-Content MV3 fixture — ES-module background service worker
// that registers a MAIN-world content script at runtime.
//
// This is Phantom's EVM injection shape: the module SW imports its chunks,
// then calls chrome.scripting.registerContentScripts([{ world: "MAIN", ... }])
// to place an inpage provider on every page. If `__fixtureResult.registered`
// is true in the BG AND `window.__fixtureMainWorldInjected` is true on a dapp
// page, module execution + post-import dynamic registration + MAIN-world
// injection all composed.
import { TARGET, RULE_ID } from "./chunk-config.js";

globalThis.__fixtureResult = {
  kind: "module-register-content",
  target: TARGET,
  registered: false,
};

chrome.scripting
  .registerContentScripts([
    {
      id: RULE_ID,
      matches: ["http://*/*", "https://*/*"],
      js: ["inpage.js"],
      runAt: "document_start",
      world: "MAIN",
      allFrames: false,
    },
  ])
  .then(() => {
    globalThis.__fixtureResult.registered = true;
  })
  .catch((e) => {
    globalThis.__fixtureResult.registerError = (e && e.message) || String(e);
  });
