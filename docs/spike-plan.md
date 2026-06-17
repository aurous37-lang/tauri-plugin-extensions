# Spike plan — 2026-04-20

The 2-week spike from `engineering-summary.md`, decomposed into the units of
work this session's parallel agents will produce. Acceptance at the end of
the spike is a no-op MV3 extension loading, content-script injection,
round-trip message to background, and `chrome.storage.local` persistence.
**Phantom is the v1 target**, but the spike itself uses a hand-rolled no-op
extension because it exercises every subsystem with no license / ToS friction.

## Workstreams (parallel)

All six workstreams can run concurrently once the root scaffold lands.

### A. Manifest parser (`src/manifest/`)

Types the subset of MV3 we honor in v1. Tests against Phantom's real
`manifest.json`, MetaMask's, Rabby's, plus `fixtures/test-extensions/noop-mv3/`.

**Exit criteria:** all three real manifests parse without error; MV2 is
rejected with a typed error; unknown fields do not crash the parser
(forward-compat).

### B. URL / glob matchers (`src/matcher/`)

Chrome-faithful match-pattern and glob implementations with a large unit-test
suite.

**Exit criteria:** matches the Chrome reference test vectors
([Chrome's extensions test corpus](https://source.chromium.org/chromium/chromium/src/+/main:extensions/common/url_pattern_unittest.cc))
cherry-picked into our tests; handles `<all_urls>`, scheme wildcards,
subdomain wildcards, port edge cases.

### C. Plugin skeleton + loader + IPC bus (`src/lib.rs`, `src/loader.rs`, `src/ipc/`, `src/registry.rs`)

Plugin wiring, Tauri commands (`load_unpacked`, `unload`, `list_extensions`,
`send_message`), registry, and in-process IPC bus.

**Exit criteria:** `cargo check --workspace` clean; `load_unpacked(path)`
returns an `ExtensionId`; `list_extensions()` returns the loaded set;
`send_message` delivers to a registered port.

### D. Background runner (`src/runtime/background.rs` + js-runtime BG)

Hidden `WebviewWindow` per loaded extension. Evaluates
`background.service_worker` against the BG-flavored `chrome.*` shim.

**Exit criteria:** an extension's `background.js` runs; its
`chrome.runtime.onInstalled` fires; it can call `chrome.storage.local.set`
and the value is persisted.

### E. chrome.* shim (js-runtime/)

TypeScript. `chrome.runtime`, `chrome.storage.local`, `chrome.storage.session`,
`chrome.action` (stubs), `chrome.scripting.executeScript` (stub). Built with
esbuild; output is embedded into the Rust crate via `include_str!`.

**Exit criteria:** `pnpm build` emits `embedded-js/content-bootstrap.js` and
`embedded-js/background-bootstrap.js`; content-script flavor successfully
posts a message that reaches the BG flavor.

### F. Fixtures + integration harness (`fixtures/test-extensions/noop-mv3/`, `examples/minimal-host/`, `tests/integration/`)

Minimal valid MV3 extension plus a minimal Tauri host app that loads the
plugin and the extension and drives the acceptance test. Script to fetch
Phantom's unpacked build (for future v1 work, not committed).

**Exit criteria:** `pnpm tauri dev` inside `examples/minimal-host/` launches
an app that has loaded `noop-mv3`; navigating to a configured test URL
triggers the round-trip assertion.

## Integration + acceptance

After the six workstreams land, a single reconciliation pass:

1. `cargo check --workspace` clean.
2. `cargo test --workspace` clean.
3. `cd js-runtime && pnpm install && pnpm build` clean.
4. `cd examples/minimal-host && pnpm tauri dev` launches and loads the
   noop extension end-to-end.
5. Evidence captured in `docs/spike-acceptance-YYYY-MM-DD.md`.

## Kill-switch

Per the engineering summary, if none of the three background-runner approaches
are viable on WebView2 without breaking its security model, reassess before
committing to v1. The hidden-webview approach (D-001) is the pre-committed
path; the check at the end of this spike is: does evaluating arbitrary JS
inside a hidden `WebviewWindow` successfully run code equivalent to a
service-worker host? If `navigator.serviceWorker` behaviors materially
differ from what extension code expects, we reopen D-001 before v1.
