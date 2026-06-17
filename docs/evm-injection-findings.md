# EVM provider injection — diagnosis, fixes, and per-wallet results

Goal: get `window.ethereum` (EIP-1193 / EIP-6963) injecting for MetaMask, Rabby,
and Phantom's EVM side in the `tauri-plugin-extensions` MV3 runtime. The owner's
target is EVM (Monad, chain 143), so EVM provider injection is the v1 bar.

All wallet code is chain-agnostic and wallet-agnostic — there are **no
wallet-name conditionals** anywhere in the crate or shim. Each wallet is just an
MV3 extension; where one needed special handling, that surfaced as a missing
generic `chrome.*` API, which is what got implemented.

## How each wallet puts a provider on the page (the three mechanisms)

| Wallet   | EVM provider mechanism | Depends on |
|----------|------------------------|-----------|
| **MetaMask** | `scripts/inpage.js` declared as a static `world:"MAIN"` content script; the modern multichain provider then establishes a transport to the content script before it is usable | content-script injection (have) **+ a live content↔BG relay** |
| **Rabby** | content script injects `<script src=chrome.runtime.getURL("pageProvider.js")>` (a `web_accessible_resource`) | **serving `web_accessible_resources`** |
| **Phantom (EVM)** | background service worker calls `chrome.scripting.registerContentScripts([{ js:["evmAsk.js"], world:"MAIN", runAt:"document_start" }])`; the EVM inpage script then handshakes with the ISOLATED `contentScript.js` over `phantom#provider_injection_options` | **`chrome.scripting.registerContentScripts` + a working module service worker + an `https`/`localhost` origin** |

Phantom's Solana provider, by contrast, is a *static* `world:"MAIN"` content
script (`solana.js`) and already worked — which is exactly why Solana injected
but EVM did not.

## Diagnosis — instrumented, per wallet

Method: loaded each wallet in isolation in `examples/minimal-host`, captured the
background worker's error buffer (`window.__extLastErrors`) and a new
always-on `chrome.*`-gap buffer (`window.__extApiGaps`, fed by `recordGap` in
`js-runtime/src/shared/trace.ts`), and probed the page for provider globals +
EIP-6963 via `fixtures/test-dapp/index.html`. The page probe is read back
authoritatively from Rust via a `location.hash` + base64 channel
(`minimal_host_introspect_dapp`) because MetaMask's SES/LavaMoat `lockdown()`
breaks the page's own Tauri IPC.

### Per-wallet gap table

| Wallet | Provider injected before? | Root cause | Missing/blocking `chrome.*` (observed) |
|--------|---------------------------|-----------|-----------------------------------------|
| **Phantom EVM** | No (`ethereum:false`, `phantomEthereum:false`) | BG **never runs**: `background.type:"module"` SW begins with top-level `import "../chunk-*.js"`. The bootstrap is one concatenated *classic* script, so the `import` is a parse-time `SyntaxError` that kills the *entire* BG bootstrap (error-tap, polyfill, shim, bridge — all of it). BG round-trip times out at 15s; `__extLastErrors` **and** `__extApiGaps` are both empty (nothing ran). So Phantom never reaches its `chrome.scripting.registerContentScripts` call. | (BG dead before any `chrome.*`) → then `chrome.scripting.registerContentScripts` (was a silent no-op stub) |
| **Rabby** | No (`ethereum:false`, `rabbyGlobal:false`) | content script's `<script src=getURL("pageProvider.js")>` resolved to `tauri://extension-resource/...` which **nothing served** → 404 → provider never loaded. | `web_accessible_resources` not served; BG also dies on `chrome.alarms.getAll` and `chrome.tabs`/`webNavigation` `onActivated`, and uses top-level `importScripts(...)` |
| **MetaMask** | No (`ethereum:false`, `isMetaMask:false`, `eip6963:0`) | provider object never completes init — its modern multichain inpage needs the content↔BG transport, and the BG dies almost immediately on `chrome.management.getSelf`. The real BG logic lives in `importScripts`-loaded chunks (`service-worker.js` only references `chrome.runtime.id` directly). | `chrome.management.getSelf`, then `chrome.tabs`, plus top-level `importScripts(...)` for all real logic |

## What was implemented (TDD, generic, no wallet conditionals)

1. **`chrome.scripting.registerContentScripts` / `unregisterContentScripts` /
   `getRegisteredContentScripts`** — real, end-to-end. New
   `src/runtime/dynamic_scripts.rs` `DynamicScriptStore` (per-extension dynamic
   registrations, dup-id rejection, traversal-free file reads). Merged into the
   on_page_load injection flow (`src/runtime/injection.rs`). Three new plugin
   commands (`src/lib.rs`, `build.rs`, `permissions/default.toml`). Shim wired in
   `js-runtime/src/shared/stubs.ts`. Tests: `tests/dynamic_scripts.rs` (6).

2. **`web_accessible_resources` serving** via a custom `extres://` URI scheme
   (Windows: `http://extres.localhost/<id>/<path>`). New
   `src/runtime/resources.rs` (WAR allowlist via the Glob matcher, lexical
   traversal guard, MIME mapping) + the scheme handler in `src/lib.rs`
   (canonicalize + `starts_with(root)` defense-in-depth). `chrome.runtime.getURL`
   now returns the served origin, threaded through `configure()`’s new
   `resourceBase`. Tests: `resources` unit module (7). The embedding app must
   allow the scheme origin in its CSP (the minimal host adds
   `http://extres.localhost` to `script-src`).

3. **Background-worker survival stubs** for the namespaces wallet BG workers
   touch during init — `chrome.{management, tabs, alarms, offscreen, windows,
   webNavigation, idle, notifications}` (`js-runtime/src/shared/stubs.ts` +
   `install.ts`). Before, the first access threw and aborted the whole worker
   (the BG source runs in one try/catch IIFE). These are benign recording
   no-ops (logged to `__extApiGaps`); they keep the worker alive past those
   calls. They do **not** by themselves make MetaMask/Rabby fully functional —
   those BGs still need `importScripts` (see remaining work).

4. **Instrumentation + canary**: the always-on `__extApiGaps` gap recorder; a
   multi-wallet canary probing `ethereum`/`isMetaMask`/`isRabby`/
   `phantom.ethereum` + EIP-6963 announce counting + a live `eth_chainId`
   round-trip attempt; per-wallet BG introspection; the EVM matrix in the
   minimal host (`examples/minimal-host/dist/main.js`). Plus
   `docs/wallet-fixtures.md` (unpacking steps for all three wallets on Windows).

5. **Two correctness fixes from adversarial review**: (a) the lifecycle
   `internal_stop` now drops an extension's dynamic registrations so a reloaded
   BG re-registers cleanly instead of hitting Chrome's duplicate-id error
   (regression-locked in `tests/dynamic_scripts.rs`); (b)
   `registerContentScripts` resolves `source_dir` from the lifecycle manager as
   a fallback, closing a boot-time race where the registry projection hadn't yet
   caught up.

## Canary results — per wallet (authoritative `GLOBALS FOUND` JSON)

> From `examples/minimal-host/acceptance-report.json` → `evm.<wallet>.probe`,
> read back via the Rust `location.hash` channel (`dappIntrospect:true` = the
> reading itself succeeded). One wallet loaded at a time for an unambiguous
> `window.ethereum`.

**Rabby — provider injects ✅ (`ethereum:true`, `isRabby:true`, EIP-6963 announces):**
```json
{
  "ethereum": true,
  "ethereumIsRabby": true,
  "ethereumIsMetaMask": true,        // Rabby's deliberate MetaMask-compat masquerade
  "rabbyGlobal": true,
  "eip6963Count": 1,
  "eip6963Rdns": ["io.rabby"],
  "eip6963Providers": [{ "rdns": "io.rabby", "name": "Rabby Wallet", "hasProvider": true }],
  "chainId": { "ok": false, "error": "eth_chainId timed out after 4s" },
  "phantomPresent": false, "solana": false,
  "dappIntrospect": true
}
```

**MetaMask — provider does NOT inject ❌:**
```json
{
  "ethereum": false,
  "ethereumIsMetaMask": false,
  "eip6963Count": 0,
  "chainId": null,
  "dappIntrospect": true
}
```
BG errors: `chrome.management.getSelf` undefined, then a fatal throw. Real BG
logic is `importScripts`-loaded, which the runtime does not provide.

**Phantom — Solana injects, EVM does NOT ❌:**
```json
{
  "phantomPresent": true, "phantomSolana": true, "solanaIsPhantom": true,
  "ethereum": false, "phantomEthereum": false,
  "eip6963Count": 0,
  "dappIntrospect": true
}
```
BG `__extLastErrors` and `__extApiGaps` both empty → the module-`import` SW
bootstrap never executed.

## Test counts — before / after

- Before: **135** (66 lib unit + 69 integration).
- After: **148** — 73 lib unit (+7 `runtime::resources`), 75 integration
  (+6 `dynamic_scripts` registration/unregister/reload + the existing 69).
- `cargo clippy -p tauri-plugin-extensions --all-targets -- -D warnings`: clean.
- Minimal-host acceptance: 20/20 original steps still green (noop + Phantom
  Solana + reload regression + diagnostics), plus the EVM matrix.

## Still not working — ranked by impact on EVM-wallet readiness

1. **Module service workers (`background.type:"module"`) — blocks Phantom EVM
   entirely.** Phantom's whole BG is dead because the classic-script bootstrap
   can't parse top-level `import`. Fix needs loading the SW as a real module
   (e.g. host the BG document on the `extres://` scheme and reference the SW
   via `<script type="module">` so relative `../chunk-*.js` imports resolve
   against a served origin), while keeping `__TAURI_INTERNALS__` available.
   Once the BG runs, the new `registerContentScripts` support + an
   `https`/`localhost`-origin canary (Phantom gates its provider-options
   handshake on `location.protocol==='https:' || hostname∈{localhost,127.0.0.1}`)
   should let `window.phantom.ethereum` inject.

2. **`importScripts()` + deep BG support — blocks MetaMask and Rabby
   `eth_chainId` (and MetaMask's provider).** Both wallets' real BG logic is
   loaded via top-level `importScripts(...)`, absent on the window context.
   Needs an `importScripts` shim that fetches (via the `extres://` scheme) and
   evaluates the imported scripts synchronously, plus enough of
   `chrome.tabs`/`alarms`/`offscreen` and `IndexedDB`/`crypto.subtle` for their
   controllers to initialize. The survival stubs added here are the first layer;
   real implementations are the next. MetaMask additionally needs its
   content↔BG transport working before `window.ethereum` appears at all.

3. **BG → page push has no path (`chainChanged` / `accountsChanged`).** Wallets
   broadcast events to the page via `chrome.tabs.sendMessage` or BG-initiated
   ports; v1 has no tab abstraction, so these are dropped. Dapps that subscribe
   to provider events won't see updates even once a provider is present.

4. **Single extension identity per page (documented limitation, widened by the
   dynamic-script merge).** `configure()` pins the frame to the first matching
   extension's id; a second extension's ISOLATED content script then routes
   under the wrong id. Irrelevant for v1 (one wallet per page) but blocks
   multi-wallet pages. Interim hardening: prefer the first ISOLATED request when
   choosing the configure identity. Real fix: per-extension worlds.

5. **`web_accessible_resources` `matches` origin allowlist is unenforced.** Any
   page that can reach the `extres://` scheme can fetch any WAR resource of any
   loaded extension (responses also carry `Access-Control-Allow-Origin: *`).
   Acceptable for a controlled single-dapp host; for untrusted pages, thread the
   request origin through `resolve_resource` and enforce per-entry `matches`.

6. **Resource scheme reads files synchronously on the webview UI thread.** Fine
   for small JS providers (and matches Tauri's own `asset` protocol), but a
   multi-MB `.wasm` WAR asset would jank the event loop. Switch to
   `register_asynchronous_uri_scheme_protocol` + `spawn_blocking` if large
   assets become common.

7. **Page CSP blocks the resource-script tag on real dapps.** Chrome exempts
   `chrome-extension:` resources from page CSP; we cannot replicate that without
   a header-rewrite layer. The embedding app must allow the `extres` origin in
   its CSP (the minimal host does). Strict-CSP dapps will still block Rabby-style
   `<script src=getURL(...)>` injection.
