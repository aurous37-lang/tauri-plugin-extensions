//! Tauri v2 plugin: load and run Chromium MV3 browser extensions inside a
//! Tauri webview.
//!
//! Top-level entry is [`init`], which returns a `tauri::plugin::TauriPlugin`
//! the host application adds via `tauri::Builder::plugin`.
//!
//! See `docs/ARCHITECTURE.md` for the subsystem map.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod ipc;
pub mod lifecycle;
pub mod loader;
pub mod manifest;
pub mod matcher;
pub mod registry;
pub mod runtime;
pub mod storage;

pub use error::{Error, Result};
pub use registry::ExtensionId;

use std::{collections::HashMap, sync::Arc};

use tauri::{
    plugin::{Builder as PluginBuilder, TauriPlugin},
    Emitter, Manager, Runtime,
};

use crate::{
    ipc::{bus::Bus, router, Router},
    lifecycle::{Diagnostics, LifecycleEvent, LifecycleManager, LifecycleSummary, EVENT_NAME},
    matcher::MatchPatternSet,
    registry::{ExtensionRegistry, ExtensionSummary},
    runtime::{
        dynamic_scripts::{DynamicScriptStore, RegisteredScript},
        Backend, RunAt, World,
    },
    storage::{LocalStorageManager, SessionStorageManager, StorageArea},
};

/// Name registered with Tauri — host apps reference this in `tauri.conf.json`
/// permissions and in command invocations.
pub const PLUGIN_NAME: &str = "extensions";

// The backend is owned by [`LifecycleManager`]; there is no longer a
// separate `BackendState` Tauri-state entry. `Backend` is imported above
// only for the `default_backend` return type.
#[allow(dead_code)]
fn _backend_import_witness() -> Option<Arc<dyn Backend>> {
    None
}

/// Resolve the root directory for `LocalStorage` JSON files. Uses the Tauri
/// `PathResolver` when available; falls back to the system temp dir. Kept as
/// a standalone function so the `.setup()` closure stays readable.
fn resolve_local_storage_root<R: Runtime>(app: &tauri::AppHandle<R>) -> std::path::PathBuf {
    match app.path().app_data_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "app_data_dir unavailable; falling back to temp");
            std::env::temp_dir().join("tauri-plugin-extensions")
        }
    }
}

/// Register the extension-resource URI scheme on the plugin builder.
///
/// Serves extension files at `<scheme>://.../<ext-id>/<path>` (on Windows:
/// `http://extres.localhost/<ext-id>/<path>`). Two privilege levels, keyed off
/// the requesting webview (D-008):
///
/// - **The extension's own background webview** (`ctx.webview_label()` ==
///   `ext-bg-<id>`) is the extension's privileged origin — like a Chrome
///   service worker, it may read ANY packaged file. This is what lets the BG
///   load its own service worker + `import`/`importScripts` chunks (which are
///   not `web_accessible_resources`) from this scheme.
/// - **Every other webview** (a dapp page, the host frontend) is served only
///   `web_accessible_resources`, exactly as Chrome gates `chrome-extension://`
///   reads from web pages. Rabby's `pageProvider.js` injects this way.
///
/// Root + WAR globs resolve through the synchronously-maintained
/// [`runtime::resources::ResourceRegistry`] (populated before the BG spawns, so
/// a booting worker never races the async [`ExtensionRegistry`] projection),
/// falling back to the registry. A second canonicalize + `starts_with(root)`
/// check is defense-in-depth on top of the lexical traversal guard.
///
/// Note: the embedding app's CSP must allow scripts from the resource scheme
/// origin (Chrome exempts `chrome-extension:` from page CSP via a privilege we
/// cannot replicate without a header-rewrite layer). Classic-`importScripts`
/// workers additionally need `'unsafe-eval'`. See README / minimal-host
/// `tauri.conf.json`.
fn attach_resource_scheme<R: Runtime>(builder: PluginBuilder<R>) -> PluginBuilder<R> {
    use tauri::http::{header, Response, StatusCode};

    builder.register_uri_scheme_protocol(runtime::resources::RESOURCE_SCHEME, |ctx, request| {
        let err = |status: StatusCode, msg: &str| {
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .body(msg.as_bytes().to_vec())
                .unwrap_or_else(|_| Response::new(Vec::new()))
        };

        let path = request.uri().path().to_string();
        let requesting_label = ctx.webview_label().to_string();
        let app = ctx.app_handle();

        // Shared "canonicalize-under-root then read" tail for both branches.
        let read_under_root = |source_dir: std::path::PathBuf, resolved: std::path::PathBuf| {
            let under_root = match (
                std::fs::canonicalize(&source_dir),
                std::fs::canonicalize(&resolved),
            ) {
                (Ok(root), Ok(file)) => file.starts_with(&root),
                _ => false,
            };
            if !under_root {
                return err(StatusCode::NOT_FOUND, "resource not found");
            }
            match std::fs::read(&resolved) {
                Ok(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .header(
                        header::CONTENT_TYPE,
                        runtime::resources::content_type_for(&resolved),
                    )
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .body(bytes)
                    .unwrap_or_else(|_| Response::new(Vec::new())),
                Err(_) => err(StatusCode::NOT_FOUND, "resource not found"),
            }
        };

        // Branch 1 — the request came from an extension's own background webview
        // (identified by its `ext-bg-*` label, not the URL). This is the
        // extension's privileged origin: it may read ANY packaged file, and its
        // worker may reference files origin-absolute (`/background.js`) — those
        // resolve against the extension root, not the shared scheme origin.
        if requesting_label.starts_with("ext-bg-") {
            if let Some((ext_id, root)) = app
                .try_state::<runtime::resources::ResourceRegistry>()
                .and_then(|rr| rr.find_by_bg_label(&requesting_label))
            {
                let rel = runtime::resources::bg_request_rel(ext_id.as_str(), &path);
                let resolved =
                    match runtime::resources::resolve_resource_privileged(&root.source_dir, rel) {
                        Ok(p) => p,
                        Err(_) => return err(StatusCode::FORBIDDEN, "resource not accessible"),
                    };
                return read_under_root(root.source_dir, resolved);
            }
            // BG label with no known root (race / unknown) — fall through to the
            // page path, which will 404 on a malformed/absolute worker path.
        }

        // Branch 2 — any other webview (a dapp page, the host frontend). Parse
        // `/<id>/<rel>` and serve only `web_accessible_resources`.
        let (id, rel) = match runtime::resources::split_request_path(&path) {
            Ok(v) => v,
            Err(_) => return err(StatusCode::BAD_REQUEST, "malformed extension-resource path"),
        };
        let ext_id = ExtensionId::new(id);

        // Resolve the on-disk root + WAR globs. Prefer the resource registry
        // (populated before BG spawn); fall back to the read-only projection.
        let (source_dir, war_globs) = if let Some(root) = app
            .try_state::<runtime::resources::ResourceRegistry>()
            .and_then(|rr| rr.get(&ext_id))
        {
            (root.source_dir, root.war_globs)
        } else if let Some(loaded) = app
            .try_state::<ExtensionRegistry>()
            .and_then(|registry| registry.get(&ext_id))
        {
            let war = loaded
                .manifest
                .web_accessible_resources
                .iter()
                .flat_map(|w| w.resources.clone())
                .collect();
            (loaded.source_dir.clone(), war)
        } else {
            return err(StatusCode::NOT_FOUND, "extension not loaded");
        };

        let resolved = match runtime::resources::resolve_resource(&source_dir, &war_globs, &rel) {
            Ok(p) => p,
            Err(_) => return err(StatusCode::FORBIDDEN, "resource not accessible"),
        };
        read_under_root(source_dir, resolved)
    })
}

/// Initialize the plugin. Add via `tauri::Builder::plugin(init())`.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    // Content-script injection is driven by Tauri's plugin-level
    // `on_page_load` callback, which must be attached on the builder
    // before `.build()`. `register_hooks(app)` runs later inside the
    // `.setup()` closure below to install the per-frame bootstrap
    // tracker that the on_page_load closure consults.
    let builder = PluginBuilder::new(PLUGIN_NAME);
    let builder = runtime::injection::attach_on_page_load(builder);
    let builder = attach_resource_scheme(builder);
    builder
        .setup(|app, _api| {
            app.manage(ExtensionRegistry::new());
            app.manage(Bus::new());
            app.manage(Router::new());
            app.manage(SessionStorageManager::new());
            app.manage(DynamicScriptStore::new());
            app.manage(runtime::resources::ResourceRegistry::new());
            let local_root = resolve_local_storage_root(app);
            app.manage(LocalStorageManager::new(&local_root));

            // Select a backend. On non-Windows (stub runtimes) or if the
            // Windows constructor errors, downgrade to `None`; the lifecycle
            // manager treats this as "extension runs without a background
            // worker" and keeps going.
            let backend = match runtime::default_backend(&app.app_handle().clone()) {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::warn!(error = %e, "no runtime backend; BG workers will not spawn");
                    None
                }
            };
            // Wire the lifecycle manager. Persistence lives under
            // app_data_dir/extensions/state.json so the installed set
            // survives restarts.
            let store = Arc::new(lifecycle::FsStateStore::under_app_data(&local_root));
            let manager = Arc::new(LifecycleManager::new(
                app.app_handle().clone(),
                backend,
                store,
            ));
            app.manage(Arc::clone(&manager));

            // Boot sequence:
            //   1. Reconcile orphan `ext-bg-*` windows — closes zombies
            //      left over from pre-lifecycle dev runs.
            //   2. Restore every persisted enabled extension via
            //      install_or_reload.
            let boot_manager = Arc::clone(&manager);
            tauri::async_runtime::spawn(async move {
                match boot_manager.reconcile_orphans().await {
                    Ok(n) if n > 0 => {
                        tracing::info!(count = n, "closed orphan BG windows at boot")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "orphan reconciliation failed"),
                }
                match boot_manager.boot_restore().await {
                    Ok(n) if n > 0 => tracing::info!(count = n, "restored persisted extensions"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "boot restore failed"),
                }
            });

            // Content-script injection tracker. The on_page_load closure
            // (attached on the plugin builder above) consults this to
            // dedupe `content-bootstrap.js` injection across the
            // Started → Finished phases of a single page load.
            if let Err(e) = runtime::injection::register_hooks(app.app_handle()) {
                tracing::warn!(error = %e, "register_hooks failed; content scripts may not inject");
            }

            // Periodic watchdog — reconciles orphan `ext-bg-*` windows and
            // checks every lifecycle invariant on a 5-minute cadence. Emits
            // a `WatchdogAlert` event only when something is wrong; a clean
            // sweep is silent.
            let watchdog_manager = Arc::clone(&manager);
            let watchdog_app = app.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(300));
                // Skip the immediate first tick — boot_restore + initial
                // reconcile_orphans above have already covered t=0.
                interval.tick().await;
                loop {
                    interval.tick().await;
                    let reaped = match watchdog_manager.reconcile_orphans().await {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::warn!(error = %e, "watchdog reconcile failed");
                            continue;
                        }
                    };
                    watchdog_manager.bump_watchdog_counter();

                    if reaped > 0 {
                        tracing::error!(
                            count = reaped,
                            "watchdog reaped orphan ext-bg windows — something is leaking"
                        );
                    }

                    let violations = watchdog_manager.invariants().await;
                    if !violations.is_empty() {
                        tracing::error!(
                            ?violations,
                            "watchdog invariant violations detected"
                        );
                    }

                    if reaped > 0 || !violations.is_empty() {
                        let ev = LifecycleEvent::WatchdogAlert { reaped, violations };
                        if let Err(e) = watchdog_app.emit(EVENT_NAME, ev) {
                            tracing::warn!(
                                error = %e,
                                "watchdog alert emit failed"
                            );
                        }
                    }
                }
            });

            tracing::info!(plugin = PLUGIN_NAME, "initialized");
            Ok(())
        })
        .on_event(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Graceful shutdown — every Running extension transitions
                // to Stopped { Shutdown } and its BG webview closes. Blocks
                // exit briefly to avoid leaking WebView2 handles.
                if let Some(manager) = app.try_state::<Arc<LifecycleManager<R>>>() {
                    let mgr = Arc::clone(&*manager);
                    tauri::async_runtime::block_on(async move {
                        if let Err(e) = mgr.shutdown_all().await {
                            tracing::warn!(error = %e, "shutdown_all errored");
                        }
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            extensions_load_unpacked,
            extensions_unload,
            extensions_list,
            extensions_list_lifecycle,
            extensions_reload,
            extensions_enable,
            extensions_disable,
            extensions_reconcile_orphans,
            extensions_diagnostics,
            extensions_content_ready,
            extensions_scripting_register_content_scripts,
            extensions_scripting_unregister_content_scripts,
            extensions_scripting_get_registered_content_scripts,
            extensions_runtime_send_message,
            extensions_runtime_connect,
            extensions_runtime_port_post,
            extensions_runtime_port_disconnect,
            extensions_storage_get,
            extensions_storage_set,
            extensions_storage_remove,
            extensions_storage_clear,
        ])
        .build()
}

/// Convenience: load an unpacked MV3 extension from a directory on disk.
/// Idempotent — calling twice against the same path reloads the existing
/// extension rather than duplicating it. See
/// [`lifecycle::LifecycleManager::install_or_reload`] for the semantics.
pub async fn load_unpacked<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &std::path::Path,
) -> Result<ExtensionId> {
    let manager = app.state::<Arc<LifecycleManager<R>>>();
    manager.install_or_reload(path).await
}

/// Host-facing API: send a `chrome.runtime` message to an extension's
/// background worker and await its `sendResponse`. The payload arrives at
/// the BG's `chrome.runtime.onMessage` listeners exactly like a message
/// from one of the extension's own surfaces, with a sender id of `"host"`.
///
/// Errors when the extension has no live background webview or the worker
/// doesn't respond within [`ipc::router::SEND_MESSAGE_TIMEOUT`].
pub async fn send_message_to_background<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    extension_id: &ExtensionId,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let router = app.state::<Router>();
    router::send_to_background(
        app,
        &router,
        extension_id,
        payload,
        serde_json::json!({ "id": "host" }),
    )
    .await
}

/// Embedded content-world bootstrap — injected at `document_start` into
/// every matched frame. Replaced at build time by `js-runtime/`.
#[allow(dead_code)] // Consumed by Agent D (background runtime) when it wires the Backend.
pub(crate) const CONTENT_BOOTSTRAP_JS: &str =
    include_str!("../embedded-js/content-bootstrap.js");

/// Embedded background-world bootstrap — evaluated inside the hidden
/// per-extension webview. Replaced at build time by `js-runtime/`.
#[allow(dead_code)] // Consumed by Agent D (background runtime) when it wires the Backend.
pub(crate) const BACKGROUND_BOOTSTRAP_JS: &str =
    include_str!("../embedded-js/background-bootstrap.js");

// ---------------------------------------------------------------------------
// Tauri commands. Each is thin — the real work lives in loader / bus / storage.
// ---------------------------------------------------------------------------

// Each command takes `app: AppHandle<R>` so Tauri's command-handler macro
// can infer R; the manager resolves via `app.state::<Arc<LifecycleManager<R>>>()`.
// (Taking `State<'_, Arc<LifecycleManager<R>>>` directly trips E0283 because
// the macro has no other anchor for R.)

fn manager_state<R: Runtime>(
    app: &tauri::AppHandle<R>,
) -> Arc<LifecycleManager<R>> {
    Arc::clone(&*app.state::<Arc<LifecycleManager<R>>>())
}

#[tauri::command]
async fn extensions_load_unpacked<R: Runtime>(
    app: tauri::AppHandle<R>,
    path: String,
) -> Result<ExtensionId> {
    let path = std::path::PathBuf::from(path);
    manager_state(&app).install_or_reload(&path).await
}

#[tauri::command]
async fn extensions_unload<R: Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
) -> Result<()> {
    let id = ExtensionId::new(id);
    manager_state(&app).uninstall(&id).await?;
    // Session is discarded; local persists on disk but we drop the in-memory
    // handle so a subsequent reload re-reads from disk cleanly.
    app.state::<SessionStorageManager>().drop_extension(&id);
    app.state::<LocalStorageManager>().drop_extension(&id);
    // Drop any dynamic content-script registrations — a reinstall re-registers
    // from the background worker's init.
    app.state::<DynamicScriptStore>().drop_extension(&id);
    Ok(())
}

#[tauri::command]
async fn extensions_list<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Vec<ExtensionSummary>> {
    Ok(manager_state(&app).legacy_registry_summaries().await)
}

/// Rich lifecycle view — state machine info per extension. Host frontends
/// that care about reload/enable/disable states use this; simple lists
/// should use `extensions_list`.
#[tauri::command]
async fn extensions_list_lifecycle<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Vec<LifecycleSummary>> {
    Ok(manager_state(&app).list().await)
}

/// Reload an installed extension — atomic stop + start, emits a single
/// `Reloaded` event.
#[tauri::command]
async fn extensions_reload<R: Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
) -> Result<()> {
    manager_state(&app).reload(&ExtensionId::new(id)).await
}

/// Enable a disabled extension — starts the BG worker if one was declared.
#[tauri::command]
async fn extensions_enable<R: Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
) -> Result<()> {
    manager_state(&app).enable(&ExtensionId::new(id)).await
}

/// Disable an enabled extension — stops the BG worker but keeps the entry
/// installed.
#[tauri::command]
async fn extensions_disable<R: Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
) -> Result<()> {
    manager_state(&app).disable(&ExtensionId::new(id)).await
}

/// Close any `ext-bg-*` window that no currently-Running extension owns.
/// Primarily for debugging — the lifecycle manager runs this at boot
/// automatically.
#[tauri::command]
async fn extensions_reconcile_orphans<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<usize> {
    manager_state(&app).reconcile_orphans().await
}

/// Lifecycle-service diagnostics — counts, invariants, orphan signal.
/// The minimal host uses this to assert that the zombie-window bug
/// cannot regress, and the watchdog emits a similar payload when
/// something is wrong.
#[tauri::command]
async fn extensions_diagnostics<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Diagnostics> {
    Ok(manager_state(&app).diagnostics().await)
}

// ---------------------------------------------------------------------------
// chrome.runtime message + port routing. Command names and payload shapes
// match what the JS shim invokes (js-runtime/src/shared/types.ts →
// RuntimeCommands; bundled into embedded-js/*.js). Delivery to the receiving
// surface goes through ipc::router — see its module docs for the flow.
// ---------------------------------------------------------------------------

/// `chrome.runtime.sendMessage`, both directions of it:
///
/// - **Request** (`phase` absent): `{from?, to?, extensionId, frameId?,
///   payload}` — routes to the target surface and awaits its
///   `sendResponse`. Returns the shim's `SendMessageResult` shape
///   `{ok, response?, error?}`.
/// - **Response** (`phase: "response"`): `{requestId, response}` — resolves
///   the pending request the receiving surface is answering.
///
/// v1 routes content/host → background only. Background → content is not a
/// path Chrome itself serves through `runtime.sendMessage` (that's
/// `tabs.sendMessage`, out of scope per ARCHITECTURE.md), so it reports
/// "receiving end does not exist" just like Chrome would.
/// Build a synthetic `sender.tab` for a content-script-originated message/port.
/// Chrome gives content-script senders a `tab`; wallet background controllers
/// read `port.sender.tab.id` (and `.tab.id` on messages) to track and route the
/// connection — a missing tab strands the provider handshake. Returns `null`
/// for the extension's own surfaces (background / the host's main window),
/// which have no tab in Chrome either. The id is derived stably from the webview
/// label so the same content webview always maps to the same tab id, and the
/// reverse mapping (id → label) lets a future `chrome.tabs.sendMessage` route
/// back. Only content/page webviews (not `ext-bg-*`, not `main`) get a tab.
fn synthetic_tab<R: Runtime>(webview: &tauri::Webview<R>) -> serde_json::Value {
    let Some(id) = content_tab_id(webview.label()) else {
        return serde_json::Value::Null;
    };
    let url = webview.url().map(|u| u.to_string()).ok();
    serde_json::json!({
        "id": id, "url": url, "active": true, "windowId": 1, "index": 0,
        "highlighted": true, "incognito": false, "pinned": false, "frameId": 0,
    })
}

/// Stable, positive tab id for a content/page webview label, or `None` for the
/// extension's own surfaces (`ext-bg-*`) and the host's `main` window, which
/// have no tab in Chrome. Pure + deterministic so the same content webview
/// always maps to the same id (the basis for a future `chrome.tabs.sendMessage`
/// reverse route).
fn content_tab_id(label: &str) -> Option<i64> {
    if label.starts_with("ext-bg-") || label == "main" || label.is_empty() {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    label.hash(&mut h);
    // Positive 31-bit id: JS-safe and shaped like Chrome's small tab ids.
    Some((h.finish() & 0x7fff_ffff) as i64)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn extensions_runtime_send_message<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    router: tauri::State<'_, Router>,
    phase: Option<String>,
    request_id: Option<String>,
    response: Option<serde_json::Value>,
    to: Option<String>,
    extension_id: Option<String>,
    frame_id: Option<u32>,
    payload: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    use serde_json::json;

    if phase.as_deref() == Some("response") {
        let Some(request_id) = request_id else {
            return Err(Error::Ipc("response phase requires requestId".into()));
        };
        let delivered =
            router.resolve_pending(&request_id, response.unwrap_or(serde_json::Value::Null));
        if !delivered {
            tracing::debug!(request = %request_id, "late or duplicate sendResponse dropped");
        }
        return Ok(json!({ "ok": true }));
    }

    let Some(extension_id) = extension_id else {
        return Err(Error::Ipc("sendMessage requires extensionId".into()));
    };
    let ext = ExtensionId::new(extension_id);

    // Only the background surface receives runtime.sendMessage in v1; a BG
    // sender targeting "content" gets Chrome's no-receiver behavior.
    if to.as_deref() == Some("content") {
        return Ok(json!({
            "ok": false,
            "error": "Could not establish connection. Receiving end does not exist.",
        }));
    }

    let sender = json!({
        "id": ext.as_str(),
        "url": webview.url().map(|u| u.to_string()).ok(),
        "frameId": frame_id.unwrap_or(0),
        "tab": synthetic_tab(&webview),
    });

    match router::send_to_background(
        &app,
        &router,
        &ext,
        payload.unwrap_or(serde_json::Value::Null),
        sender,
    )
    .await
    {
        Ok(response) => Ok(json!({ "ok": true, "response": response })),
        Err(e) => Ok(json!({ "ok": false, "error": e.to_string() })),
    }
}

// ---------------------------------------------------------------------------
// chrome.scripting.registerContentScripts surface. Wallets (Phantom's EVM
// provider, Rabby) register inpage scripts at runtime from the background
// service worker rather than declaring them in the manifest. These commands
// back the shim's chrome.scripting.* methods; the dynamic registrations are
// merged into the on_page_load injection flow via DynamicScriptStore.
// ---------------------------------------------------------------------------

/// One `chrome.scripting.RegisteredContentScript` as the shim sends it (camelCase).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterScriptInput {
    id: String,
    #[serde(default)]
    matches: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
    /// CSS files — parsed for fidelity but not injected (no CSS support yet).
    #[serde(default)]
    #[allow(dead_code)]
    css: Vec<String>,
    #[serde(default)]
    run_at: Option<String>,
    #[serde(default)]
    world: Option<String>,
    #[serde(default)]
    all_frames: bool,
    #[serde(default)]
    persist_across_sessions: Option<bool>,
}

/// `chrome.scripting.RegisteredContentScript` as returned by getRegistered.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisteredScriptOutput {
    id: String,
    matches: Vec<String>,
    js: Vec<String>,
    run_at: String,
    world: String,
    all_frames: bool,
    persist_across_sessions: bool,
}

fn parse_scripting_world(world: Option<&str>) -> World {
    match world.map(str::to_ascii_uppercase).as_deref() {
        Some("MAIN") => World::Main,
        _ => World::Isolated,
    }
}

fn parse_scripting_run_at(run_at: Option<&str>) -> RunAt {
    match run_at {
        Some("document_start") => RunAt::DocumentStart,
        Some("document_end") => RunAt::DocumentEnd,
        // chrome.scripting's default run_at is document_idle.
        _ => RunAt::DocumentIdle,
    }
}

fn run_at_str(run_at: RunAt) -> &'static str {
    match run_at {
        RunAt::DocumentStart => "document_start",
        RunAt::DocumentEnd => "document_end",
        RunAt::DocumentIdle => "document_idle",
    }
}

/// `chrome.scripting.registerContentScripts(scripts)`.
#[tauri::command]
async fn extensions_scripting_register_content_scripts<R: Runtime>(
    app: tauri::AppHandle<R>,
    extension_id: String,
    scripts: Vec<RegisterScriptInput>,
) -> Result<()> {
    let ext = ExtensionId::new(extension_id);
    // Resolve the extension's on-disk root. Prefer the read-only registry
    // (lock-free), but fall back to the lifecycle manager — a worker can call
    // registerContentScripts during its boot, before the registry projection
    // catches up, and the manager already holds the entry by then.
    let source_dir = match app.state::<ExtensionRegistry>().get(&ext) {
        Some(e) => e.source_dir.clone(),
        None => manager_state(&app)
            .source_dir(&ext)
            .await
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "registerContentScripts: extension '{ext}' is not loaded"
                ))
            })?,
    };

    let mut compiled = Vec::with_capacity(scripts.len());
    for s in scripts {
        let matches = MatchPatternSet::parse_many(&s.matches)?;
        compiled.push(RegisteredScript {
            id: s.id,
            source_dir: source_dir.clone(),
            matches,
            match_strings: s.matches,
            js_files: s.js.into_iter().map(std::path::PathBuf::from).collect(),
            run_at: parse_scripting_run_at(s.run_at.as_deref()),
            world: parse_scripting_world(s.world.as_deref()),
            all_frames: s.all_frames,
            persist_across_sessions: s.persist_across_sessions.unwrap_or(false),
        });
    }
    app.state::<DynamicScriptStore>().register(&ext, compiled)
}

/// `chrome.scripting.unregisterContentScripts(filter?)`.
#[tauri::command]
async fn extensions_scripting_unregister_content_scripts<R: Runtime>(
    app: tauri::AppHandle<R>,
    extension_id: String,
    ids: Option<Vec<String>>,
) -> Result<()> {
    let ext = ExtensionId::new(extension_id);
    app.state::<DynamicScriptStore>()
        .unregister(&ext, ids.as_deref());
    Ok(())
}

/// `chrome.scripting.getRegisteredContentScripts(filter?)`.
#[tauri::command]
async fn extensions_scripting_get_registered_content_scripts<R: Runtime>(
    app: tauri::AppHandle<R>,
    extension_id: String,
    ids: Option<Vec<String>>,
) -> Result<Vec<RegisteredScriptOutput>> {
    let ext = ExtensionId::new(extension_id);
    let scripts = app
        .state::<DynamicScriptStore>()
        .get_registered(&ext, ids.as_deref());
    Ok(scripts
        .into_iter()
        .map(|s| RegisteredScriptOutput {
            id: s.id,
            matches: s.match_strings,
            js: s
                .js_files
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            run_at: run_at_str(s.run_at).to_string(),
            world: match s.world {
                World::Main => "MAIN",
                World::Isolated => "ISOLATED",
            }
            .to_string(),
            all_frames: s.all_frames,
            persist_across_sessions: s.persist_across_sessions,
        })
        .collect())
}

/// Content surface ready. The JS shim calls this from `__tauri_ext_configure`
/// once it finishes installing `globalThis.chrome`.
#[tauri::command]
async fn extensions_content_ready(
    extension_id: String,
    frame_id: Option<u32>,
) -> Result<()> {
    tracing::debug!(
        extension = %extension_id,
        frame = frame_id.unwrap_or(0),
        "content surface ready"
    );
    Ok(())
}

/// `chrome.runtime.connect` — opens a port. The JS shim mints the `portId`
/// (both ends share it) and passes `{portId, from, to, extensionId, name}`.
/// The router records the route and announces the connect to the target
/// surface, whose shim materializes the peer `Port`.
#[tauri::command]
async fn extensions_runtime_connect<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    router: tauri::State<'_, Router>,
    port_id: String,
    to: Option<String>,
    extension_id: String,
    name: Option<String>,
) -> Result<String> {
    use serde_json::json;

    if to.as_deref() == Some("content") {
        // BG → content connect needs a tab abstraction (chrome.tabs); v1
        // matches Chrome's no-receiver behavior for runtime.connect too.
        return Err(Error::Ipc(
            "Could not establish connection. Receiving end does not exist.".into(),
        ));
    }

    let ext = ExtensionId::new(extension_id);
    let target_label = runtime::bg_window_label(&ext);
    if app.get_webview_window(&target_label).is_none() {
        return Err(Error::Ipc(format!(
            "extension '{ext}' has no background webview — receiving end does not exist"
        )));
    }

    let name = name.unwrap_or_default();
    router.register_port(
        port_id.clone(),
        ipc::PortRoute {
            extension_id: ext.clone(),
            opener_label: webview.label().to_string(),
            target_label: target_label.clone(),
            name: name.clone(),
        },
    );

    let announce = json!({
        "portId": port_id,
        "extensionId": ext.as_str(),
        "name": name,
        "sender": {
            "id": ext.as_str(),
            "url": webview.url().map(|u| u.to_string()).ok(),
            "tab": synthetic_tab(&webview),
        },
    });
    router::dispatch(&app, &target_label, router::EVENT_INBOUND_CONNECT, &announce)?;
    Ok(port_id)
}

/// `port.postMessage` — fire-and-forget delivery to the port's other end.
/// Which end is "other" is decided by the calling webview's label.
#[tauri::command]
async fn extensions_runtime_port_post<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    router: tauri::State<'_, Router>,
    port_id: String,
    payload: serde_json::Value,
) -> Result<()> {
    use serde_json::json;

    let Some(route) = router.port_route(&port_id) else {
        return Err(Error::Ipc(format!("unknown port: {port_id}")));
    };
    let peer = Router::peer_label(&route, webview.label());
    router::dispatch(
        &app,
        &peer,
        router::EVENT_PORT_MESSAGE,
        &json!({ "portId": port_id, "payload": payload }),
    )
}

/// `port.disconnect` — removes the route and notifies the peer end.
#[tauri::command]
async fn extensions_runtime_port_disconnect<R: Runtime>(
    app: tauri::AppHandle<R>,
    webview: tauri::Webview<R>,
    router: tauri::State<'_, Router>,
    port_id: String,
) -> Result<()> {
    use serde_json::json;

    if let Some(route) = router.remove_port(&port_id) {
        let peer = Router::peer_label(&route, webview.label());
        // Best-effort: the peer webview may already be gone (BG reload).
        if let Err(e) = router::dispatch(
            &app,
            &peer,
            router::EVENT_PORT_DISCONNECT,
            &json!({ "portId": port_id }),
        ) {
            tracing::debug!(port = %port_id, error = %e, "peer disconnect notify failed");
        }
    }
    Ok(())
}

#[tauri::command]
async fn extensions_storage_get(
    local: tauri::State<'_, LocalStorageManager>,
    session: tauri::State<'_, SessionStorageManager>,
    extension_id: String,
    area: StorageArea,
    keys: Option<Vec<String>>,
) -> Result<HashMap<String, serde_json::Value>> {
    let ext = ExtensionId::new(extension_id);
    match area {
        StorageArea::Local => local.for_extension(&ext).get_many(keys.as_deref()).await,
        StorageArea::Session => Ok(session.for_extension(&ext).get_many(keys.as_deref())),
    }
}

// Arg is named `items` to match what the baked JS shim sends
// (js-runtime/src/shared/storage.ts) — chrome.storage.set's own parameter
// name. A mismatch here strands BG workers mid-write: the shim's promise
// rejects, the worker's sendResponse never fires, and the sender times out.
#[tauri::command]
async fn extensions_storage_set(
    local: tauri::State<'_, LocalStorageManager>,
    session: tauri::State<'_, SessionStorageManager>,
    extension_id: String,
    area: StorageArea,
    items: HashMap<String, serde_json::Value>,
) -> Result<()> {
    let ext = ExtensionId::new(extension_id);
    match area {
        StorageArea::Local => local.for_extension(&ext).set_many(items).await,
        StorageArea::Session => {
            session.for_extension(&ext).set_many(items);
            Ok(())
        }
    }
}

#[tauri::command]
async fn extensions_storage_remove(
    local: tauri::State<'_, LocalStorageManager>,
    session: tauri::State<'_, SessionStorageManager>,
    extension_id: String,
    area: StorageArea,
    keys: Vec<String>,
) -> Result<()> {
    let ext = ExtensionId::new(extension_id);
    match area {
        StorageArea::Local => local.for_extension(&ext).remove_many(&keys).await,
        StorageArea::Session => {
            session.for_extension(&ext).remove_many(&keys);
            Ok(())
        }
    }
}

#[tauri::command]
async fn extensions_storage_clear(
    local: tauri::State<'_, LocalStorageManager>,
    session: tauri::State<'_, SessionStorageManager>,
    extension_id: String,
    area: StorageArea,
) -> Result<()> {
    let ext = ExtensionId::new(extension_id);
    match area {
        StorageArea::Local => local.for_extension(&ext).clear().await,
        StorageArea::Session => {
            session.for_extension(&ext).clear();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::content_tab_id;

    #[test]
    fn content_tab_id_only_for_page_webviews() {
        // The extension's own surfaces have no tab (matches Chrome).
        assert_eq!(content_tab_id("ext-bg-unpacked-eol"), None);
        assert_eq!(content_tab_id("main"), None);
        assert_eq!(content_tab_id(""), None);
        // A content/page webview gets a stable, positive, JS-safe id.
        let a = content_tab_id("test-dapp").expect("page webview has a tab id");
        assert!(a > 0 && a <= 0x7fff_ffff);
        assert_eq!(content_tab_id("test-dapp"), Some(a), "id is deterministic");
        // Distinct labels (very likely) distinct ids.
        assert_ne!(content_tab_id("test-dapp"), content_tab_id("other-page"));
    }
}
