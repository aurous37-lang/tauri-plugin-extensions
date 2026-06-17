# Architectural Decisions

This document records commitments made at project start that shape how the
rest of the codebase is built. The engineering-summary.md at the repo root
lists four open questions marked "decided in the spike" — this file resolves
them so parallel implementation work does not diverge.

## D-001 — Background runner: hidden off-screen webview

**Decision:** The MV3 background service-worker analog runs inside a hidden
`WebviewWindow` (visible=false, decorations=false, skip_taskbar=true,
focus=false) owned by the plugin. One hidden webview per loaded extension.

**Alternatives considered:**

- Embedded JS engine in Rust (boa / deno_core). Lower memory cost, but the
  shim surface balloons: MetaMask, Phantom, and Rabby all use `indexedDB`,
  `crypto.subtle`, `fetch`, `WebSocket`, and the URL / Blob / FormData web
  platform APIs inside their background workers. Rebuilding that surface
  against a bare JS engine is a multi-quarter project and an ongoing drift
  risk. Rejected.
- Shared background worker across all extensions. Efficient, but introduces
  cross-extension isolation risk (a single hosted-engine bug bleeds state
  between wallets, which is exactly the opposite of what a wallet runtime
  needs). Rejected.

**Consequence:** Per-extension webview RAM cost (~30–60 MB each, empirically
measurable). Acceptable for wallet-scale (1–3 extensions typical); revisit if
a target consumer needs dozens of extensions loaded simultaneously.

## D-002 — Packaging: Tauri plugin, not wry fork

**Decision:** Ship as `tauri-plugin-extensions`, a standard Tauri v2 plugin.

**Alternatives considered:**

- Fork wry to expose additional webview lifecycle hooks. Would give tighter
  control over navigation events and script-injection timing, but forks the
  maintenance burden onto us, fragments from upstream wry fixes, and makes
  consumer adoption meaningfully harder ("clone our wry fork" vs "add a
  plugin dep").
- Upstream the required hooks into wry. Correct long-term move; bootstrapping
  cost is wrong for spike stage. Revisit after v1 if lifecycle hooks prove
  inadequate — an upstream PR is cheaper than maintaining a fork.

**Consequence:** Any capability not exposable from the plugin surface is a
blocker and gets upstreamed to wry/Tauri rather than worked around with a
fork. If the spike hits a hard blocker, this decision is the first to revisit.

**Amendment 2026-06-10 (first concrete hard blocker — real isolated worlds).**
The EVM session identified the first capability that is *not* exposable from the
plugin surface and therefore triggers this decision's "upstream to wry" clause:
**per-extension isolated content-script worlds** (Chrome's "separate JS globals,
shared DOM"). The plugin approximates isolation by running every content script
in the page's single shared realm. That holds for Phantom and Rabby, but
**MetaMask** fails the acceptance bar because of it: MM's `contentscript.js` runs
LavaMoat `scuttleGlobalThis` (captured as `x = globalThis`), which replaces every
non-exception global with a throwing getter. In Chrome that scuttle hits MM's
isolated world; in the shared realm it poisons the **page** realm, so
`window.ethereum`/`isMetaMask` become unreadable and the plugin's own page bridge
dies. A generic shared-world fix was **built, measured, and reverted** (a
`with(Object.create(realGlobal))` surrogate global): it made MM's provider
readable (`ethereum:true`, `isMetaMask:true`) but regressed Phantom EVM injection
and Rabby `eth_chainId` (the shared world can't give the scuttle an isolated
`globalThis` *and* every wallet's content↔BG transport full real-window identity
at once). Full evidence: `docs/bg-host-service-worker.md` Phase 2f. **Next phase:
upstream real isolated-world script injection to wry/WebView2** (per this
decision's consequence clause) — it is the single change that lets all three
wallets pass simultaneously. Until then, MetaMask is documented as unsupported;
non-scuttling wallets (Phantom, Rabby) work.

## D-003 — Platforms: Windows-only v1, trait-abstracted backend

**Decision:** v1 ships Windows only (WebView2). macOS (WKWebView) and Linux
(WebKitGTK) ship later through platform-specific backends behind a common Rust
trait.

**Rationale:** Engineering summary flags that both WKWebView and WebKitGTK
offer zero native extension support. Building three runtimes simultaneously
triples the spike scope and delays the v1 Phantom acceptance by a calendar
quarter. WebView2 on Windows is the one platform where at least *partial*
MV3-adjacent plumbing exists, so it is the right v1 target.

**Consequence:** `crates/tauri-plugin-extensions/src/runtime/` exposes a
`Backend` trait; the concrete Windows impl lives in `runtime/webview2.rs`.
Stub `runtime/wkwebview.rs` and `runtime/webkitgtk.rs` exist from day one to
force the trait to stay platform-agnostic.

## D-004 — License: PolyForm Noncommercial 1.0.0 (amended 2026-06-17; originally MIT OR Apache-2.0)

**Decision (amended 2026-06-17):** Single source-available license —
[PolyForm Noncommercial License 1.0.0](https://polyformproject.org/licenses/noncommercial/1.0.0).
Free for any noncommercial purpose; commercial use requires a separate
commercial license from the copyright holder.

**Rationale (amendment):** The project is being taken to market. A permissive
dual MIT/Apache license grants everyone an irrevocable, royalty-free right to
use and redistribute commercially, which is incompatible with selling
commercial licenses. PolyForm Noncommercial keeps the source public (essential
for a developer-infrastructure product: evaluation, trust, discoverability)
while reserving commercial use for paid licensing. PolyForm was chosen over
BUSL-1.1 for simplicity (no time-bomb conversion to manage) and over a fully
private repo because the public source *is* the storefront for this kind of
crate.

**Consequence for crates.io:** PolyForm Noncommercial is a valid SPDX
identifier (`PolyForm-Noncommercial-1.0.0`) but is **not** an OSI-approved /
permissive license. The prior "publish a free crate to crates.io" plan no
longer holds as-is — distribution is now via the public GitHub repo under the
noncommercial license, with commercial licenses sold separately. Revisit if an
open-core split (permissive core + commercial add-ons) is later preferred.

**Superseded text (original D-004, 2026-04-20):** Dual MIT + Apache-2.0,
matching the Rust ecosystem default — kept commercial options open via
future relicense of owned contributions, avoided copyleft/custom-license
cat-herding, matched what Tauri ships. Reversed because "future relicense"
cannot retract the permissive grant already given to downstream users; the
clean path is to license restrictively from the start.

## D-005 — Acceptance target order: Phantom → MetaMask + Rabby

**Decision:** v1 acceptance is Phantom (single wallet). v2 acceptance brings
up MetaMask and Rabby in parallel (both Ethereum-first, share enough EVM
heritage that the shim maturation amortizes).

**Overrides:** The original engineering-summary.md says "MetaMask as the v1
acceptance test." This decision replaces that, per user direction on
2026-04-20. Reasoning: the product pull for this runtime is a new-user-facing
crypto experience ("click Connect Wallet and it just works"), and Phantom's
UX is the one newcomers recognize. MetaMask's manifest is more complex and
harder to de-risk; using it as v1 delays first proof-of-life unnecessarily.

**Consequence:** All test fixtures, shim completeness checks, and tracing
output default to Phantom first. When Phantom exercises a `chrome.*` path
that's unimplemented, we implement it immediately. MetaMask and Rabby only
drive work after Phantom passes acceptance.

## D-006 — Rust toolchain: stable-x86_64-pc-windows-msvc

**Decision:** Same toolchain the parent wallet-harness workspace uses. Pinned
in `rust-toolchain.toml`. No GNU/mingw, no nightly.

**Rationale:** Matches the surrounding ecosystem this code will be consumed
from, avoids linkage drift with `wry` / `webview2-com` (which are MSVC-only
in practice), and keeps CI simple. Consistent with the parent project's
`Tauri_App_Win/verify.ps1` guarantees.

## D-007 — Extension lifecycle is a first-class subsystem

**Decision:** A dedicated `LifecycleManager` owns every mutation of extension
runtime state. The explicit states are `Installed → Running → Stopping →
Stopped → Uninstalling`, with typed `StopReason` for every `Stopped`
transition. No other module may call `Backend::spawn_background` or
`BackgroundHandle::shutdown`; those are lifecycle-internal.

**Rationale:** The original spike had no lifecycle concept. Every call to
`load_unpacked` minted a fresh `ExtensionId`, spawned a fresh hidden WebView2
window, and left prior ones running. A single dev run accumulated 92 hidden
webview instances (~3–5 GB RAM) before the acceptance harness caught it.
That's a design problem, not a bug: without a state machine there is no
definition of "reload," no idempotency, no graceful shutdown, no orphan
cleanup. Enterprise extension runtimes (Chromium, Electron's
`session.loadExtension`, Firefox's `AddonManager`) all model this
explicitly; we match their shape.

**Consequence:** The loader is a thin shim that delegates to
`LifecycleManager::install_or_reload`. Identity is stable: `ExtensionId::from_key`
(manifest-key-derived, Chromium-faithful) or `ExtensionId::from_source_dir`
(SHA-256 of canonical path, case-folded on Windows) — never random. Installed
extensions persist to `app_data_dir/extensions/state.json` via an atomic
`.tmp`+rename store and are re-installed on boot. Every transition emits
`extensions://lifecycle/changed` so host UIs don't poll. Orphan reconciliation
runs at boot and kills any `ext-bg-*` window the manager doesn't own — this
is the closure for the pre-lifecycle zombie-window bug.

**Non-consequences:** The existing `ExtensionRegistry` stays, but becomes a
read-only projection of the manager's state, re-synced after every
transition. Consumers that already depend on the registry keep working; new
code should prefer the manager directly.

## D-008 — Background service worker is loaded from the resource origin, not inlined

**Amends D-001's implementation** (the background runner is still a hidden
off-screen webview; this changes only how the worker's code gets into it). Does
not relitigate D-001 — it makes D-001's runner faithful to the two MV3
service-worker loading mechanisms it previously could not honor.

**Decision:** The hidden background webview no longer concatenates the
extension's `background.service_worker` source into a classic-script
`initialization_script`. Instead, after the chrome.* shim + bridge are in place,
it loads the worker **from the `extres://` resource origin**:

- `background.type: "module"` → `import("extres://<id>/<sw>")` — a native ES
  module graph; relative `import` specifiers resolve against the extension root.
- `background.type: "classic"` (default) → a synchronous `importScripts` shim
  (absent on a `Window`): a synchronous fetch over the resource origin +
  global-scope `eval`, resolving relative specifiers against the worker's
  directory. The entry is loaded by calling that shim on the entry URL.

The background webview document stays on the **app origin** (so
`__TAURI_INTERNALS__.invoke` / `chrome.*` remain available to the worker); only
the worker *script* is fetched from `extres://`. A script's fetch URL — not the
document's origin — is what determines module/`importScripts` resolution, so
this dissolves the "must be app origin vs. must resolve relative imports"
conflict cleanly. The resource scheme serves a BG webview its own private files
(the SW + its chunks, which are not `web_accessible_resources`) on a privileged
path keyed to the requesting `ext-bg-*` label; every other webview stays
WAR-gated (see `src/runtime/resources.rs`, `serve_mode`).

**Evidence forcing it (phase-1 → phase-2):** the inlined-classic-script host
was a hard blocker for real wallets. Phantom's `type:"module"` worker began with
a top-level `import`, which is a *parse-time* `SyntaxError` in a classic script —
its entire background (error-tap, shim, bridge, and all real logic) never ran
(`__extLastErrors` empty). MetaMask and Rabby pull their real background logic in
via top-level `importScripts(...)`, absent on a `Window`, so their workers were
shells. After D-008, with no other change: Phantom's module worker executes and
runs deep into its real logic (now blocked on a *narrower* missing API,
`chrome.identity.getRedirectURL`); MetaMask's classic worker loads via
`importScripts` and runs its real logic; Rabby is unchanged (already injecting).
Locked by `fixtures/test-extensions/{module-import,classic-importscripts,
module-register-content}-mv3` + `tests/background_host.rs` + the minimal-host
`bghost_*` acceptance steps.

**Alternatives considered:**

- *Keep inlining, special-case module workers.* Rejected: there is no way to
  inline a top-level `import` into a classic script, and detecting/transforming
  `importScripts` usage statically is fragile. The uniform "load from the origin"
  path is both simpler and more faithful.
- *Run the worker in a real `Worker` / `new Worker(url, {type:"module"})`.* This
  would give native `importScripts` + module semantics with **no `eval`** and an
  off-main-thread worker — strictly more faithful. Deferred, not rejected: a
  `Worker` has no `window`/`__TAURI_INTERNALS__`, so the chrome.* shim's invoke +
  the Rust→worker event dispatch would need a `postMessage` bridge through the
  host document. That is the right long-term shape (revisit when the document
  host's `window`-vs-worker mismatches start biting); the document host ships
  now because it reuses the existing IPC transport unchanged.

**Consequence — host CSP:** a strict, eval-free CSP is enough for module
workers. **Classic-`importScripts` workers require the host CSP to allow
`'unsafe-eval'`** (the shim evaluates fetched chunks; Chrome's native
`importScripts` is not `eval`, so we cannot avoid it in a `Window`). The
background webview is the extension's privileged sandbox, so this is defensible,
but it is app-wide (the BG shares the app origin's CSP), so hosts that load
classic-`importScripts` wallets (MetaMask, Rabby) opt into `'unsafe-eval'`
knowingly. Documented in the README + minimal-host `tauri.conf.json`.
