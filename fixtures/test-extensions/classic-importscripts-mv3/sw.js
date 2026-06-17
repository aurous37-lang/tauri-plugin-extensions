// Classic-importScripts MV3 fixture — classic background service worker.
//
// `importScripts` is a WorkerGlobalScope primitive absent from a Window. The
// BG host shims it (synchronous fetch over the resource origin + global eval).
// Each imported file contributes one addend; if the sum comes back 42, BOTH
// imports were fetched, resolved relative to this worker's URL, and evaluated
// in this global scope in order.
importScripts("./part-a.js", "./part-b.js");

self.__fixtureResult = {
  kind: "classic-importScripts",
  sum: (self.__partA || 0) + (self.__partB || 0),
};

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg && msg.kind === "ping") {
    sendResponse({ ok: true, kind: "pong", sum: self.__fixtureResult.sum });
    return false;
  }
  sendResponse({ ok: false, reason: "unknown kind" });
  return false;
});
