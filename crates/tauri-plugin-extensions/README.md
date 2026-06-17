# tauri-plugin-extensions

Load and run unpacked Chromium MV3 browser extensions inside a Tauri v2 app —
the capability Electron ships natively as `session.loadExtension`, brought to
the Tauri ecosystem.

**Status:** v1 preview. Windows only (WebView2); macOS and Linux are on the
roadmap.

Given an unpacked MV3 extension directory, the plugin parses the manifest,
injects content scripts per the manifest's match rules (including MAIN vs
ISOLATED world), hosts the background service worker in a hidden webview,
routes `chrome.runtime` messages, persists `chrome.storage.{local,session}`,
and manages the full extension lifecycle (install / reload / enable / disable
/ uninstall) with a persistent on-disk inventory and orphan cleanup.

Proven against real extensions: the Phantom wallet loads cleanly with its
Chromium-faithful extension id; MetaMask and Rabby manifests parse today and
are the next acceptance targets.

## Quickstart

```toml
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

Loading is idempotent — repeat calls against the same directory reload in
place. Lifecycle transitions emit `extensions://lifecycle/changed` events,
and the same surface is exposed to frontends as capability-gated plugin
commands (`extensions_load_unpacked`, `extensions_list_lifecycle`,
`extensions_reload`, `extensions_enable`, `extensions_disable`,
`extensions_unload`, `extensions_diagnostics`, …).

> **Host frontend note:** each extension's background worker runs in a hidden
> webview that loads your app origin's root document. If your frontend runs
> boot-time logic, guard it so it no-ops when
> `window.__TAURI_INTERNALS__.metadata.currentWebview.label` starts with
> `ext-bg-`.

## Supported chrome.* surface (v1)

- ✅ `chrome.runtime`: `id`, `getURL`, `getManifest`, `sendMessage`,
  `onMessage`, `connect`, `onConnect`, `onInstalled`, `onStartup` —
  content/host → background round-trips work end-to-end, including async
  `sendResponse` and ports
- ✅ host → extension messaging from Rust (`send_message_to_background`)
- ✅ `chrome.storage.local` / `chrome.storage.session` — full promise +
  callback shapes, `onChanged` fan-out
- ✅ `chrome.scripting.registerContentScripts` + `web_accessible_resources`
  served at the `extres://` resource scheme
- ✅ background service worker `"type": "module"` (top-level `import`) and
  classic `importScripts(...)` — loaded from the extension's resource origin
  so relative specifiers resolve (classic `importScripts` needs host CSP
  `'unsafe-eval'`); see `docs/bg-host-service-worker.md`
- ⚠️ `chrome.action` / `chrome.scripting.executeScript` / `chrome.i18n` /
  `chrome.permissions` / `chrome.management` / `chrome.alarms` /
  `chrome.offscreen` — stubs
- ⚠️ one extension identity per page today (first matching extension wins
  the page's content-script world) — per-extension worlds are post-v1
- ❌ `chrome.offscreen` (real document), `chrome.identity`, `chrome.tabs`,
  `chrome.webRequest`, `chrome.storage.sync`, BG→page provider events,
  browser-action popups, DevTools panels, CRX auto-update, per-extension
  process isolation — out of scope for v1

## Platforms

**Windows only in v1** (WebView2). macOS (WKWebView) and Linux (WebKitGTK)
offer zero native extension hosting; they ship later behind the same
`runtime::Backend` trait — stubs exist today and return
`PlatformUnsupported`.

## More

Full docs, a runnable minimal host example, wallet fetch scripts, and the
architecture / decision log live in the
[repository](https://github.com/aurous37-lang/tauri-plugin-extensions).

## License

Licensed under the [PolyForm Noncommercial License 1.0.0](./LICENSE.md) —
free for any noncommercial use; **commercial use requires a separate
commercial license** from the author (see the
[repository](https://github.com/aurous37-lang/tauri-plugin-extensions)).
