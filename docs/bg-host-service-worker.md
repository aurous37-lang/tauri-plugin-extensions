# Faithful background service-worker host (D-008) — design + findings

Phase 2 of the EVM work. Phase 1 (`docs/evm-injection-findings.md`) closed the
resource-scheme and dynamic-content-script gaps and got Rabby's provider
injecting, but found all three wallets blocked at the **same lower layer**: the
hidden background webview was not a faithful service-worker environment. This
phase makes it one.

## The gap phase 1 identified

The background webview concatenated the extension's `background.service_worker`
source into a classic-script `initialization_script`, wrapped in an IIFE. That
fails the two MV3 service-worker loading mechanisms real wallets use:

1. **Module workers** (`background.type: "module"`). Phantom's worker begins with
   a top-level `import "../chunk-*.js"`. A top-level `import` is a *parse-time*
   `SyntaxError` in a classic script, so the **entire** bootstrap (error-tap,
   shim, bridge, and all of Phantom's real logic) never ran — `__extLastErrors`
   was empty because nothing executed at all.
2. **`importScripts()`**. MetaMask and Rabby pull their real background logic in
   via top-level `importScripts(...)`, which does not exist on a `Window`. Their
   workers were shells.

## Design (D-008)

The background webview document **stays on the app origin** (so
`__TAURI_INTERNALS__` / `chrome.*` remain available to the worker). What changed
is how the worker's code is loaded: instead of inlining the source, the
bootstrap loads the worker **from the `extres://` resource origin** after the
shim + bridge are in place.

```
ext-bg-<id> webview (app origin: http://tauri.localhost)
  initialization_script (classic, document_start):
    1. error-tap
    2. event-dispatch polyfill
    3. chrome.* shim bundle
    4. __extRuntime bridge
    5. __tauri_ext_configure({ surface: 'background', resourceBase })
    6. SW LOADER  ← new (src/runtime/resources.rs::sw_loader_script)
         module : import("http://extres.localhost/<id>/<sw>")
         classic: importScripts shim, then importScripts(<sw url>)
```

Key insight that dissolves the phase-1 "must be app origin vs. must resolve
relative imports" conflict: **a script's fetch URL — not the document's origin —
is what determines module/`importScripts` resolution.** A
`<script type=module src=extres://…>` (here, a dynamic `import(...)`) runs in the
app-origin realm (sees `window.chrome`, `__TAURI_INTERNALS__`) while resolving
its relative `import`s against the extres origin. So the worker stays privileged
*and* its `../chunk.js` imports resolve against the extension root.

### Serving the worker's private files (privileged path)

The SW and its chunks are **not** `web_accessible_resources` — Chrome would not
serve them to a page, but the service worker's own origin can `fetch` them. The
`extres://` handler mirrors this with two privilege levels keyed off
`ctx.webview_label()` (`src/runtime/resources.rs::serve_mode`):

- requesting webview == the extension's own `ext-bg-<id>` window →
  **privileged**: any packaged file (traversal-guarded only).
- any other webview (dapp page, host frontend) → **WAR-gated** (unchanged).

A BG window requesting a *different* extension's id falls back to WAR-gated, so
one extension's background cannot read another's private files.

### Boot-race fix

The BG fetches its SW from `extres://` *during boot*, earlier than the async
`ExtensionRegistry` projection (`reproject`) lands at the end of a load
transition. A URI-scheme handler is synchronous and cannot await the manager.
Fix: a synchronously-maintained `ResourceRegistry` (`DashMap`) that the
`LifecycleManager` upserts **before** it spawns the BG, so the handler always
resolves a just-spawned worker's root. Falls back to `ExtensionRegistry`.

### `importScripts` and CSP

`importScripts` must be synchronous; a `Window` has no native equivalent, so the
shim does a synchronous `XMLHttpRequest` over the resource origin + global-scope
`eval`, resolving relative specifiers against the worker directory (the faithful
"resolve relative to the worker URL" rule). `eval` means **classic-`importScripts`
workers require the host CSP to allow `'unsafe-eval'`**. Module workers use a
native dynamic `import` and need **no** eval — they work under a strict,
eval-free CSP. See the D-008 CSP consequence note.

## (a) What changed

- `src/runtime/resources.rs` — `serve_mode`, `resolve_resource_privileged`,
  `sw_load_plan`, `sw_loader_script`, and the `ResourceRegistry`.
- `src/runtime/webview2.rs` — `compose_bootstrap` reads `background.{service_worker,type}`
  from the manifest and appends the loader instead of inlining the source;
  `Backend::spawn_background` drops the now-unused source argument.
- `src/runtime/mod.rs`, `wkwebview.rs`, `webkitgtk.rs` — trait + stub signatures.
- `src/lifecycle/manager.rs` — `upsert_resource_root` before each spawn
  (install/reload/enable), remove on uninstall.
- `src/lib.rs` — the `extres://` handler gains the privileged/WAR decision and
  resolves through `ResourceRegistry`; manages `ResourceRegistry`.
- `DECISIONS.md` — D-008 amendment.

## (b) Fixture results — module SW + importScripts semantics

Three committed fixtures under `fixtures/test-extensions/`, each gating the
minimal-host acceptance report (`bghost_*` steps), all **green**:

| Fixture | Proves | Result |
|---|---|---|
| `module-import-mv3` | module SW with top-level `import` of a sibling chunk | `{ kind:"module-import", brand:"module-import-ok", sum:42 }` — module parsed + executed, relative import resolved |
| `classic-importscripts-mv3` | classic SW `importScripts`-ing two files | `{ kind:"classic-importScripts", sum:42 }` — both files fetched, resolved relative to the worker, evaluated in order |
| `module-register-content-mv3` | module SW that, post-import, `registerContentScripts({world:"MAIN"})` (Phantom's shape) | BG `{ registered:true }` **and** dapp page `{ mainWorldFixture:true }` |

Pure logic locked by `tests/background_host.rs` (5) + `runtime::resources` units.

## (c) Per-wallet canary — before (phase 1) / after (this phase)

One wallet loaded at a time; `GLOBALS FOUND` read authoritatively from Rust via
the `location.hash` channel. The decisive column is the BG error/gap buffer,
which shows how far the worker now executes.

**Phantom EVM** — module-SW blocker resolved; now blocked deeper.
```
before: ethereum:false  BG: __extLastErrors EMPTY, __extApiGaps EMPTY (parse SyntaxError — nothing ran)
after : ethereum:false  BG ran into real logic; error:
        "module SW failed: …/background/serviceWorker.js
         Cannot read properties of undefined (reading 'getRedirectURL')"
        (Solana still injects: phantomPresent:true, solana:true)
```
The `type:"module"` worker now executes; it dies on a missing
`chrome.identity.getRedirectURL`, not on loading.

**MetaMask** — importScripts blocker resolved; now blocked deeper.
```
before: ethereum:false  BG died at chrome.management.getSelf (undefined → throw)
after : ethereum:false  BG: apiGaps:["management.getSelf"] (survived); classic SW now
        loads its real logic via importScripts and throws later:
        "classic SW threw: Cannot read properties of undefined (reading 'state')"
```
`importScripts` works; MetaMask's real logic runs and hits a further missing
API/state surface.

**Rabby** — unchanged provider success; `eth_chainId` blocked downstream.
```
before/after: ethereum:true, isRabby:true, eip6963Count:1, rdns:["io.rabby"]
              chainId: { ok:false, error:"eth_chainId timed out after 6s" }
              BG: apiGaps:["alarms.getAll","offscreen.hasDocument","offscreen.createDocument"]
```
Rabby reaches `chrome.offscreen.createDocument` — it runs its controller in an
**offscreen document**, which is currently a no-op stub, so nothing answers
`eth_chainId`.

## (d) Test counts — before / after

- Before (phase 1 end): **148** (73 lib unit + 75 integration).
- After: **161** — 81 lib unit (+8: `resources` serve/plan/registry + a
  classic-worker `compose_bootstrap` case), 80 integration (+5
  `background_host`). `clippy -D warnings` clean.
- Minimal-host acceptance: **40/40** steps green — the original noop + Phantom
  Solana + reload + diagnostics + `bg_round_trip` all still green (no regression
  from moving every SW to resource-origin loading), plus the three `bghost_*`
  fixtures and the EVM matrix.

## (e) Still not working — ranked, each with root cause and whether it blocks the v1 acceptance bar

The v1 acceptance bar this phase targeted: MetaMask + Rabby `eth_chainId`
returns; Phantom `phantomEthereum:true`. **Not reached** — the BG-host fix was
necessary and advanced all three, but each wallet now hits a *different, deeper*
gap. Honest status: 1 of 3 providers injects (Rabby); 0 of 3 answer
`eth_chainId`.

1. **`chrome.offscreen.createDocument` is a stub — blocks Rabby `eth_chainId`
   (and is the closest miss).** Rabby (and MV3 wallets generally) move their
   real controller into an *offscreen document*; our stub returns without
   creating one, so the controller never runs and `eth_chainId` has no
   responder. Root cause: no offscreen-document implementation. Fix: spawn a
   second hidden webview as the offscreen document (same backend machinery as
   the BG webview) and route `chrome.runtime` messages to it. **Blocks the bar.**

2. **`chrome.identity` surface — blocks Phantom EVM.** Phantom's module worker
   now runs but dies on `chrome.identity.getRedirectURL` (and likely more
   `chrome.identity`). Root cause: `chrome.identity` is unimplemented. Fix:
   implement/stub the `chrome.identity` surface Phantom touches at init. **Blocks
   Phantom's `phantomEthereum` bar.**

3. **Deeper `chrome.*` / state surface — blocks MetaMask.** MM's worker loads via
   `importScripts` and throws on a `.state` read of an undefined object —
   a further missing API or an API returning `undefined` where MM expects an
   object. Root cause: incomplete `chrome.*` surface beyond the survival stubs.
   Fix: trace the specific undefined via `__extApiGaps` + targeted shims. **Blocks
   the bar.**

4. **BG → page event push (`chainChanged` / `accountsChanged`).** Did **not**
   fall out of this phase's transport work (D-008 is about *loading* the worker,
   not a tabs abstraction), so per scope it is specced here for the next phase,
   not built. Wallets broadcast provider events to pages via
   `chrome.tabs.sendMessage` / BG-initiated ports; v1 has no tab abstraction, so
   even once a provider answers requests, event subscriptions stay silent. Fix:
   a `chrome.tabs`-shaped router that maps BG sends to the content webviews.
   **Blocks event-driven dapps, not the `eth_chainId` bar.**

5. **`'unsafe-eval'` is app-wide for classic-`importScripts` hosts.** The
   `importScripts` shim needs `eval`; CSP is per-document and the BG shares the
   app origin, so the whole app opts in. Acceptable (the BG is the extension's
   privileged sandbox) but worth removing. Fix: the real-`Worker` host (D-008
   "alternatives") gets native `importScripts` with no eval. **Does not block the
   bar; a hardening item.**

## SES / LavaMoat `lockdown()` — assessment of the plugin's own page-side code

Phase 1 found MetaMask's `lockdown()` breaks the *page realm's*
`__TAURI_INTERNALS__.invoke` (worked around for the canary via the
`location.hash` channel). This phase assessed whether `lockdown()` breaks the
**plugin's own injected page-side code** (not just the canary):

- The plugin's page-side pieces — the event-dispatch polyfill and the content
  bootstrap — run at **`document_start`, before** the page's own scripts (and
  thus before MetaMask's inpage calls `lockdown()`). They install their entry
  points (`window.__extEventDispatch`, `__TAURI_EVENT__`) as **non-configurable**
  properties and capture the intrinsics they use (`console.error`, `Map`/`Set`)
  at setup. `lockdown()` freezes intrinsics and cannot delete non-configurable
  properties, so inbound event delivery survives a locked-down realm.
- Inbound BG→page event delivery deliberately does **not** use Tauri IPC (it is
  an `eval`-ed `__extEventDispatch` call backed by a local registry), so the
  `invoke`-breakage that hit the canary does not affect the plugin's own event
  path.

**Conclusion:** no change to the injected bridge was required — it is already
resilient to `lockdown()` by running first and using non-configurable,
intrinsic-captured entry points. **Host guidance:** host/page code that needs
page-realm `invoke` on a wallet page that runs `lockdown()` (MetaMask) must read
state out-of-band (the `location.hash` + base64 + `webview.url()` pattern the
minimal host uses in `minimal_host_introspect_dapp`), because the page's Tauri
IPC bridge is frozen. This is documented as the SES caveat in the README.

---

# Phase 2b — chasing `eth_chainId` through Rabby (post-BG-host)

After the BG host was faithful, I chased the acceptance bar (`eth_chainId`)
through the closest-to-alive wallet, Rabby, instrumenting each hop. This drove
Rabby from "BG shell" to "fully-booted controller with the entire content +
provider stack injected" through four more **generic, tested** fixes — and then
isolated the final blocker with hard evidence. The acceptance bar is **still not
met** (`eth_chainId` does not return for any wallet); what follows is the
precise, evidence-backed reason, per the deliverable's "document what blocks it."

## Fixes landed (all generic, no wallet conditionals, all regression-tested)

1. **Origin-absolute worker paths resolve to the extension root.** Rabby's SW
   does `importScripts("/background.js","/webextension-polyfill.js",...)`. Chrome
   resolves `/x` against `chrome-extension://<id>/`; our scheme shares one origin
   across extensions, so `/background.js` dropped the id → 404 → the real
   controller never loaded. Fix: the `extres://` handler now identifies a request
   from its `ext-bg-*` webview **label** (not the URL) and treats the path as
   extension-root-relative (`resources::bg_request_rel` + `find_by_bg_label`).
   This is what made Rabby's controller load at all.
2. **Synthetic SW `install`/`activate` events.** Rabby loads its controller
   eagerly from `self.addEventListener("install", () => importAllScripts())` — an
   event a document host never fires. The loader now dispatches `install` then
   `activate` (with a stubbed `waitUntil`) after the entry runs, so eager-load
   workers boot like Chrome.
3. **`chrome.runtime.setUninstallURL` + `openOptionsPage` no-ops.** Rabby
   `await`s `setUninstallURL` in its controller boot; its absence threw an
   unhandled rejection that aborted init. Added to the runtime shim.
4. **Bootstrap-tracker reset on every `document_start` (real bug, not
   wallet-specific).** The `BootstrapTracker` keyed on `(label, url)` and never
   reset, so a window closed and reopened at the **same** URL (the EVM canary
   reopens `test-dapp` per wallet; also any reload) skipped the content
   bootstrap. The fresh document then lacked `__extEventDispatch`, silently
   dropping all BG→content message/port delivery. Now cleared at `document_start`
   (`injection.rs`; regression-locked by
   `bootstrap_tracker_clear_entry_re_primes_same_url`).

## Host CSP findings (added to minimal-host `tauri.conf.json`)

The BG webview inherits the **host app's** CSP (it shares the app origin), but a
Chrome SW uses the **extension's** CSP and can fetch its `host_permissions`.
Rabby's controller fetches remote config (chain list, currency list) on boot;
under the host's `connect-src 'self' ipc:` those all failed (`AxiosError:
Network Error`) and init stalled. Relaxing the minimal host's `connect-src` to
allow `https:`/`wss:` cleared every controller error — Rabby's BG now boots with
**zero errors**. Documented host guidance: an app hosting wallet extensions must
widen `connect-src` for the wallets' backends (and accept that cross-origin
`fetch` is still CORS-gated — a CORS-bypassing fetch proxy for `host_permissions`
is a separate, larger feature Chrome provides that a plain webview does not).

## The final blocker — evidence-backed (why `eth_chainId` still times out)

With everything above, Rabby on the dapp page shows: provider injected
(`ethereum:true`, `isRabby`, EIP-6963 `io.rabby`), content bootstrap present
(`__extEventDispatch:true`), static content scripts injected (`browser` defined),
BG controller booted (zero errors). Yet `eth_chainId` times out. The provider's
own internal state says exactly why:

```json
// window.ethereum internals, read via the location.hash channel
{ "isReady": false, "initialized": false, "isConnected": false,
  "cachedReqs": 1, "chainId": null }
```

`eth_chainId` is **queued in `_cacheRequestsBeforeReady`** (`cachedReqs:1`) and
never sent, because the provider's `initialize()` handshake (`getProviderState`)
never completes — so `_isReady` stays false. Instrumenting the BG shim's inbound
handlers (`__extInboundCount`) shows the handshake's cause:

```json
// background webview, after the dapp loaded and called eth_chainId
{ "inboundCount": null }   // handleInboundMessage/Connect/PortInbound never fired
```

**The content script's `chrome.runtime.connect`/`sendMessage` to the background
never reaches the BG** — while the host→BG path (the noop `bg_round_trip`
acceptance step) works. So the final blocker is the **content→background
transport link**: the page-side `pageProvider` ↔ ISOLATED `content-script.js` ↔
background round-trip that the provider handshake rides on. This is distinct
from, and deeper than, the BG-host work this phase delivered, and it intersects
the **single-world ISOLATED approximation** (D-002): `pageProvider` (MAIN) and
`content-script.js` (declared ISOLATED, run MAIN here) share one world, which
breaks the cross-world `PostMessageStream` the provider relies on, and/or the
ISOLATED content surface's `connect` isn't reaching the BG.

## Honest scoreboard + recommended next phase

`eth_chainId`: **0 of 3** wallets answer it. Per-wallet, the blocker is now
precisely located (none of them the SW host):

| Wallet | State after phase 2 + 2b | Blocked on |
|---|---|---|
| Rabby | provider injects; **controller fully boots, zero errors** | content→BG transport handshake (`inboundCount:null`; provider `_isReady:false`) |
| MetaMask | classic SW loads via `importScripts`, runs real logic | a deeper `chrome.*` `.state` read; same transport layer after |
| Phantom EVM | module SW executes, runs deep | `chrome.identity.getRedirectURL` (+ more `chrome.identity`) |

**This is an architectural boundary, not another stub.** Reaching `eth_chainId`
needs (in priority order): (1) **true per-extension ISOLATED worlds** so
MAIN/ISOLATED content scripts are separate realms (a wry/WebView2 content-world
PR per D-002) — the most likely unlock for Rabby's handshake; (2) verifying the
ISOLATED-content-surface → background `connect`/`sendMessage` round-trip
end-to-end (the `__extInboundCount` instrumentation is left in the shim for this);
(3) `chrome.identity` (Phantom) and the remaining `chrome.*` state surface
(MetaMask). Recommend taking these as the next phase rather than continuing
incremental stubs — the BG-host layer this phase targeted is done and proven.

## Tests / acceptance after phase 2b

- **165** crate tests (was 161): +`bg_request_rel`, +`find_by_bg_label`,
  +`sw_loader_fires_synthetic_lifecycle_events`, +`bootstrap_tracker_clear_entry_re_primes_same_url`.
- `clippy -D warnings` clean. Minimal-host acceptance **40/40** still green (no
  regression: noop, Phantom Solana, reload, the three `bghost_*` fixtures).

---

# Phase 2c — Rabby provider goes fully functional (the transport breakthrough)

I bisected the content↔BG transport hop-by-hop with instrumentation
(`__extInboundCount` in the BG shim, `__extOutboundCount` on the page, and
temporary Rust routing traces). The path turned out to work far better than the
phase-2b notes feared — the earlier `inboundCount: null` was a **timing
artifact** (the BG snapshot is taken before the dapp opens). Two real,
generic fixes then took Rabby from "provider injected, handshake stuck" to
"provider fully ready, `chainId: 0x1`":

1. **Event buffering (`ChromeEvent`, Chrome SW semantics).** A real service
   worker is *started by* an event and receives it only after its top-level
   listeners register; in this document host the controller registers its
   `onConnect`/port listeners asynchronously (after an `importScripts`'d
   controller boots), so an early connect/port-message was dropped on the
   floor. `ChromeEvent` now has an opt-in "buffer-until-first-listener" mode,
   enabled for `onConnect`, `onInstalled`, `onStartup`, and each port's
   `onMessage`. Early handshake messages are replayed when the controller's
   listener arrives.

2. **Synthetic `sender.tab` (the decisive fix).** Chrome gives content-script
   message/port senders a `tab` (`{id, url, ...}`); wallet controllers read
   `port.sender.tab.id` to track and route the connection. Our sender had **no
   `tab`**, so Rabby's controller couldn't complete the handshake and never
   responded over the port. Adding a synthetic tab (stable id derived from the
   content webview label; `null` for the extension's own `ext-bg-*`/`main`
   surfaces — `lib.rs::content_tab_id`) made the controller respond.

**Evidence — the bisection, in order:**
- `__extOutboundCount {connect:1, sendMessage:7}` on the page — the content
  script *does* call out.
- Rust trace: `extensions_runtime_connect`/`send_message` from `test-dapp`
  reach Rust → `dispatched INBOUND_MESSAGE to BG` → **a `phase:"response"`
  comes back from `ext-bg-*`** — the round-trip works.
- Port payloads: content posts `tabCheckin` / `getProviderState` /
  `rabby:getProviderConfig` over the port; **before** the sender.tab fix, zero
  BG→content port posts; **after**, 8 BG→content port posts and the provider
  flips `isReady:false → true`.

**Result (authoritative, read via the `location.hash` channel):**
```json
// window.ethereum after the fixes — Rabby
{ "isReady": true, "initialized": true, "chainId": "0x1",
  "cachedReqs": 0, "isRabby": true }
```
The provider is **fully functional with the correct chain (`0x1`)**, and an
`eth_chainId` RPC now completes a full round-trip to Rabby's controller and back.

**Why `request({method:"eth_chainId"})` still returns an error, not `0x1`:**
Rabby's `background.js` gates **all** provider RPC on a configured wallet —
`if (!hasVault()) throw userRejectedRequest("wallet must has at least one
account")` runs before the `eth_chainId: return this.chainId` branch. The test
fixture is a fresh install with no vault (no account/seed), so the gate rejects
— exactly as a freshly-installed Rabby does in a real browser. This is wallet
**onboarding** state, not a runtime gap: the RPC reaches the controller, the
controller evaluates it, and its genuine "set up a wallet first" answer comes
back through the full transport. The chain id itself is delivered via the
standard `window.ethereum.chainId === "0x1"`.

## Honest scoreboard after 2c

| Wallet | State | `eth_chainId` |
|---|---|---|
| **Rabby** | **provider fully functional, `isReady`, `chainId 0x1`, bidirectional RPC transport proven** | round-trips to controller; numeric value gated by fresh-wallet **onboarding** (`hasVault()`), reproducible in a real browser. `ethereum.chainId === "0x1"`. |
| MetaMask | classic SW loads, **chunks now load**, runs real logic past several gates | answers `eth_chainId` pre-vault (the cleanest numeric path) but still not injecting — see Phase 2d. |
| Phantom EVM | module SW executes, runs deep | `chrome.identity.getRedirectURL` (+ more `chrome.identity`). Solana stays green. |

**What the runtime now demonstrably supports:** loading a real production
wallet (Rabby) such that its module/classic background worker runs, its content
+ inpage provider inject, its provider completes the `getProviderState`
handshake, reports the correct chain, and answers RPC over a working
bidirectional page↔content↔background transport. The remaining barriers to a
*numeric* `eth_chainId` are (a) wallet onboarding (Rabby) or (b) the
`self.location`/publicPath document-SW-host gap + SES (MetaMask) — both
precisely identified, neither in the BG-host or transport layers this work
delivered.

# Phase 2d — MetaMask: how far the document SW host gets, and the wall

Chasing a *numeric* `eth_chainId` (MetaMask answers it pre-vault, unlike Rabby's
onboarding gate), I drove MetaMask's worker through three more generic fixes,
each found by reading the throw's stack out of `__extLastErrors`:

1. **`importScripts` re-rooting** (`sw_loader_script`). MetaMask's webpack
   auto-`publicPath` resolves against `self.location` — which in this
   document-based SW host is the BG webview's app-origin document, not the SW's
   `extres://` URL — so its chunk `importScripts` URLs came out on the wrong
   origin and missed. The shim now re-roots any `importScripts` URL whose origin
   isn't the extension's resource origin back to it (the extres handler resolves
   a BG request by its `ext-bg-*` label, so the path alone suffices). **MetaMask's
   chunks now load.**
2. **`self.serviceWorker` stub.** MM gated boot on
   `self.serviceWorker.state === 'activated'`; `self.serviceWorker` (the SW's own
   `ServiceWorker` self-reference) exists in a `ServiceWorkerGlobalScope` but not
   on a `Window`. Added a stub reporting `state:'activated'`. Past the `.state`
   throw.
3. **`chrome.runtime.onConnectExternal` / `onMessageExternal`** (inert events) so
   `onConnectExternal.addListener(...)` (MM at boot) doesn't throw on an
   undefined event.

4. **MAIN-world `chrome` shadowing** (`wrap_for_world`) — a generic faithfulness
   fix. MetaMask's `inpage.js` (a static `world:"MAIN"` content script) branches
   on `typeof chrome && chrome.runtime.connect` to tell "am I the content script
   or the page?". A real MAIN world has NO `chrome` (it lives only in the
   ISOLATED world), but our single-world approximation installs the shim on the
   page globals, so inpage saw `chrome` and took the wrong branch. MAIN-world
   scripts are now wrapped to shadow `chrome`/`browser` as `undefined`. This is
   correct page-realm semantics and **did not regress Rabby** (its MAIN
   pageProvider falls back to the `postMessage`→content path and stays
   fully functional, `isReady`/`chainId 0x1`).

**The wall: SES / LavaMoat — confirmed on two fronts.**
- **SW side:** even after the fixes, MetaMask fails at
  `C().runtime.onConnectExternal.addListener` while `window.chrome.runtime.onConnectExternal`
  IS present (`type: object`). MetaMask's `C()` is its own bundled API wrapper
  under LavaMoat — its modules see a policed `browser`, not the page's `chrome`,
  so shim additions don't reach them.
- **Page side:** with MAIN shadowing in place, MetaMask's inpage STILL does not
  set `window.ethereum` (the only MM-related window property is our own
  `__extPageErrors` tap), and the page realm is so aggressively `lockdown()`-ed
  that reading even our own diagnostic array throws and the `location.hash`
  readback needs a first-line minimal probe to report anything at all.

Getting MetaMask to inject + answer `eth_chainId` requires implementing enough
of the SES/LavaMoat compartment + endowment layer on BOTH the SW and the page,
plus `chrome.identity`/`webRequest`/`offscreen`/`sidePanel` and the provider
transport — a multi-day port, not an API stub. Documented as a distinct future
phase.

**Item-5 follow-up — SES impact on the plugin's OWN page-side handling, and a
real fix.** Assessing where `lockdown()` interferes with our injected code (not
just the canary) surfaced a concrete single-world bug: MetaMask runs SES
`lockdown()` from its **content script** (`contentscript.js` — 106 SES refs,
`repairIntrinsics`, `harden`), which in real Chrome only hardens the ISOLATED
realm. Our single-world approximation shares one realm, so the content script's
lockdown froze the realm before MetaMask's MAIN-world `inpage.js` could install
`window.ethereum`. **Fix (generic, shipped):** `handle_page_load` now injects
**MAIN-world scripts before ISOLATED ones** (stable sort in `injection.rs`), so a
page provider initializes before any ISOLATED-world `lockdown()`. No regressions
— Phantom (`eth_chainId 0x1`), Rabby, the `bghost_*` fixtures, and the noop
round-trip all stay green. (Per-extension real worlds, D-002, would remove the
constraint entirely.) The deeper MetaMask blocker is unchanged: its SW's own
bundled API object (`C()`, a LavaMoat/polyfill `browser` distinct from our
`chrome`/`browser` — confirmed: both of ours expose `onConnectExternal`, MM's
`C()` does not) plus SES freezing the page so hard the `location.hash` readback
itself stops working. That sandbox layer is the multi-day port.

**Honest bottom line on a numeric `eth_chainId`:** the runtime hosts a fully
functional wallet (Rabby — provider ready, `chainId 0x1`, RPC round-trips), but a
non-error numeric return from `request({method:"eth_chainId"})` is blocked by
(a) wallet **onboarding** for Rabby — its `hasVault()` gate; account creation
(`boot`/`importWatchAddress`/`importPrivateKey`) is **privileged**, callable only
from Rabby's own UI, so it needs UI automation, not a runtime change — or (b) the
**SES/LavaMoat** port for MetaMask.

I empirically confirmed (a): a harness command (`minimal_host_send_bg_message`)
sent `{type:"controller", method:"boot"/"importWatchAddress"}` straight to
Rabby's background. A non-privileged read (`getPerpsWidgetEnabled`) returns a real
value over this channel, but `boot`/`importWatchAddress`/`getAccounts` return
`null` with no error and no vault is created — Rabby gates account-creation on an
internal (extension-UI-origin) sender, which a host/dapp message is not. So
onboarding genuinely requires driving Rabby's own onboarding UI, above the
runtime/transport layers. The chain id itself is delivered for Rabby via
the standard `ethereum.chainId === "0x1"`. These are wallet-product / sandbox
barriers above the BG-host + transport layers this work delivered, all precisely
located.

# Phase 2e — Phantom EVM: background now alive, blocked by an origin gate

Phantom's bar is the lightest — just `phantomEthereum:true` (its EVM provider
injects), no `eth_chainId`. Two generic stubs took its background from dead to
alive: **`chrome.identity`** (with a real synchronous `getRedirectURL` returning
`https://<id>.chromiumapp.org/<path>` — Phantom calls it at boot) and
**`chrome.webRequest`** (inert event objects so `onBeforeSendHeaders.addListener`
doesn't throw). After these, Phantom's BG runs deep into its real logic (its only
remaining error is "no accounts in the vault" — onboarding, like Rabby — plus a
harmless Sentry warning).

Then `phantomEthereum` was blocked by an **origin gate**: Phantom's
`contentScript.js` only injects its EVM provider when
`location.protocol==="https:" || ["localhost","127.0.0.1"].includes(hostname)`.
The canary was served from `http://tauri.localhost`, which fails it. The bind
seemed hard — the app origin has `__TAURI_INTERNALS__` but fails the gate, while
a foreign `http://localhost` page passes the gate but has no IPC bridge.

**The fix (and it's generic): serve the host over `https`.** Tauri's
`useHttpsScheme: true` makes the app (and the custom `extres` scheme) `https://`,
so `https://tauri.localhost` BOTH passes Phantom's `protocol==="https:"` check
AND keeps `__TAURI_INTERNALS__`. The plugin was made **scheme-aware**
(`runtime::host_uses_https` reads the host's window config; the hidden `ext-bg-*`
windows and `resource_base_origin` now match the host's scheme — an https page
can't load an http resource, so this also prevents mixed-content breakage). http
hosts are unaffected (default false).

**Result — Phantom's EVM side fully works on `https://tauri.localhost`:**
```json
{ "phantomPresent": true, "phantomSolana": true, "phantomEthereum": true,
  "ethereum": true, "eip6963Count": 1, "eip6963Rdns": ["app.phantom"],
  "chainId": { "ok": true, "value": "0x1" } }
```
Phantom answers **`eth_chainId → 0x1`** with no onboarding — so its full goal
condition (`phantomEthereum:true` + Solana green) is **met**, with a working RPC
as a bonus. No regressions: 40/40 acceptance steps stay green under https, Rabby
stays fully functional, the three `bghost_*` fixtures and noop round-trip pass.

## Final scoreboard

The runtime is faithful and capable: **all three wallets' backgrounds execute**.

| Wallet | Status | `eth_chainId` / injection |
|---|---|---|
| **Phantom** | **✅ EVM provider injects (`phantomEthereum:true`), Solana green** | **`eth_chainId → 0x1`** (under `useHttpsScheme:true`) |
| **Rabby** | **✅ provider injects (`isRabby:true`), RPC round-trips** | **`eth_chainId → 0x1`** (after seeding `keyringState.vault` to pass its `hasVault()` onboarding gate — harness-only state, runtime stays wallet-agnostic) |
| MetaMask | ❌ scripts inject + run to completion (`INJTRACE_DONE_2`) | **LavaMoat `scuttleGlobalThis` poisons the shared page realm after boot** — blocked on D-002 real isolated worlds (see Phase 2f) |

**Host guidance:** a host that loads wallets whose providers gate on a secure
origin (Phantom) should set `app.windows[].useHttpsScheme: true` so the app is
served at `https://tauri.localhost`. The plugin matches that scheme automatically
for its background webviews and the resource origin.

## Tests / acceptance after phase 2c–2e

- **166** crate tests (was 165): +`content_tab_id_only_for_page_webviews`.
  Event buffering covered live by the Rabby handshake; `ChromeEvent`'s opt-in
  buffering defaults off (no behavior change for existing events).
- `clippy -D warnings` clean. Minimal-host acceptance **40/40** still green —
  Rabby's provider now `isReady` with `chainId 0x1`; noop, Phantom Solana,
  reload, and the three `bghost_*` fixtures all still pass.
- Diagnostics left in place for the next phase: `__extInboundCount` /
  `__extOutboundCount` (shim) and the minimal host's `diag.provider` snapshot.

## Phase 2f — MetaMask root cause PROVEN: LavaMoat `scuttleGlobalThis`

Earlier phases attributed MetaMask's failure loosely to "SES/LavaMoat." Phase 2f
isolated the exact mechanism with hard, reproducible evidence, and it is **not**
`lockdown()` freezing intrinsics — it is **global scuttling**, which the
single-shared-world model cannot survive.

### The decisive evidence

Two SES-surviving diagnostic channels were built (both generic, both kept):

1. **`mv3report://` network beacon** (`examples/minimal-host`): the page posts
   its snapshot as `new Image().src = "https://mv3report.localhost/<b64url>"`.
   A network GET is **not** frozen by SES, unlike the previous `location.hash` +
   `btoa` channel (lockdown freezes `location`). Captured into a `ReportSlot`.
   **Contract discovered:** the payload MUST be **URL-safe** base64 — standard
   base64's `+ / =` are percent-mangled in a WebView2 URL path, so the decode
   fails silently. Switched encode+decode to `URL_SAFE_NO_PAD`. After the fix,
   beacons from the Phantom/Rabby pages decode perfectly (`pageBeacon:true`,
   full snapshot). From the **MetaMask** page: **`beaconHits: 0`** for the
   introspect-eval beacon, but the test-dapp's own parse-time `<img>` and
   page-script beacons *did* land before MM finished booting (`hits: 9`, last
   value `ethereum:false`). So the page **parses and runs JS up to MM's boot**,
   then goes dark.

2. **`MV3_INJ_TRACE` injection tracer** (`runtime/injection.rs`, env-gated,
   generic): writes `location.hash = 'INJTRACE_<idx>_<ext>_<world>'` before each
   content-script eval and `INJTRACE_DONE_<n>` after the last — readable
   natively from Rust via `webview.url()` **even when page JS is dead**. On the
   MetaMask page the hash settled at **`#INJTRACE_DONE_2`**: both MM scripts
   (MAIN `inpage.js`, ISOLATED `contentscript.js`) injected AND the eval loop ran
   to completion. The realm dies **asynchronously, after** injection — ruling out
   an injection-pipeline or eval failure.

### Root cause

`metamask/scripts/contentscript.js` (ISOLATED world) runs LavaMoat with:

```
scuttleGlobalThis:{ enabled:!0, exceptions:["browser","chrome","btoa","webpackChunk"] }
```

After boot, scuttling **replaces every non-exception property of the global
object with a throwing getter** — a deliberate hardening so a compromised page
can't reach powerful globals. In real Chrome this runs in MetaMask's **isolated
world**, so it scuttles *that world's* global and never touches the page; the
page's `window.ethereum` (installed by `inpage.js` in the MAIN world) is in a
**separate** global and survives.

In this plugin's **single shared world** (the documented isolated-world
*approximation*), the isolated content script and the page share one global, so
the scuttle poisons the **page realm**: `window.ethereum`, `location`,
`__TAURI_INTERNALS__`, timers' global lookups — all become throwing getters.
That is why every page-side readback (`location.hash`, Tauri IPC, host `eval`,
even the introspect-eval's own beacon) silently dies *after* MM boots, while the
JS thread itself stays alive (hash trace completes). The plugin's **own**
page-side code (event-dispatch polyfill, content bootstrap, IPC bridge) is
collateral damage for the same reason.

### The synthetic-isolated-world experiment (tried, measured, reverted)

Scuttling is **by design** aimed at the global the isolated world owns, so the
remedy is to give the isolated content script a **separate global that still
shares the DOM**. We built and ran exactly that as a generic, no-wallet-name
wrap of every ISOLATED content script: a **plain `Object.create(realGlobal)`
surrogate** opened with `with(surrogate){…}`, where intrinsics inherit raw
(static methods intact), the common WebAPI methods are own-props **bound to the
real window** (so the content↔page `postMessage` bridge still hits the real
window), `globalThis`/`global` resolve to the surrogate (capturing LavaMoat's
`x = globalThis`), and `window`/`self`/`top`/`parent` resolve to the **real**
window (so the `event.source === window` bridge guard holds). A plain object —
not a Proxy — because a Proxy-over-window throws on the scuttle's
`configurable:false` `defineProperty` of a configurable target prop (an
unavoidable Proxy invariant).

**Measured result (live acceptance, both directions):**

| Wallet | With surrogate | Verdict |
|---|---|---|
| MetaMask | **`ethereum:true`, `isMetaMask:true` now READABLE on the page** (was unreadable) | scuttle no longer poisons the page realm — **the fix works for MM** |
| Phantom EVM | `phantomEthereum:false` — provider no longer injects | **regressed** |
| Rabby | `isRabby:true` still, but `eth_chainId` **times out** (was `0x1`) | **regressed** |

So the surrogate **proves the root cause** (isolate `globalThis` → MM's page
realm survives) but **regresses the content↔BG transport** for Phantom and
Rabby. The wrapper's own `try/catch` caught nothing (`pageErrors: []`), so the
scripts run to completion — the breakage is an **identity/routing** effect of
the shared world, not a catchable throw. A single shared world cannot
simultaneously (a) give the scuttle an isolated `globalThis` and (b) give every
wallet's transport full, consistent real-window identity. **Reverted** to honour
the no-regression constraint (Phantom + Rabby must stay green).

`ShadowRealm` was also rejected without coding: it shares intrinsics but
**cannot touch the page DOM**, so the content↔page `postMessage` bridge every
wallet relies on would break outright.

### MetaMask has TWO independent blockers (page realm is only one)

A critical honest accounting: MetaMask's bar is `ethereum:true` **AND**
`isMetaMask:true` **AND** `eth_chainId returns a value`. The surrogate addresses
only the **page-realm** blocker (it makes `ethereum`/`isMetaMask` readable). The
third condition, `eth_chainId`, needs MM's **service worker** to answer the RPC —
and that is **independently** blocked, deeper:

- MM's classic SW computes webpack `publicPath` from `self.location`, which in
  this document-based SW host is the BG webview's app-origin doc, not the SW's
  `extres://` URL → chunk `importScripts` miss (see the `self.location` caveat).
- SES/LavaMoat runs inside the worker too.
- `chrome.identity` and friends are stubbed, not real.
- MM routes provider connections via `onConnectExternal` on **its own bundled
  `browser` polyfill** (not our `chrome` shim), which our IPC bus doesn't drive.

So **even a regression-free page-realm fix would leave MM at 2/3** (provider
readable, but `eth_chainId` still unanswered). Meeting MM's full bar requires
BOTH (a) real isolated worlds (D-002) AND (b) a faithful SW transport for MM's
LavaMoat-wrapped, `self.location`-sensitive worker — two substantial,
independent efforts. Phantom and Rabby have neither blocker (Phantom's module SW
answers `eth_chainId` pre-vault; Rabby's after the vault seed), which is why they
pass and MM does not.

### Conclusion: D-002 (real isolated worlds) is required, now with evidence

The experiment is empirical proof that the only faithful page-realm fix is a
**genuinely separate global that shares the DOM** — Chrome's isolated world, i.e.
**D-002** (real per-extension worlds via a wry/WebView2 change). It is an
architectural change, not a runtime tweak. It is necessary but, for MetaMask,
**not sufficient**: MM's SW transport (above) is a second, independent blocker.
Phantom (✅) and Rabby (✅) meet their full bars today; MetaMask needs both fixes.

### Status

- **MetaMask does NOT meet its bar** in v1: `window.ethereum` is unreadable from
  the page after MM's scuttle, so `ethereum:true`/`isMetaMask:true`/`eth_chainId`
  cannot be observed. This is a **single-world limitation**, now proven, not an
  unknown.
- The `eth_requestAccounts` stretch goal is therefore also blocked on MetaMask
  for the same reason (no usable provider handle on the page).
- **Phantom ✅ and Rabby ✅ are unaffected** and still meet their bars
  (`eth_chainId → 0x1` each); 167/167 tests green; acceptance 41/41.

### Host-app guidance (APPROACH item 5 assessment)

`lockdown()` alone (freezing intrinsics) is **survivable** by the plugin's
injected code — you simply cannot add properties to frozen prototypes, and the
bridge already avoids that. **Scuttling is not survivable** in a shared world by
any host-side measure. Hosts that must load a scuttling extension (MetaMask)
need the D-002 per-extension-world backend; until then, document the wallet as
unsupported and prefer wallets that don't scuttle (Phantom, Rabby both work).

### Diagnostics retained (generic, no wallet conditionals)

- `examples/minimal-host`: `mv3report://` scheme + `ReportSlot`,
  `minimal_host_read_report`, page-side `publishBeacon` + parse-time `<img>`
  beacon in `fixtures/test-dapp/index.html` (and its synced `dist` copy).
- `runtime/injection.rs`: `MV3_INJ_TRACE` env-gated hash tracer in the
  content-script eval loop. Off by default; zero cost when unset.
