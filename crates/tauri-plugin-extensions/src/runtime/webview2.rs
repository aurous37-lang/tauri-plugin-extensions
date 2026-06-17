//! WebView2 backend (Windows). Owned by Agent D.
//!
//! Per D-001 the MV3 background service-worker analog runs inside a hidden
//! `WebviewWindow` (visible=false, decorations=false, skip_taskbar=true,
//! focused=false). One hidden webview per loaded extension. This module is
//! the concrete implementation of [`crate::runtime::Backend`] for Windows.
//!
//! ## What's wired
//!
//! - [`Webview2Backend::spawn_background`] — creates a 1x1 hidden
//!   `WebviewWindow` labelled `ext-bg-<shortid>`, seeds it with
//!   [`crate::BACKGROUND_BOOTSTRAP_JS`] + a bridge stub + the extension's
//!   own service-worker source via `initialization_script`, and returns a
//!   [`BackgroundHandle`] bound to the originating app handle.
//! - [`Webview2Backend::from_tauri`] — downcasts any `AppHandle<R>` to
//!   `AppHandle<Wry>`. Non-Wry runtimes (e.g. the mock runtime in tests)
//!   surface as [`crate::Error::PlatformUnsupported`]. In practice the
//!   wallet-harness consumer is always `Wry`.
//!
//! ## Injection path
//!
//! [`Webview2Backend::inject`] is the *programmatic* (one-shot) entry point
//! — host code calls it directly to force a script into the main webview.
//! The per-navigation, content-script-driven flow is handled separately in
//! [`crate::runtime::injection`], which hooks Tauri's plugin-level
//! `on_page_load` and funnels every matched rule through `Webview::eval`.
//! Both paths share the same world-wrapping helper so the MAIN vs
//! ISOLATED approximation stays consistent.

use std::any::{Any, TypeId};
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, Wry};

use super::{
    background::BackgroundHandle,
    injection::wrap_for_world,
    wait::{is_label_already_exists, poll_until},
    Backend, InjectionRequest,
};
use crate::{registry::ExtensionId, Error, Result};

/// How long `spawn_background` waits for a lingering `ext-bg-<id>` label to
/// leave the registrar before (re)building. The previous incarnation's
/// `shutdown` already waited for teardown; this only bites when a caller
/// raced it or a close is wedged.
const SPAWN_LABEL_FREE_CEILING: Duration = Duration::from_secs(2);

/// Poll cadence for the registrar check (cheap map read, no wry calls).
const SPAWN_LABEL_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Concrete WebView2 backend. Holds an `AppHandle<Wry>` so the async
/// `spawn_background` can create windows without being given one at call
/// time.
#[derive(Clone)]
pub struct Webview2Backend {
    app: AppHandle<Wry>,
}

impl Webview2Backend {
    /// Construct the backend from a concrete `AppHandle<Wry>`. Most consumers
    /// should prefer [`Webview2Backend::from_tauri`] which accepts any
    /// runtime generic.
    pub fn new(app: AppHandle<Wry>) -> Self {
        Self { app }
    }

    /// Construct from a generic `AppHandle<R>`. In practice Tauri apps on
    /// Windows always use `Wry`, but the plugin type-parameter is generic
    /// for forward-compat. This downcasts at runtime via [`std::any::Any`];
    /// a non-Wry runtime surfaces as [`Error::PlatformUnsupported`].
    pub fn from_tauri<R: Runtime>(app: &AppHandle<R>) -> Result<Self> {
        // Short-circuit on the TypeId — avoids an Any coercion for the
        // mismatch case and makes the intent explicit.
        if TypeId::of::<R>() != TypeId::of::<Wry>() {
            return Err(Error::PlatformUnsupported);
        }
        // `AppHandle<R>` is `'static` for every `R: Runtime` (the Runtime
        // trait carries a `'static` bound), so the coercion to `&dyn Any`
        // is well-formed and `downcast_ref` is safe Rust.
        let any: &dyn Any = app;
        match any.downcast_ref::<AppHandle<Wry>>() {
            Some(concrete) => Ok(Self::new(concrete.clone())),
            None => Err(Error::PlatformUnsupported),
        }
    }

    /// Compose the bootstrap script for a background webview. Concatenates,
    /// in order:
    ///
    /// 1. [`crate::runtime::EVENT_DISPATCH_POLYFILL_JS`] — gives the shim a
    ///    working `__TAURI_EVENT__` backed by `window.__extEventDispatch`,
    ///    which the Rust router `eval`s inbound traffic through. Must come
    ///    first so `getEventApi()` finds it whenever the bundle asks.
    /// 2. [`crate::BACKGROUND_BOOTSTRAP_JS`] — the JS runtime bundle
    ///    emitted by Agent E (defines `__tauri_ext_configure`).
    /// 3. A small bridge that exposes `__extRuntime.invoke` for the shim to
    ///    reach Rust via `window.__TAURI_INTERNALS__.invoke`.
    /// 4. The `__tauri_ext_configure` call — hands the shim its extension
    ///    id + manifest, flips the surface to `background`, and attaches
    ///    the event wiring (which fires the synthetic `onInstalled`).
    /// 5. The **service-worker loader** ([`super::resources::sw_loader_script`])
    ///    — D-008. Instead of inlining the worker source into this classic
    ///    bootstrap (which fatally fails `background.type:"module"` workers at
    ///    parse time and provides no `importScripts`), it loads the worker from
    ///    the resource origin: `import(url)` for a module, a synchronous
    ///    `importScripts` shim for classic. Relative `import`/`importScripts`
    ///    specifiers resolve against the extension root because the entry is
    ///    fetched from `extres://<id>/...`. The worker still executes in this
    ///    app-origin realm, so `__TAURI_INTERNALS__`/`chrome.*`/`__extRuntime`
    ///    stay visible to it.
    ///
    /// `initialization_script` runs at document_start before any other
    /// script on the page, so layers 1–4 are in place before the loader
    /// (layer 5) starts the worker.
    ///
    /// The worker entry path + module flag are read from `manifest.background`;
    /// the on-disk worker source is no longer needed here (the resource scheme
    /// serves it), only its existence gates spawning, upstream in `try_spawn`.
    fn compose_bootstrap(
        extension: &ExtensionId,
        manifest: &serde_json::Value,
        use_https: bool,
    ) -> String {
        // Inject the extension id as a compile-time-safe JSON literal.
        // serde_json::to_string on a string produces a valid JS string
        // literal, which is safer than hand-escaping.
        let ext_id_literal = serde_json::to_string(extension.as_str())
            .unwrap_or_else(|_| "\"\"".to_string());

        // BG-only diagnostics: buffer console.error / window errors /
        // unhandled rejections so host tooling can read them back by eval.
        // Extension BG workers run headless — without this, a failing shim
        // invoke or a worker crash is invisible. Capped at 50 entries.
        let error_tap = r#"
;(function () {
  if (window.__extLastErrors) return;
  var buf = [];
  window.__extLastErrors = buf;
  var push = function (kind, args) {
    try {
      buf.push(kind + ': ' + Array.prototype.map.call(args, String).join(' '));
      if (buf.length > 50) buf.shift();
    } catch (e) {}
  };
  var origError = console.error;
  console.error = function () { push('console.error', arguments); return origError.apply(console, arguments); };
  window.addEventListener('error', function (ev) { push('window.onerror', [ev.message || String(ev)]); });
  window.addEventListener('unhandledrejection', function (ev) {
    var stack = (ev.reason && ev.reason.stack) ? (' @ ' + String(ev.reason.stack).slice(0, 240)) : '';
    push('unhandledrejection', [((ev.reason && (ev.reason.message || ev.reason)) || 'unknown') + stack]);
  });
})();
"#;

        let bridge = format!(
            r#"
;(function () {{
  // Background-webview bridge. The shim layer (Agent E) binds against
  // this; the page itself should never see it.
  const invoke = (cmd, args) => {{
    if (!window.__TAURI_INTERNALS__ || typeof window.__TAURI_INTERNALS__.invoke !== 'function') {{
      return Promise.reject(new Error('tauri-plugin-extensions: __TAURI_INTERNALS__ unavailable in background webview'));
    }}
    return window.__TAURI_INTERNALS__.invoke(cmd, args);
  }};
  Object.defineProperty(window, '__extRuntime', {{
    value: Object.freeze({{
      extensionId: {ext_id_literal},
      invoke,
      // Agent E fills this in with the BG-flavored chrome.* shim.
      onMessage: () => {{}},
    }}),
    configurable: false,
    writable: false,
  }});
}})();
"#
        );

        let manifest_lit =
            serde_json::to_string(manifest).unwrap_or_else(|_| "{}".to_string());
        let resource_base_lit =
            serde_json::to_string(&super::resources::resource_base_origin(use_https))
                .unwrap_or_else(|_| "\"\"".to_string());
        let configure = format!(
            "try {{ if (typeof window.__tauri_ext_configure === 'function') \
              window.__tauri_ext_configure({{ extensionId: {ext_id_literal}, \
              manifest: {manifest_lit}, surface: 'background', \
              resourceBase: {resource_base_lit} }}); }} \
             catch (e) {{ console.error('[tauri-plugin-extensions] bg configure threw:', e); }}"
        );

        // Layer 5: the service-worker loader. Read the entry path + module
        // flag from the manifest; the resource scheme serves the worker bytes.
        let bg = manifest.get("background");
        let sw_rel = bg
            .and_then(|b| b.get("service_worker"))
            .and_then(|v| v.as_str());
        let is_module = bg
            .and_then(|b| b.get("type"))
            .and_then(|v| v.as_str())
            .map(|t| t.eq_ignore_ascii_case("module"))
            .unwrap_or(false);
        let loader = match sw_rel {
            Some(rel) => {
                let plan = super::resources::sw_load_plan(
                    &super::resources::resource_base_origin(use_https),
                    extension.as_str(),
                    rel,
                    is_module,
                );
                super::resources::sw_loader_script(&plan)
            }
            None => {
                // Spawning is gated on a readable worker upstream, so this is
                // unreachable in practice; emit nothing rather than panic.
                String::new()
            }
        };

        let mut out = String::with_capacity(
            error_tap.len()
                + super::EVENT_DISPATCH_POLYFILL_JS.len()
                + crate::BACKGROUND_BOOTSTRAP_JS.len()
                + bridge.len()
                + configure.len()
                + loader.len()
                + 96,
        );
        out.push_str(error_tap);
        out.push_str("\n;\n");
        out.push_str(super::EVENT_DISPATCH_POLYFILL_JS);
        out.push_str("\n;\n");
        out.push_str(crate::BACKGROUND_BOOTSTRAP_JS);
        out.push_str("\n;\n");
        out.push_str(&bridge);
        out.push_str("\n;\n");
        out.push_str(&configure);
        out.push_str("\n;\n");
        out.push_str(&loader);
        out.push('\n');
        out
    }

    /// Generate the stable per-extension window label. Delegates to the
    /// shared [`super::bg_window_label`] so the spawn path and the message
    /// router can never drift apart on the addressing scheme.
    fn window_label(extension: &ExtensionId) -> String {
        super::bg_window_label(extension)
    }
}

#[async_trait::async_trait]
impl Backend for Webview2Backend {
    async fn inject(&self, request: InjectionRequest) -> Result<()> {
        // Programmatic (non-page-load-driven) injection into the host's
        // main webview. The per-navigation content-script flow lives in
        // `runtime::injection` and is driven by the plugin-level
        // `on_page_load` hook; this method is the escape hatch for host
        // code that wants to force a script into the current page.
        //
        // Label: "main" is the Tauri convention for the primary webview
        // window. If a consumer renames theirs, the registry-driven flow
        // still works (it operates on the `Webview` handed to the hook);
        // this one-shot path intentionally targets "main" only.
        let window = self
            .app
            .get_webview_window("main")
            .ok_or_else(|| Error::Runtime("no 'main' webview to inject into".into()))?;
        let script = wrap_for_world(&request.source, request.world);
        tracing::debug!(
            extension = %request.extension.as_str(),
            world = ?request.world,
            bytes = script.len(),
            "eval content script into main webview"
        );
        window
            .eval(&script)
            .map_err(|e| Error::Runtime(format!("eval: {e}")))?;
        Ok(())
    }

    async fn spawn_background(
        &self,
        extension: ExtensionId,
        manifest: serde_json::Value,
    ) -> Result<BackgroundHandle> {
        let label = Self::window_label(&extension);
        // The hidden BG webview must use the same scheme as the host app
        // (http vs https) so it shares the app origin and the resource scheme
        // it loads its worker from matches (no mixed content).
        let use_https = super::host_uses_https(&self.app);
        let bootstrap = Self::compose_bootstrap(&extension, &manifest, use_https);

        tracing::debug!(
            extension = %extension.as_str(),
            label = %label,
            bootstrap_bytes = bootstrap.len(),
            use_https,
            "spawning background webview"
        );

        // A previous incarnation's close may still be settling — wry
        // dispatches close() asynchronously, so the label can linger in the
        // registrar after `BackgroundHandle::shutdown` gave up waiting.
        // Wait for it to free instead of racing into AlreadyExists.
        self.await_label_free(&label).await;

        match self.build_bg_window(&label, &bootstrap, use_https) {
            Ok(()) => {}
            Err(Error::Tauri(err)) if is_label_already_exists(&err) => {
                // Lost the race between our registrar check and the build —
                // wait once more and retry a single time.
                tracing::debug!(
                    extension = %extension.as_str(),
                    label = %label,
                    "label re-registered between check and build; waiting to retry"
                );
                let freed = self.await_label_free(&label).await;
                match self.build_bg_window(&label, &bootstrap, use_https) {
                    Ok(()) => {}
                    Err(Error::Tauri(err)) if is_label_already_exists(&err) && !freed => {
                        // The old window outlived every ceiling — its close
                        // is wedged, not in flight. Adopt it rather than
                        // leaving the extension with no BG at all: the old
                        // worker realm (with its previous bootstrap) stays
                        // live, which for a same-source reload is the same
                        // worker Chrome would have restarted. The watchdog's
                        // state_consistency rule still covers us if the
                        // adopted window dies later.
                        if self.app.get_webview_window(&label).is_some() {
                            tracing::warn!(
                                extension = %extension.as_str(),
                                label = %label,
                                "adopting existing background webview — previous \
                                 teardown never completed; old worker realm stays live"
                            );
                            return Ok(BackgroundHandle {
                                extension,
                                label,
                                app: Some(self.app.clone()),
                            });
                        }
                        return Err(Error::Tauri(err));
                    }
                    Err(other) => return Err(other),
                }
            }
            Err(other) => return Err(other),
        }

        if self.app.get_webview_window(&label).is_none() {
            return Err(Error::Runtime(format!(
                "background webview '{}' failed to register with AppHandle after build",
                label
            )));
        }

        tracing::info!(
            extension = %extension.as_str(),
            label = %label,
            "background webview ready"
        );

        Ok(BackgroundHandle {
            extension,
            label,
            app: Some(self.app.clone()),
        })
    }
}

impl Webview2Backend {
    /// Poll the registrar until `label` is free (or the spawn ceiling
    /// expires). Returns whether the label freed.
    async fn await_label_free(&self, label: &str) -> bool {
        let app = self.app.clone();
        let label_owned = label.to_string();
        let freed = poll_until(
            SPAWN_LABEL_POLL_INTERVAL,
            SPAWN_LABEL_FREE_CEILING,
            move || app.get_webview_window(&label_owned).is_none(),
        )
        .await;
        if !freed {
            tracing::warn!(
                label = %label,
                ceiling_ms = SPAWN_LABEL_FREE_CEILING.as_millis() as u64,
                "background webview label did not free within spawn ceiling"
            );
        }
        freed
    }

    /// One build attempt for the hidden BG window. Split out so the
    /// AlreadyExists retry path can run it twice.
    ///
    /// `WebviewWindowBuilder::build` must run on the main thread in Tauri
    /// v2. `AppHandle::run_on_main_thread` takes a `FnOnce() + Send +
    /// 'static` with no return value, so we shuttle the build result back
    /// through a std mpsc channel. The receiver blocks this (non-main)
    /// async task's thread, which is acceptable because `spawn_background`
    /// is called infrequently (once per extension load).
    fn build_bg_window(&self, label: &str, bootstrap: &str, use_https: bool) -> Result<()> {
        // WebviewUrl::App(PathBuf) loads the Tauri app URL root. For a
        // background webview we don't need any real page; initialization_script
        // is what actually runs the worker. An empty path resolves to the
        // tauri://localhost root, which is adequate as a host document — and
        // crucially keeps the app origin so `__TAURI_INTERNALS__.invoke`
        // exists for the BG-side chrome.* shim. (Consequence for hosts: the
        // app's own frontend document boots inside every BG window; host
        // frontends should no-op when `currentWebview.label` starts with
        // `ext-bg-`.)
        let url = WebviewUrl::App(std::path::PathBuf::from(""));

        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<(), tauri::Error>>();
        let app_for_closure = self.app.clone();
        let label_for_build = label.to_string();
        let bootstrap_for_build = bootstrap.to_string();

        self.app
            .run_on_main_thread(move || {
                let builder =
                    WebviewWindowBuilder::new(&app_for_closure, &label_for_build, url)
                        .visible(false)
                        .decorations(false)
                        .skip_taskbar(true)
                        .focused(false)
                        .resizable(false)
                        .inner_size(1.0, 1.0)
                        .use_https_scheme(use_https)
                        .initialization_script(&bootstrap_for_build);
                let outcome = builder.build().map(|_window| ());
                // If the receiver is gone the caller was cancelled; that's fine.
                let _ = tx.send(outcome);
            })
            .map_err(Error::Tauri)?;

        rx.recv()
            .map_err(|e| Error::Runtime(format!("main-thread build channel dropped: {e}")))?
            .map_err(Error::Tauri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_label_format() {
        let id = ExtensionId::new("local-0123456789abcdef");
        let label = Webview2Backend::window_label(&id);
        assert_eq!(label, "ext-bg-local-012345");
        assert!(label.starts_with("ext-bg-"));
        // Label always <= "ext-bg-" (7 chars) + 12 chars = 19 chars.
        assert!(label.len() <= 19);
    }

    #[test]
    fn window_label_short_id_is_not_padded() {
        let id = ExtensionId::new("abc");
        let label = Webview2Backend::window_label(&id);
        assert_eq!(label, "ext-bg-abc");
    }

    #[test]
    fn compose_bootstrap_embeds_all_five_layers() {
        let id = ExtensionId::new("test-ext-id");
        let manifest = serde_json::json!({
            "name": "Test Ext",
            "manifest_version": 3,
            "background": { "service_worker": "background/sw.js", "type": "module" }
        });
        let out = Webview2Backend::compose_bootstrap(&id, &manifest, false);

        // Layer 1: the dispatcher polyfill comes before the bundle so
        // getEventApi() finds __TAURI_EVENT__ whenever the shim asks.
        let polyfill_at = out
            .find("__extEventDispatch")
            .expect("polyfill present");
        let bundle_at = out
            .find(crate::BACKGROUND_BOOTSTRAP_JS)
            .expect("bundle present");
        assert!(polyfill_at < bundle_at, "polyfill must precede the bundle");
        // Layer 3: the bridge exposes __extRuntime with the id.
        assert!(out.contains("__extRuntime"));
        assert!(out.contains("\"test-ext-id\""));
        assert!(out.contains("__TAURI_INTERNALS__"));
        // Layer 4: configure hands the shim its id + manifest as background.
        assert!(out.contains("__tauri_ext_configure"));
        assert!(out.contains("\"Test Ext\""));
        assert!(out.contains("surface: 'background'"));
        // Layer 5 (D-008): a module worker loads via dynamic import of the
        // resource-origin entry, AFTER the configure call — never inlined.
        let configure_at = out.find("__tauri_ext_configure").expect("configure");
        let loader_at = out.find("import(SW_URL)").expect("module loader present");
        assert!(configure_at < loader_at, "loader must follow configure");
        assert!(out.contains("test-ext-id/background/sw.js"));
    }

    #[test]
    fn compose_bootstrap_classic_worker_uses_importscripts_loader() {
        let id = ExtensionId::new("classic-ext");
        let manifest = serde_json::json!({
            "manifest_version": 3,
            "background": { "service_worker": "sw.js", "type": "classic" }
        });
        let out = Webview2Backend::compose_bootstrap(&id, &manifest, false);
        // Classic worker: the synchronous importScripts shim drives the entry,
        // and the loader is flagged non-module so the import() branch is dead.
        assert!(out.contains("self.importScripts(SW_URL)"));
        assert!(out.contains("classic-ext/sw.js"));
        assert!(out.contains("IS_MODULE = false"), "classic worker must not run the module branch");
    }

    #[test]
    fn compose_bootstrap_escapes_extension_id_safely() {
        // An extension id with quotes would break naive concatenation.
        let id = ExtensionId::new("evil\"; window.__pwned = 1; //");
        let manifest = serde_json::json!({
            "background": { "service_worker": "sw.js" }
        });
        let out = Webview2Backend::compose_bootstrap(&id, &manifest, false);
        // serde_json escapes the embedded quote; the raw injection pattern
        // must not appear verbatim in the composed bootstrap.
        assert!(!out.contains("evil\"; window.__pwned"));
        // But the escaped form does.
        assert!(out.contains("evil\\\""));
    }
}
