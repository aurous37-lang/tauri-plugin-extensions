# Architecture

This document maps the subsystems of `tauri-plugin-extensions` and how they
interact. Read `DECISIONS.md` first — it resolves the architectural choices
this document assumes.

## Subsystem map

```
┌───────────────────────────────────────────────────────────────────────────┐
│                          Tauri host application                           │
│                                                                           │
│   ┌──────────────────────────┐      ┌────────────────────────────────┐    │
│   │   Main app WebView       │      │   Hidden BG WebView (per ext)  │    │
│   │   (user-facing)          │      │   (visible=false)              │    │
│   │                          │      │                                │    │
│   │   • Page JS              │      │   • background.service_worker  │    │
│   │   • Content scripts      │      │   • chrome.* shim (BG flavor)  │    │
│   │   • chrome.* shim        │      │                                │    │
│   │     (isolated + main)    │      │                                │    │
│   └────────────┬─────────────┘      └───────────────┬────────────────┘    │
│                │                                    │                     │
│                │        Tauri IPC (invoke)          │                     │
│                └──────────────┬─────────────────────┘                     │
│                               │                                           │
│   ┌───────────────────────────▼──────────────────────────────────────┐    │
│   │         Rust plugin crate (tauri-plugin-extensions)              │    │
│   │                                                                  │    │
│   │   ┌─────────────┐   ┌──────────────┐   ┌──────────────────┐      │    │
│   │   │  manifest   │   │   matcher    │   │    runtime       │      │    │
│   │   │   parser    │   │  (URL/glob)  │   │  (Backend trait) │      │    │
│   │   └──────┬──────┘   └──────┬───────┘   └─────────┬────────┘      │    │
│   │          │                 │                     │               │    │
│   │   ┌──────▼─────────────────▼─────────────────────▼────────┐      │    │
│   │   │                  ExtensionRegistry                    │      │    │
│   │   │   (loaded extensions, script-injection rules, id→BG)  │      │    │
│   │   └──────────────────────────┬────────────────────────────┘      │    │
│   │                              │                                   │    │
│   │   ┌──────────────────────────▼────────────────────────────┐      │    │
│   │   │                     IPC message bus                   │      │    │
│   │   │   (routes chrome.runtime messages between surfaces)   │      │    │
│   │   └───────────────────────────────────────────────────────┘      │    │
│   └──────────────────────────────┬───────────────────────────────────┘    │
│                                  │                                        │
│                                  ▼                                        │
│                          ┌──────────────┐                                 │
│                          │   Storage    │    (chrome.storage.local/       │
│                          │   backend    │     session — JSON files on     │
│                          │              │     disk under app_data_dir)    │
│                          └──────────────┘                                 │
└───────────────────────────────────────────────────────────────────────────┘
```

## Subsystem A — Manifest parsing

Path: `crates/tauri-plugin-extensions/src/manifest/`

Parses Chrome MV3 `manifest.json` into typed Rust structs. Rejects MV2
with a clear error. Owns the schema for the subset we honor in v1.

Not the MV3 spec's full schema — scope limited to what the three acceptance
extensions (Phantom, MetaMask, Rabby) actually declare. Fields added on
demand when a target extension trips on a missing one.

## Subsystem B — URL and glob matchers

Path: `crates/tauri-plugin-extensions/src/matcher/`

Faithful implementation of Chrome's two pattern languages:

- **Match patterns** (used by `content_scripts.matches`, `host_permissions`,
  `web_accessible_resources.matches`): `<scheme>://<host>/<path>` with
  restricted wildcards, plus the special `<all_urls>`.
- **Globs** (used by `content_scripts.include_globs` / `exclude_globs`):
  shell-style globbing.

Reference: [Chrome docs: Match patterns](https://developer.chrome.com/docs/extensions/develop/concepts/match-patterns).

## Subsystem C — Runtime backend (Windows: WebView2)

Path: `crates/tauri-plugin-extensions/src/runtime/`

`Backend` trait abstracts the webview-specific work of:

- Injecting scripts at `document_start` / `document_end` / `document_idle`
- Creating an isolated world for content scripts
- Hosting the hidden background webview

Windows impl (`webview2.rs`) uses wry's hooks through Tauri's plugin API.
macOS / Linux stubs exist from day one but return `PlatformUnsupported`.

## Subsystem D — Extension registry

Path: `crates/tauri-plugin-extensions/src/registry.rs`

In-memory registry of loaded extensions, keyed by stable extension id.
Each entry holds the parsed manifest, the compiled match rules, the
handle to the hidden BG webview, and any open chrome.runtime ports.

## Subsystem E — IPC bus

Path: `crates/tauri-plugin-extensions/src/ipc/`

Routes `chrome.runtime.sendMessage` / `onMessage` / `connect` / `onConnect`
traffic between content scripts, background, and (future) popup. Built on
a dashmap of `PortId → mpsc::Sender<Message>`. No cross-process concerns;
everything runs in the host process.

## Subsystem F — Storage

Path: `crates/tauri-plugin-extensions/src/storage/`

Implements `chrome.storage.local` and `chrome.storage.session`. Local
persists to `app_data_dir/extensions/<ext_id>/storage-local.json`. Session
is in-memory only and cleared on extension unload.

`chrome.storage.sync` is explicitly out of scope for v1 — there is no
Google-account cloud to sync against, and wallets do not rely on it.

## JS runtime (js-runtime/)

TypeScript project that builds two bundles via esbuild:

- `content-bootstrap.js` — injected into page/content-script worlds; sets
  up `window.chrome` / `chrome` globals that proxy to the host via
  `window.__TAURI_INTERNALS__.invoke`.
- `background-bootstrap.js` — runs in the hidden BG webview; evaluates
  the extension's `background.service_worker` module and wires up the
  BG-flavored `chrome.*` shim.

Bundles are built to `crates/tauri-plugin-extensions/embedded-js/` and
embedded into the Rust crate via `include_str!`.

## Message flow example — content script → background

1. Page loads; `Backend::before_navigate` injects `content-bootstrap.js` at
   `document_start` in the isolated world.
2. Bootstrap calls `invoke('extensions:content_ready', {extId, frameId})`.
3. Registry injects each matching content script into the isolated world.
4. Extension content script calls `chrome.runtime.sendMessage(msg)`.
5. Shim calls `invoke('extensions:runtime_send_message', {extId, msg})`.
6. Rust IPC bus forwards to the hidden BG webview via its
   `WebviewWindow::eval("__ext.onMessage(…)")`.
7. BG's `chrome.runtime.onMessage` listener fires; its `sendResponse`
   travels the reverse path.

## Out of scope for v1

- `chrome.tabs` — we don't expose a tab abstraction to host apps yet.
- `chrome.webRequest` — blocks on intercepting network traffic at the
  wry / CEF layer; not plumbed yet.
- Popup windows rendered in a separate OS window (browser-action popups).
- Extension auto-update (the CRX update protocol).
- `chrome.storage.sync`.
- Per-extension process isolation.
- DevTools panel integration.

See `engineering-summary.md` for the rationale behind each exclusion.
