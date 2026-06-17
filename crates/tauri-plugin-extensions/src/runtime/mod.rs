//! Platform-specific webview integration.
//!
//! The [`Backend`] trait is the stable contract. Concrete impls live in
//! `webview2.rs` (Windows, v1 target), with stubs for WKWebView and
//! WebKitGTK that return [`crate::Error::PlatformUnsupported`].
//!
//! ## How Agent C obtains a backend
//!
//! The Windows backend holds a `tauri::AppHandle<tauri::Wry>` internally, so
//! constructing one requires an `AppHandle`. Two entry points:
//!
//! - [`default_backend`] — takes an `&AppHandle<R>` and returns a boxed
//!   `Arc<dyn Backend>`. Intended for the `.setup()` closure in `lib.rs`.
//!   On non-Wry runtimes it returns [`crate::Error::PlatformUnsupported`]
//!   (see [`webview2::Webview2Backend::from_tauri`]). Agent C already
//!   calls this with `app.app_handle().clone()` and downgrades `Err` to
//!   `None`, so macOS / Linux loads run without a background backend.
//! - [`webview2::Webview2Backend::new`] — Windows-only direct constructor
//!   for call sites that already have a concrete `AppHandle<tauri::Wry>`.
//!
//! Agent C stores the `Arc<dyn Backend>` in Tauri state via `app.manage(...)`
//! and resolves it from the loader with `app.state::<BackendState>()`.

pub mod background;
pub mod dynamic_scripts;
pub mod injection;
pub mod resources;
pub mod wait;

#[cfg(target_os = "windows")]
pub mod webview2;
#[cfg(target_os = "macos")]
pub mod wkwebview;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod webkitgtk;

use crate::{registry::ExtensionId, Result};

/// Stable label of the hidden background `WebviewWindow` for an extension:
/// `ext-bg-<first 12 chars of id>`. Tauri window labels must be unique per
/// app and non-empty; 12 chars of the stable id give enough entropy.
/// Shared by the spawn path (`webview2::Webview2Backend`), the lifecycle
/// orphan reconciler, and the message router (which addresses BG-bound
/// dispatches by this label).
pub fn bg_window_label(extension: &ExtensionId) -> String {
    let prefix: String = extension.as_str().chars().take(12).collect();
    format!("ext-bg-{}", prefix)
}

/// Whether the host app serves its webviews over `https` (Tauri's
/// `useHttpsScheme`). All plugin-created webviews (the hidden `ext-bg-*`
/// windows) and the `extres://` resource origin must match the host's scheme:
/// WebView2 serves the app + custom schemes at `https` when the host opts in,
/// and an `https` page cannot load an `http` resource (mixed content). Read
/// from the first window's config; defaults to `false` (http) — so http hosts
/// are unaffected. Some wallets (Phantom's EVM side) only inject on `https`/
/// localhost origins, so a host that wants them sets `useHttpsScheme: true`.
pub fn host_uses_https<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    app.config()
        .app
        .windows
        .first()
        .map(|w| w.use_https_scheme)
        .unwrap_or(false)
}

/// Dispatcher polyfill injected into every surface (BG bootstrap + content
/// bootstrap) BEFORE the embedded shim bundle evaluates.
///
/// The shim's `getEventApi()` looks for `window.__TAURI_EVENT__`; nothing in
/// a stock Tauri v2 webview provides one (the real event plugin works via
/// `plugin:event|listen` invokes and per-listener window slots). Instead of
/// requiring event-plugin capabilities on every surface, the Rust router
/// delivers inbound traffic by `eval`-ing `window.__extEventDispatch(event,
/// payload)` into the target webview; this polyfill backs that entry point
/// with a local handler registry and exposes it through the `__TAURI_EVENT__`
/// shape the shim already understands. Works on any origin — including
/// `file://` pages where Tauri injects no IPC bridge at all.
pub const EVENT_DISPATCH_POLYFILL_JS: &str = r#"
;(function () {
  if (window.__extEventDispatch) return;
  var handlers = new Map();
  Object.defineProperty(window, '__extEventDispatch', {
    value: function (event, payload) {
      var set = handlers.get(event);
      if (!set) return;
      Array.from(set).forEach(function (fn) {
        try { fn({ payload: payload }); }
        catch (e) { console.error('[tauri-plugin-extensions] event handler threw:', e); }
      });
    },
    configurable: false,
    writable: false,
  });
  window.__TAURI_EVENT__ = {
    listen: function (event, handler) {
      var set = handlers.get(event);
      if (!set) { set = new Set(); handlers.set(event, set); }
      set.add(handler);
      return Promise.resolve(function () { set.delete(handler); });
    },
  };
})();
"#;

/// When a script should be injected in the page lifecycle. Mirrors Chrome's
/// `content_scripts[].run_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAt {
    /// `document_start` — before DOM construction.
    DocumentStart,
    /// `document_end` — after DOMContentLoaded but before `window.load`.
    DocumentEnd,
    /// `document_idle` — after `window.load` (default).
    DocumentIdle,
}

/// Which JS world to inject into. `Main` shares the page's global; `Isolated`
/// is the Chrome-extension default and exposes `chrome.*` to the content
/// script without leaking to the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum World {
    /// Page's main world.
    Main,
    /// Isolated content-script world.
    Isolated,
}

/// Compiled injection request for a single script.
#[derive(Debug, Clone)]
pub struct InjectionRequest {
    /// Extension the script belongs to.
    pub extension: ExtensionId,
    /// JS source to evaluate.
    pub source: String,
    /// When to inject.
    pub run_at: RunAt,
    /// Which world to target.
    pub world: World,
}

/// Platform-specific webview integration. Implementations:
/// - `webview2::Backend` on Windows (v1 target)
/// - `wkwebview::Backend` on macOS (stub, returns `PlatformUnsupported`)
/// - `webkitgtk::Backend` on Linux (stub, returns `PlatformUnsupported`)
#[async_trait::async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Inject the given script into the target webview at the appropriate
    /// lifecycle phase. Called by the registry once per matched frame.
    async fn inject(&self, request: InjectionRequest) -> Result<()>;

    /// Spin up a hidden webview hosting the given extension's background
    /// service worker. `manifest` is the extension's raw manifest JSON: it is
    /// handed to the shim's `__tauri_ext_configure` so
    /// `chrome.runtime.getManifest()` works in the BG surface, AND it carries
    /// `background.service_worker` + `background.type`, which the backend uses
    /// to load the worker from the resource origin (D-008) — `import(url)` for
    /// a module, a synchronous `importScripts` shim for classic. The worker's
    /// on-disk source is not passed: the resource scheme serves it, and its
    /// existence is gated upstream in the lifecycle manager. See
    /// `background.rs` for the implementation.
    async fn spawn_background(
        &self,
        extension: ExtensionId,
        manifest: serde_json::Value,
    ) -> Result<background::BackgroundHandle>;
}

/// Select the platform-appropriate backend given a Tauri `AppHandle`.
/// Returns an `Arc<dyn Backend>` suitable for `app.manage(...)`.
///
/// On Windows this constructs a [`webview2::Webview2Backend`] bound to the
/// given app handle. On macOS / Linux it returns
/// [`crate::Error::PlatformUnsupported`].
pub fn default_backend_for_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<std::sync::Arc<dyn Backend>> {
    #[cfg(target_os = "windows")]
    {
        let backend = webview2::Webview2Backend::from_tauri(app)?;
        Ok(std::sync::Arc::new(backend))
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Silence the unused-parameter warning on non-Windows builds.
        let _ = app;
        Err(crate::Error::PlatformUnsupported)
    }
}

/// Alias for [`default_backend_for_app`] — early call sites used the
/// shorter name. Kept so Agent C's `.setup()` closure compiles whichever
/// name it converged on.
pub fn default_backend<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<std::sync::Arc<dyn Backend>> {
    default_backend_for_app(app)
}
