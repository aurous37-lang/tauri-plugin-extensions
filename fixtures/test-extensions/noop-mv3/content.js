// Noop MV3 fixture — content script.
//
// Runs at document_end in the isolated world. Sends a single ping to the
// background service worker; logs whatever comes back.

(async () => {
  try {
    const response = await chrome.runtime.sendMessage({
      kind: "ping",
      ts: Date.now(),
      href: location.href,
    });
    console.log("[noop-mv3] content got pong:", response);
    window.__NOOP_MV3_PONG__ = response;
  } catch (err) {
    console.error("[noop-mv3] content sendMessage failed:", err);
    window.__NOOP_MV3_ERROR__ = String(err);
  }
})();
