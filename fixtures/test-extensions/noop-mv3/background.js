// Noop MV3 fixture — background service worker.
//
// Answers pings, increments a persisted counter in chrome.storage.local so
// the spike acceptance can verify storage survives reload.

const STORAGE_KEY = "noop_mv3_ping_count";

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.kind !== "ping") {
    sendResponse({ ok: false, reason: "unknown kind" });
    return false;
  }

  chrome.storage.local.get([STORAGE_KEY]).then(({ [STORAGE_KEY]: prev }) => {
    const next = (prev ?? 0) + 1;
    chrome.storage.local.set({ [STORAGE_KEY]: next }).then(() => {
      sendResponse({
        ok: true,
        kind: "pong",
        count: next,
        echo: msg,
        ts: Date.now(),
      });
    });
  });

  // Keep the message channel open for the async sendResponse.
  return true;
});
