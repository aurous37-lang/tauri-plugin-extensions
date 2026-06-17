# noop-mv3

Spike fixture — the smallest MV3 extension that exercises every plugin
subsystem:

- Content script injects at `document_end` in the isolated world.
- `chrome.runtime.sendMessage` from content reaches the background service
  worker (`background.js`).
- Background reads + writes `chrome.storage.local` (the `noop_mv3_ping_count`
  key).
- Background's `sendResponse` returns the pong to the content script.

Load via the plugin's `load_unpacked` command, pointing at this directory.

## icon.png

Missing intentionally — the fixture's manifest references one, but the
spike acceptance does not require the plugin to honor `icons` yet. Drop any
16×16 PNG here if you want to exercise the (future) icon resolver.
