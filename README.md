# tauri-plugin-extensions

Load and run unpacked Chromium MV3 browser extensions inside a Tauri v2 app —
the capability Electron ships natively as `session.loadExtension`, brought to
the Tauri ecosystem.

**Status:** v1 preview. Windows only (WebView2); macOS and Linux are on the
roadmap. Not yet published to crates.io.

> **License:** Source-available under the
> [PolyForm Noncommercial License 1.0.0](./LICENSE.md) — free for any
> noncommercial use. **Commercial use requires a separate commercial license**
> (see [COMMERCIAL-LICENSE.md](./COMMERCIAL-LICENSE.md)).

## Why

Tauri apps cannot host browser extensions. There is no official plugin, no
stable community runtime, and no maintained wry fork that provides one. Teams
that need extensions — wallet integrations, password managers, content
blockers, any "desktop companion" that leans on the extension ecosystem —
either switch to Electron or go without.

This plugin closes that gap. Point it at an unpacked MV3 extension directory
and it parses the manifest, injects content scripts per the manifest's match
rules, hosts the background service worker, routes `chrome.runtime` messages,
and persists `chrome.storage` — with a real lifecycle (install / reload /
enable / disable / uninstall) underneath.

**Proven against real extensions:** the Phantom wallet loads cleanly with its
Chromium-faithful extension id, spawns its background worker, and injects its
provider into pages — the demo dapp observes `window.phantom.solana` with
`isPhantom: true`, exactly as it would under Chrome. MetaMask and Rabby
manifests parse today and are the v2 acceptance targets.

## Quickstart

```toml
# Cargo.toml
[dependencies]
tauri-plugin-extensions = "0.1"
```

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_extensions::init())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let id = tauri_plugin_extensions::load_unpacked(
                    &handle,
                    std::path::Path::new("C:/path/to/unpacked-extension"),
                )
                .await
                .expect("extension loads");
                println!("loaded extension {id:?}");
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri run");
}
```

Loading is idempotent: calling `load_unpacked` against the same directory
reloads the extension in place instead of duplicating it. Every lifecycle
transition emits an `extensions://lifecycle/changed` Tauri event, and the same
surface is exposed to your frontend as plugin commands
(`extensions_load_unpacked`, `extensions_list_lifecycle`, `extensions_reload`,
`extensions_enable`, `extensions_disable`, `extensions_unload`,
`extensions_diagnostics`, …) gated by Tauri's capability system.

> **Host frontend note:** each extension's background worker runs in a hidden
> webview that loads your app origin's root document. If your frontend runs
> boot-time logic, guard it so it no-ops when
> `window.__TAURI_INTERNALS__.metadata.currentWebview.label` starts with
> `ext-bg-`.

### Run the demo

`examples/minimal-host` is the smallest host app. It auto-runs an acceptance
sequence on boot (load → reload → background checks → diagnostics) and writes
`acceptance-report.json`:

```powershell
# 1. Fetch a real wallet extension (Phantom) from the Chrome Web Store
.\scripts\fetch-phantom.ps1

# 2. Run the host
cd examples\minimal-host
pnpm install
pnpm tauri dev
```

You'll see the host window plus a second window with a canary dapp page that
probes for the wallet's injected providers (`window.phantom`,
`window.ethereum`, …). The hidden `ext-bg-*` background webview shows up in
the report's window list.

## What works (v1)

| Surface | Status |
|---|---|
| MV3 `manifest.json` parsing (typed, MV2 rejected) | ✅ |
| Content-script injection (`matches`, `run_at`, MAIN/ISOLATED `world`) | ✅ |
| Chrome-faithful URL match patterns + globs | ✅ |
| Chromium-faithful extension ids (manifest `key` → same id Chrome assigns) | ✅ |
| Background service worker (hidden WebView2 window per extension) | ✅ |
| `chrome.runtime`: `id`, `getURL`, `getManifest`, `sendMessage`, `onMessage`, `connect`, `onConnect`, `onInstalled`, `onStartup` | ✅ |
| Content/host → background message round-trip (`sendMessage` + async `sendResponse`) | ✅ |
| Content-script → background ports (`connect` / `port.postMessage` / `disconnect`) | ✅ |
| Host → extension messaging from Rust (`send_message_to_background`) | ✅ |
| `chrome.storage.local` / `chrome.storage.session` (promise + callback, `onChanged`) | ✅ |
| Extension lifecycle: install / reload / enable / disable / uninstall, persisted across restarts | ✅ |
| Orphan reconciliation + watchdog (no leaked background webviews) | ✅ |
| `chrome.scripting.registerContentScripts` / `unregisterContentScripts` / `getRegisteredContentScripts` (dynamic content scripts, MAIN/ISOLATED) | ✅ |
| `web_accessible_resources` served at a custom URI scheme (`chrome.runtime.getURL("inpage.js")` resolves; embedding app must allow the scheme in its CSP) | ✅ |
| Background service worker `"type": "module"` (top-level `import`, relative `import` chunks resolve against the extension root) | ✅ |
| Classic background service worker `importScripts(...)` (synchronous, multi-file; **host CSP must allow `'unsafe-eval'`**) | ✅ |
| `chrome.action`, `chrome.scripting.executeScript`, `chrome.i18n`, `chrome.permissions`, `chrome.management`, `chrome.alarms`, `chrome.offscreen` (BG-survival) | ⚠️ stubs |
| Multiple extensions injecting into the SAME page (one extension identity per page today — first match wins) | ⚠️ limitation |
| `chrome.offscreen` (real document), `chrome.identity`, `chrome.tabs`, `chrome.webRequest`, `chrome.storage.sync` | ❌ out of scope for v1 |
| BG → page provider events (`chainChanged` / `accountsChanged`; needs a `chrome.tabs` router) | ❌ not yet |
| Browser-action popup windows, DevTools panels, auto-update (CRX) | ❌ out of scope for v1 |
| Per-extension process isolation | ❌ (isolated-world approximation in-page) |

## Platforms

**v1 is Windows-only** (WebView2). WKWebView (macOS) and WebKitGTK (Linux)
offer zero native extension hosting, so those backends must be built from
scratch; they will ship later behind the same `runtime::Backend` trait —
stubs exist today and return `PlatformUnsupported`.

## How it works

- **Manifest + matchers** (`src/manifest`, `src/matcher`) — typed MV3 schema
  and faithful reimplementations of Chrome's match-pattern and glob languages
  (reference vectors from Chromium's own unit tests).
- **Runtime backend** (`src/runtime`) — a platform-agnostic `Backend` trait;
  the WebView2 impl injects scripts via page-load hooks and hosts each
  extension's service worker in a hidden 1×1 `WebviewWindow`, bootstrapped
  with an embedded `chrome.*` shim (esbuild IIFE bundles, committed — no JS
  toolchain needed by consumers). The worker itself is loaded from the
  extension's resource origin — `import()` for `"type": "module"` workers, a
  synchronous `importScripts` shim for classic — so top-level `import` /
  `importScripts` resolve against the extension root (see
  `docs/bg-host-service-worker.md`). The background webview shares the host
  app's CSP, so a host loading classic-`importScripts` wallets must allow
  `'unsafe-eval'`, and a host whose extensions fetch remote config on boot must
  widen `connect-src` (the BG can't use the extension's own CSP the way Chrome
  does).
- **Lifecycle service** (`src/lifecycle`) — explicit state machine
  (`Installed → Running → Stopping → Stopped → Uninstalling`) with a single
  owner for every transition, per-extension async serialization, an atomic
  on-disk inventory that survives restarts, event fan-out, and boot-time +
  periodic reaping of orphaned background windows.
- **IPC bus** (`src/ipc`) — in-process `chrome.runtime` message routing
  (DashMap port registry, tokio mpsc/oneshot request-reply).
- **Storage** (`src/storage`) — `chrome.storage.local` persisted as JSON
  under `app_data_dir`, `session` in memory.

See [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) for the subsystem map and
[`docs/DECISIONS.md`](./docs/DECISIONS.md) for the architectural commitments
(and the alternatives that were rejected).

## Repository layout

```
crates/tauri-plugin-extensions/   the plugin crate (publishable)
js-runtime/                       TypeScript chrome.* shim source (esbuild)
examples/minimal-host/            smallest host app + auto-acceptance harness
fixtures/test-extensions/         noop-mv3 fixture (committed); wallets fetched
fixtures/test-dapp/               canary page probing for injected providers
scripts/                          CRX3 fetch scripts (Phantom, MetaMask, Rabby)
docs/                             architecture, decisions, session notes
```

## Development

```powershell
cargo check --workspace
cargo clippy -p tauri-plugin-extensions --all-targets --no-deps -- -D warnings
cargo test -p tauri-plugin-extensions --tests

# JS shim (only when changing js-runtime/; bundles are committed)
cd js-runtime; pnpm install; pnpm build
```

## License

Licensed under the [PolyForm Noncommercial License 1.0.0](./LICENSE.md).

This is a **source-available** license: you may use, modify, and distribute
the software for any **noncommercial** purpose (personal projects, research,
education, evaluation, and use by nonprofit / government organizations) free
of charge. **Commercial use requires a separate commercial license** — contact
the author via the [repository](https://github.com/aurous37-lang/tauri-plugin-extensions)
to obtain one.
