# MV3 — Tauri Extension Runtime

**Status:** parked standalone project, spun out of `wallet-harness` on 2026-04-20.

## What this is

A Rust crate + bundled JS runtime that lets a Tauri app load and run unpacked Chromium MV3 browser extensions inside its webview the same way Chrome / Brave / Edge do. Target extensions include — but are not limited to — web3 wallets (MetaMask, Rabby, Phantom, Rainbow), content blockers, password managers, and any productivity extension built against the MV3 API.

Spun out of the `wallet-harness` wallet terminal project after a scoping discussion on 2026-04-20. The wallet-harness itself is better served by an EIP-1193 provider shim (simpler, ships in weeks) than by a full extension runtime. This project — the road not taken there — is preserved because the runtime is valuable on its own merits, separate from any single app that might consume it.

## Why build it as a standalone tool

- **Real gap in the Tauri ecosystem.** Tauri apps currently cannot run browser extensions. There is no official plugin, no stable open-source runtime, no maintained wry fork that supplies this. Apps that need extensions either abandon Tauri for Electron or do without.
- **Market size.** Every Tauri-based dapp browser, web3 wallet, privacy tool, or "desktop companion for a web app" eventually hits this wall. Crypto is the loudest cohort but not the only one.
- **Precedent.** Electron shipped `session.loadExtension`, and a meaningful slice of the desktop-app ecosystem moved toward Electron specifically for that capability. A Tauri equivalent recaptures that ground while preserving Tauri's native-perf and small-binary advantages.
- **Business shape.** Reusable as a dependency; sellable as a commercial plugin; open-sourceable under dual MIT/Apache to become the default the ecosystem points at. "Tauri Extensions" is a plausible sister brand.

## Scope — what v1 looks like

A Rust crate (working name `tauri-plugin-extensions`) plus a bundled JS runtime that, given a Tauri webview and an unpacked MV3 extension directory, can load the extension such that:

- Content scripts inject per the manifest's `matches` / `exclude_matches` / `run_at` / `world` rules
- `chrome.runtime` messaging works end-to-end: `sendMessage`, `onMessage`, `connect`, `onConnect`
- `chrome.storage.local` and `chrome.storage.session` persist across page reloads and app restarts
- MV3 service-worker analog runs for background logic (the hardest subsystem — see Architecture)
- Page-context `window.chrome.runtime.sendMessage` reaches the background worker
- Extension permission prompts render through native Tauri dialogs
- **Acceptance test:** MetaMask (mainline MV3 build) loads clean and completes a signature flow against a trivial test dapp served locally

**Out of scope for v1:** devtools panel integration, browser-action popups rendered in a separate OS window, `chrome.tabs` (we don't have a tab abstraction), `chrome.storage.sync`, extension auto-updates, per-extension process isolation.

## Architecture sketch

Two major subsystems.

**Subsystem A — manifest parsing + content-script injection.**
Rust-side `manifest.json` parser. Implements Chrome's URL-pattern matcher and glob matcher faithfully. Hooks into wry's page-navigation lifecycle to inject scripts at the correct moment (`document_start` / `document_end` / `document_idle`) and into the correct world (main vs. isolated). Injection via wry's `evaluate_script` plus a per-frame bootstrap that sets up the isolated-world sandbox.

**Subsystem B — `chrome.*` API shim + background runner.**
TypeScript runtime bundled into the plugin, exposing the subset of `chrome.*` APIs that MV3 wallet and productivity extensions actually use. Messages between content scripts, background, and popup flow through Tauri IPC on the Rust side — the Rust plugin is the extension runtime's message bus, since embedded webviews cannot talk to each other directly.

The **background service worker** is the hardest architectural call. MV3 extensions run background logic in a service worker, which WebView2 does not natively host for extensions. Three candidate approaches, to be evaluated in the spike:

1. **Hidden off-screen webview.** Run `background.js` in a separate Tauri webview hidden from the user; IPC routes messages in and out. Most Chrome-faithful; pays the cost of an extra webview per loaded extension.
2. **Embedded JS engine in Rust.** Use `boa` or `deno_core` to run `background.js` without a webview. Lower memory cost; significant API shimming since most web platform APIs aren't available.
3. **Shared background worker across all loaded extensions.** One engine hosting all background scripts in separate realms. Efficient but introduces cross-extension isolation risk.

## Timeline estimate

- **Spike — 2 weeks.** Minimal no-op MV3 extension loads, injects a content script, round-trips a message to a background runner, persists to `chrome.storage.local`. Goal is to de-risk the background-worker architecture call. Kill-switches: if none of the three candidate approaches above are viable on WebView2 without breaking its security model, reassess before committing.
- **v1 — 8 to 12 weeks past end of spike.** Hits the acceptance criteria above, with MetaMask as the acceptance test.
- **Stability tail — ongoing.** Realistic cost ~1 week per quarter to track Chromium / WebView2 / MetaMask / MV3-spec drift.

## Known risks

- **WebView2's MV3 support is preview-stage and partial** as of early 2026. Microsoft has announced extension APIs for narrow enterprise scenarios; general-purpose extension hosting is not a supported path. Expect to route around, not lean on, whatever Microsoft ships. Confirm current state at project start — this may have moved.
- **WebKitGTK (Linux) offers zero extension support.** Either build the Linux runtime entirely from scratch or ship Windows-only first.
- **WKWebView (macOS) also offers zero.** Three platforms, three runtimes. Plan for platform-specific backends behind a common Rust interface.
- **Extension authors can detect we're not Chrome.** Wallet extensions sometimes refuse to load in non-Chrome contexts. Mitigation: faithful-enough API surface plus user-agent presentation. Adversarial tail possible; budget for ongoing cat-and-mouse.
- **License / ToS.** MetaMask's license permits embedding; some other extensions may not. Per-extension legal review before claiming public support.

## Open questions to answer at project start

- Boa vs. deno_core vs. hidden-webview for the background runtime — decided in the spike.
- Tauri plugin (`tauri-plugin-extensions`) or wry fork? Plugin is cleaner; fork may be necessary if webview lifecycle hooks aren't exposed.
- macOS / Linux parity — ship Windows-only v1, or commit to cross-platform from the start?
- License: MIT/Apache dual (standard Rust default), or something that accommodates future commercial licensing?

## Prior art worth reading before starting

- Electron's `session.loadExtension` source — a working reference for loading unpacked extensions inside an embedded webview.
- Brave's extension compat work — Chromium-based, but wrestles with similar API-shimming problems at the edges.
- `tauri-plugin-wry-extensions` — an early community experiment, unmaintained; worth reading for what was attempted and why it stopped.
- Microsoft's WebView2 documentation on extension support policy — sets the boundary of what's possible without forking.
- The MV3 specification itself (Chrome Developer docs) — the ground truth for what the runtime must implement.

## Resume checklist — for someone picking this up cold

1. Re-read this document end to end.
2. Check the current state of WebView2's MV3 extension APIs in Microsoft's docs and changelog. If it has matured materially since 2026-04-20, the scope may shrink.
3. Follow the Spike plan above as the first two weeks of work.
4. Keep MetaMask (MV3) as the v1 acceptance target — it's the hardest real-world extension, and if it works, most others will.
5. Related context on *why* this was carved off as a separate project lives in the `wallet-harness` repo under `docs/superpowers/specs/` — specifically the 2026-04-20 Step 02 spec that triggered the decision.
