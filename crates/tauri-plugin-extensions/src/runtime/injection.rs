//! Content-script injection pipeline.
//!
//! Wires Tauri's `on_page_load` hook (at the *plugin* level — see
//! [`attach_on_page_load`]) into the [`ExtensionRegistry`]'s
//! `content_scripts_for_url` resolver, then `eval`s the resulting scripts
//! in the appropriate JS world.
//!
//! ## How the plumbing fits together
//!
//! 1. `lib.rs` calls [`attach_on_page_load`] on the `tauri::plugin::Builder`
//!    before `.build()`. That registers a single closure that fires for
//!    **every** page-load event on **every** webview owned by the app —
//!    new or existing, main or secondary. This is simpler than walking
//!    `AppHandle::webview_windows()` + subscribing to `RunEvent::WebviewEvent::Created`
//!    manually; Tauri already does that bookkeeping for us.
//! 2. `lib.rs::init`'s `.setup()` closure then calls [`register_hooks`] with
//!    the `AppHandle`. `register_hooks` is the public entry point the spec
//!    calls for — it installs the per-frame [`BootstrapTracker`] into Tauri
//!    state so the on_page_load closure can dedupe `content-bootstrap.js`
//!    injection on subsequent lifecycle events for the same page load.
//!
//! ## Lifecycle mapping
//!
//! Chrome has three injection phases; Tauri exposes two page-load events.
//!
//! | Chrome `run_at`      | Tauri event                | Notes                  |
//! |----------------------|----------------------------|------------------------|
//! | `document_start`     | `PageLoadEvent::Started`   | Before DOM construction. |
//! | `document_end`       | *(approximated)*           | See below.             |
//! | `document_idle`      | `PageLoadEvent::Finished`  | After `window.load`.   |
//!
//! `document_end` is not a distinct Tauri event. Chrome fires it after
//! `DOMContentLoaded` but before `window.load`; `PageLoadEvent::Finished`
//! fires after `window.load`. We approximate by injecting `document_end`
//! scripts during the `Finished` phase alongside `document_idle`. In
//! practice every MV3 extension we care about either (a) runs
//! `document_end` scripts that don't care whether the load event has
//! fired yet, or (b) runs them inside their own `DOMContentLoaded`
//! listener. Both cases are fine with the late fire.
//!
//! ## World semantics
//!
//! Tauri's `WebviewWindow::eval` evaluates in the **main** world. WebView2
//! does have a mechanism for isolated-world injection (via
//! `ICoreWebView2_4::AddScriptToExecuteOnDocumentCreated`'s content world
//! parameter in newer WV2 SDKs), but Tauri v2 does not expose it through
//! `Webview::eval`. So for the spike:
//!
//! - [`World::Main`] → eval the script directly.
//! - [`World::Isolated`] → eval inside an IIFE that aliases `globalThis`
//!   and the `chrome` shim locally so the script can reference `chrome`
//!   without the page seeing injected top-level bindings. This is a
//!   *best-effort* isolation, not a true separate realm. A page that
//!   actively tries to shadow `globalThis` or monkey-patch the shim will
//!   see through it. True isolation needs either a wry PR exposing
//!   WebView2's content world API, or a per-extension `data:` URL that
//!   runs each script in its own origin. Tracked as v1 follow-up in
//!   `docs/DECISIONS.md` D-002's "hooks we may need to upstream".

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use tauri::{
    plugin::Builder as PluginBuilder,
    webview::{PageLoadEvent, PageLoadPayload},
    Manager, Runtime, Webview,
};

use crate::{
    registry::ExtensionRegistry,
    runtime::{InjectionRequest, RunAt, World},
};

/// Prefix used by hidden background webviews — see
/// [`crate::runtime::webview2::Webview2Backend::window_label`]. Page-load
/// events on these are ignored by the content-script pipeline; they already
/// have their own bootstrap injected via `initialization_script`.
const BG_WEBVIEW_LABEL_PREFIX: &str = "ext-bg-";

/// Tracks per-(webview, load-generation) whether the content bootstrap has
/// already been injected. Prevents re-injecting [`crate::CONTENT_BOOTSTRAP_JS`]
/// on every subsequent phase (`Finished` after `Started`) for the same page
/// load.
///
/// Keys: `(webview_label, url_as_string)`. A single page load fires `Started`
/// then `Finished` with the same URL, so the set membership holds across both
/// and the bootstrap injects once. Crucially, [`clear_entry`] is called at
/// every `document_start` (the start of a *new* document): a fresh document —
/// even at the SAME url (a reload, or a window closed and reopened under the
/// same label, which the EVM canary does per wallet) — has a brand-new `window`
/// with no `__extEventDispatch` polyfill, so it MUST be re-primed. Without that
/// reset, the second same-url load silently skips the bootstrap and BG→content
/// message/port delivery is dropped (it dispatches through `__extEventDispatch`).
///
/// [`clear_entry`]: BootstrapTracker::clear_entry
#[derive(Default)]
pub(crate) struct BootstrapTracker {
    bootstrapped: Mutex<HashSet<(String, String)>>,
}

impl BootstrapTracker {
    fn new() -> Self {
        Self::default()
    }

    /// Returns `true` the first time a (label, url) pair is seen; `false` on
    /// every subsequent call for that pair until the set is cleared.
    fn mark_needs_bootstrap(&self, webview_label: &str, url: &str) -> bool {
        let mut set = self
            .bootstrapped
            .lock()
            .expect("BootstrapTracker mutex poisoned");
        set.insert((webview_label.to_string(), url.to_string()))
    }

    /// Drop the bootstrap mark for one (label, url) pair. Called at every
    /// `document_start`: a new document needs a fresh bootstrap even when the
    /// URL is unchanged (see the struct docs). Idempotent.
    fn clear_entry(&self, webview_label: &str, url: &str) {
        let mut set = self
            .bootstrapped
            .lock()
            .expect("BootstrapTracker mutex poisoned");
        set.remove(&(webview_label.to_string(), url.to_string()));
    }

    /// Drop every entry for a given webview label — used when the webview is
    /// about to be recycled so stale entries don't accumulate. Not wired in
    /// the spike (per-navigation churn is low enough that the set doesn't
    /// grow unbounded in practice).
    #[allow(dead_code)]
    fn clear_for_webview(&self, webview_label: &str) {
        let mut set = self
            .bootstrapped
            .lock()
            .expect("BootstrapTracker mutex poisoned");
        set.retain(|(label, _)| label != webview_label);
    }
}

/// Public entry point called from `lib.rs::init`'s `.setup()` closure.
///
/// Installs the [`BootstrapTracker`] in Tauri state so the `on_page_load`
/// closure — which was wired on the plugin Builder before `.build()` (see
/// [`attach_on_page_load`]) — has somewhere to dedupe per-frame bootstrap.
///
/// Idempotent: if called twice (e.g. because a downstream plugin re-invokes
/// our setup) the second `manage` is a no-op.
pub fn register_hooks<R: Runtime>(app: &tauri::AppHandle<R>) -> crate::Result<()> {
    // `Manager::manage` returns `bool` — `false` means "already present", which
    // is fine for our idempotency contract.
    app.manage(Arc::new(BootstrapTracker::new()));
    tracing::debug!("content-script injection hooks registered");
    Ok(())
}

/// Wire the content-script injection flow onto a `tauri::plugin::Builder`.
///
/// Called in `lib.rs::init` *before* `.build()` because Tauri's plugin API
/// registers `on_page_load` via a builder method, not a post-construction
/// hook. Receives the plugin builder, attaches the closure, returns it for
/// further chaining.
pub(crate) fn attach_on_page_load<R: Runtime>(builder: PluginBuilder<R>) -> PluginBuilder<R> {
    builder.on_page_load(|webview, payload| {
        handle_page_load(webview, payload);
    })
}

/// The actual per-page-load dispatcher. Extracted from the closure so unit
/// tests can exercise the decision logic (skip / phase mapping) without
/// booting a Tauri app — although in practice the registry-level integration
/// test in `tests/injection_rule_resolution.rs` covers the load-bearing
/// behavior.
fn handle_page_load<R: Runtime>(webview: &Webview<R>, payload: &PageLoadPayload<'_>) {
    let label = webview.label().to_string();

    // Skip our own hidden background webviews — they already have their
    // extension's background bootstrap + worker source injected via
    // `initialization_script`. Running content-script rules inside the BG
    // webview would be a double-inject bug.
    if label.starts_with(BG_WEBVIEW_LABEL_PREFIX) {
        tracing::trace!(label = %label, "skipping page_load on BG webview");
        return;
    }

    let url = payload.url();
    let url_str = url.as_str();

    // Filter out schemes that can't host useful content scripts. Chrome
    // itself refuses to inject into these; we match the behavior so a
    // navigating `about:blank` doesn't waste cycles walking every
    // registered extension's match patterns.
    match url.scheme() {
        "tauri" | "about" | "data" | "chrome-extension" | "javascript" => {
            tracing::trace!(scheme = url.scheme(), "skipping non-injectable scheme");
            return;
        }
        _ => {}
    }

    // `PageLoadEvent` is marked `#[non_exhaustive]` in Tauri 2.x. Today the
    // only two variants are `Started` and `Finished`; the `#[allow]` lets us
    // ship a future-proof wildcard without an `unreachable_patterns` warning
    // when compiled against the current tauri release.
    #[allow(unreachable_patterns)]
    let phase = match payload.event() {
        PageLoadEvent::Started => RunAt::DocumentStart,
        PageLoadEvent::Finished => RunAt::DocumentIdle,
        // Any future variant (hypothetical `DomContentLoaded`, etc.) lands
        // here — safe default is to drop the event.
        _ => {
            tracing::debug!("unknown PageLoadEvent variant; ignoring");
            return;
        }
    };

    let app = webview.app_handle();

    let registry = match app.try_state::<ExtensionRegistry>() {
        Some(r) => r,
        None => {
            tracing::warn!(
                "ExtensionRegistry not in Tauri state; skipping injection for {url_str}"
            );
            return;
        }
    };

    let tracker = match app.try_state::<Arc<BootstrapTracker>>() {
        Some(t) => Arc::clone(&*t),
        None => {
            tracing::warn!(
                "BootstrapTracker not in Tauri state (register_hooks not called?); \
                 skipping injection for {url_str}"
            );
            return;
        }
    };

    // A new document (`document_start`) has a fresh `window` with no injected
    // bootstrap — even at the same URL (reload, or a same-label window closed
    // and reopened). Reset the mark so the next matching script re-primes the
    // frame; otherwise `__extEventDispatch` is missing on the new page and
    // BG→content message/port delivery is silently dropped.
    if phase == RunAt::DocumentStart {
        tracker.clear_entry(&label, url_str);
    }

    // Resolve every content script that matches this URL. The registry walks
    // all loaded extensions; an empty list is the common case (no matches)
    // and costs only the DashMap walk.
    let mut all_requests = registry.content_scripts_for_url(url);

    // Merge dynamically-registered scripts (chrome.scripting.registerContentScripts).
    // Wallets register their EVM inpage providers this way (Phantom, Rabby)
    // rather than declaring them in the manifest, so they must participate in
    // the same on_page_load injection as the static content_scripts.
    if let Some(dynamic) = app.try_state::<crate::runtime::dynamic_scripts::DynamicScriptStore>() {
        all_requests.extend(dynamic.requests_for_url(url));
    }

    if all_requests.is_empty() {
        return;
    }

    // Filter by phase. `document_end` scripts ride along with `document_idle`
    // (see module doc for the approximation rationale).
    let mut matching: Vec<InjectionRequest> = all_requests
        .into_iter()
        .filter(|req| {
            matches!(
                (phase, req.run_at),
                (RunAt::DocumentStart, RunAt::DocumentStart)
                    | (RunAt::DocumentIdle, RunAt::DocumentEnd)
                    | (RunAt::DocumentIdle, RunAt::DocumentIdle)
            )
        })
        .collect();

    if matching.is_empty() {
        return;
    }

    // Single-world ordering: run MAIN-world scripts BEFORE ISOLATED ones (stable
    // within each group). In real Chrome these are separate realms, but our
    // approximation shares one — so an ISOLATED content script that hardens the
    // realm with SES `lockdown()` (MetaMask's `contentscript.js` does) would
    // freeze the realm before a MAIN-world inpage provider gets to install
    // `window.ethereum`. Injecting MAIN first lets the page provider initialize
    // before any ISOLATED-world lockdown. (Per-extension real worlds — D-002 —
    // would remove this constraint.)
    matching.sort_by_key(|req| match req.world {
        World::Main => 0u8,
        World::Isolated => 1u8,
    });

    tracing::debug!(
        label = %label,
        url = %url_str,
        phase = ?phase,
        count = matching.len(),
        "injecting content scripts"
    );

    // First script into this (webview, url) pair? If so, prime the frame
    // with `content-bootstrap.js` + a configure() call before the extension
    // source lands. All subsequent requests reuse the primed frame.
    if tracker.mark_needs_bootstrap(&label, url_str) {
        // Dispatcher polyfill first — gives the shim's getEventApi() a
        // working __TAURI_EVENT__ backed by window.__extEventDispatch, the
        // entry point the Rust router evals inbound messages through. Must
        // land before the bundle so attachRuntimeEvents() (called from
        // configure below) finds it. Idempotent; eval failure downgrades the
        // surface to invoke-only rather than aborting injection.
        if let Err(e) = webview.eval(crate::runtime::EVENT_DISPATCH_POLYFILL_JS) {
            tracing::warn!(label = %label, error = %e, "event-polyfill eval failed");
        }

        // Page-error tap (diagnostic): MAIN-world content scripts (e.g. a
        // wallet's inpage provider) are eval'd unwrapped, so a throw is an
        // uncaught page error. Capture them so headless canaries can see WHY a
        // provider didn't appear. Runs at document_start, before inpage scripts.
        let _ = webview.eval(
            r#";(function(){ if(window.__extPageErrors)return; var b=[]; window.__extPageErrors=b;
              var p=function(m){try{b.push(String(m).slice(0,300)); if(b.length>30)b.shift();}catch(e){}};
              window.addEventListener('error',function(ev){p((ev.message||ev.error||'err')+' @ '+(ev.filename||'')+':'+(ev.lineno||''));});
              window.addEventListener('unhandledrejection',function(ev){p('reject: '+((ev.reason&&(ev.reason.message||ev.reason))||'unknown'));});
            })();"#,
        );

        // Bootstrap is eval'd in the main world because Tauri's
        // `Webview::eval` targets main (see module doc on the isolated-world
        // approximation). The bootstrap itself guards against double-install
        // via `__tauri_ext_content_bootstrapped`.
        if let Err(e) = webview.eval(crate::CONTENT_BOOTSTRAP_JS) {
            tracing::warn!(
                label = %label,
                error = %e,
                "failed to eval content-bootstrap; skipping injections"
            );
            return;
        }

        // Configure using the FIRST matching extension's identity. Multiple
        // extensions injecting into the same frame is legal in Chrome, but
        // each content script there runs in its *own* isolated world tagged
        // with its own extension id — a nuance our spike's single-world
        // approximation can't fully honor. We pick the first request's id;
        // future multi-extension correctness lands when we split the world
        // per extension.
        let first = &matching[0];
        let ext_id_lit = serde_json::to_string(first.extension.as_str())
            .unwrap_or_else(|_| "\"\"".to_string());
        let manifest_lit = "{}"; // Registry projection of manifest lives post-spike.
        let resource_base_lit = serde_json::to_string(
            &crate::runtime::resources::resource_base_origin(crate::runtime::host_uses_https(app)),
        )
        .unwrap_or_else(|_| "\"\"".to_string());
        let configure = format!(
            "try {{ if (typeof window.__tauri_ext_configure === 'function') \
              window.__tauri_ext_configure({{ extensionId: {ext_id_lit}, \
              manifest: {manifest_lit}, frameId: 0, \
              resourceBase: {resource_base_lit} }}); }} \
             catch (e) {{ console.error('[tauri-plugin-extensions] configure threw:', e); }}"
        );
        if let Err(e) = webview.eval(&configure) {
            tracing::warn!(
                label = %label,
                error = %e,
                "failed to eval configure()"
            );
            // Non-fatal: the bootstrap is already on the page; the extension
            // scripts may still partially function without explicit configure.
        }
    }

    // Eval each matching extension script.
    //
    // MV3_INJ_TRACE (diagnostic, generic): when set, a `location.hash` marker
    // is eval'd before each script and after the last one. Evals run in order
    // on the page's JS thread, so if a content script BLOCKS that thread
    // forever (observed in the wild: a wallet content script hanging the
    // realm), the hash — readable natively from Rust via `webview.url()` even
    // with page JS dead — names the last script that started: the blocker.
    let trace = std::env::var("MV3_INJ_TRACE").is_ok();
    let total = matching.len();
    for (idx, req) in matching.into_iter().enumerate() {
        if trace {
            let marker = format!(
                "try{{location.hash='INJTRACE_{idx}_{}_{}';}}catch(e){{}}",
                req.extension.as_str(),
                match req.world {
                    World::Main => "main",
                    World::Isolated => "isolated",
                }
            );
            let _ = webview.eval(&marker);
        }
        let script = wrap_for_world(&req.source, req.world);
        if let Err(e) = webview.eval(&script) {
            tracing::warn!(
                label = %label,
                extension = %req.extension.as_str(),
                error = %e,
                "content-script eval failed"
            );
            // Continue with the remaining scripts — one extension's failure
            // should not knock out others.
        }
    }
    if trace {
        let _ = webview.eval(format!(
            "try{{location.hash='INJTRACE_DONE_{total}';}}catch(e){{}}"
        ));
    }
}

/// Wrap a content-script source for evaluation in the requested world.
///
/// - [`World::Main`] → wrapped in an IIFE that **shadows `chrome`/`browser` to
///   `undefined`**. A real MAIN world (the page realm) has no `chrome`/`browser`
///   — those exist only in the ISOLATED content-script world — but our
///   single-world approximation installs the shim on the page globals, visible
///   to MAIN scripts too. That breaks inpage providers that branch on
///   `typeof chrome` to decide "am I the content script or the page?": MetaMask's
///   `inpage.js` does exactly this and, seeing `chrome`, takes the content-script
///   branch and never assigns `window.ethereum`. Shadowing restores the page
///   realm those scripts expect; they reach their ISOLATED content script via
///   `window.postMessage`, which is unaffected. (Per-extension real worlds —
///   D-002 — would remove the need for this shim.)
/// - [`World::Isolated`] → wrapped in an IIFE that (a) captures `chrome` and
///   `browser` into locals so the script can reference them without the
///   page's globals rebinding, and (b) catches synchronous throws so one
///   bad script doesn't halt the others.
///
/// This is the isolated-world **approximation** described in the module
/// docs — it's not a real isolated realm. Exposed as `pub` so integration
/// tests (which can't reach into `pub(crate)` items) can exercise the
/// wrapping contract without booting Tauri.
pub fn wrap_for_world(source: &str, world: World) -> String {
    match world {
        World::Main => format!(
            "(function (chrome, browser) {{\n{source}\n}})(void 0, void 0);\n"
        ),
        World::Isolated => {
            // The IIFE captures `chrome` / `browser` bindings from globalThis
            // at the point the script runs (post-bootstrap, so the shim has
            // already installed them). Script code that calls
            // `chrome.runtime.sendMessage(...)` works unchanged; script code
            // that mutates `window.chrome` leaks into the page (honest-by-
            // comment limitation, not a silent bug).
            //
            // NOTE (synthetic-isolated-world experiment, 2026-06-10): a
            // `with(Object.create(realGlobal))` surrogate global was tried here
            // to stop MetaMask's LavaMoat `scuttleGlobalThis` (which captures
            // `x = globalThis`) from poisoning the shared page realm. It WORKED
            // for MetaMask — `window.ethereum`/`isMetaMask` became readable on
            // the page — but REGRESSED Phantom EVM injection and Rabby's
            // `eth_chainId` (both content↔BG transports broke: a shared world
            // can't give the scuttle an isolated `globalThis` and give every
            // wallet's transport full real-window identity at once). Reverted to
            // honour the no-regression constraint. This is empirical proof that
            // real per-extension isolated worlds (D-002) are required — see
            // `docs/bg-host-service-worker.md` Phase 2f.
            format!(
                "(function (chrome, browser) {{\n\
                   try {{\n{source}\n}} catch (err) {{\n\
                     console.error('[tauri-plugin-extensions] content script threw:', err);\n\
                   }}\n\
                 }})(globalThis.chrome, globalThis.browser);\n"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_tracker_is_first_only_for_fresh_pair() {
        let t = BootstrapTracker::new();
        assert!(t.mark_needs_bootstrap("main", "https://example.com/"));
        assert!(!t.mark_needs_bootstrap("main", "https://example.com/"));
        // Different URL on same webview resets the "first" signal.
        assert!(t.mark_needs_bootstrap("main", "https://example.com/about"));
        // Different webview is independent.
        assert!(t.mark_needs_bootstrap("other", "https://example.com/"));
    }

    #[test]
    fn bootstrap_tracker_clear_drops_only_matching_webview() {
        let t = BootstrapTracker::new();
        t.mark_needs_bootstrap("a", "u1");
        t.mark_needs_bootstrap("a", "u2");
        t.mark_needs_bootstrap("b", "u1");
        t.clear_for_webview("a");
        // After clearing "a", both (a,u1) and (a,u2) are "first" again;
        // (b,u1) still "already seen".
        assert!(t.mark_needs_bootstrap("a", "u1"));
        assert!(t.mark_needs_bootstrap("a", "u2"));
        assert!(!t.mark_needs_bootstrap("b", "u1"));
    }

    #[test]
    fn bootstrap_tracker_clear_entry_re_primes_same_url() {
        // Regression: a same-label window closed and reopened at the SAME url
        // (the EVM canary reopens `test-dapp` per wallet) must re-bootstrap —
        // the new document has no __extEventDispatch, so BG→content delivery
        // would otherwise be dropped. clear_entry at document_start fixes it.
        let t = BootstrapTracker::new();
        assert!(t.mark_needs_bootstrap("test-dapp", "http://app/dapp.html"));
        assert!(!t.mark_needs_bootstrap("test-dapp", "http://app/dapp.html"));
        // New document_start at the same url → clear, then it's "first" again.
        t.clear_entry("test-dapp", "http://app/dapp.html");
        assert!(t.mark_needs_bootstrap("test-dapp", "http://app/dapp.html"));
        // A different label is untouched by the clear.
        t.mark_needs_bootstrap("other", "http://app/dapp.html");
        t.clear_entry("test-dapp", "http://app/dapp.html");
        assert!(!t.mark_needs_bootstrap("other", "http://app/dapp.html"));
    }

    #[test]
    fn wrap_for_world_main_shadows_chrome_for_page_realm() {
        // MAIN-world scripts run in the page realm, which has no chrome/browser;
        // shadowing them as undefined lets inpage providers (MetaMask) take the
        // page branch and assign window.ethereum. The source is preserved.
        let src = "if (typeof chrome === 'undefined') window.ethereum = {};";
        let out = wrap_for_world(src, World::Main);
        assert!(out.starts_with("(function (chrome, browser) {"));
        assert!(out.contains(src));
        assert!(out.contains("(void 0, void 0)"));
    }

    #[test]
    fn wrap_for_world_isolated_wraps_in_iife_with_chrome_capture() {
        let src = "chrome.runtime.sendMessage({ ping: true });";
        let out = wrap_for_world(src, World::Isolated);
        assert!(out.starts_with("(function (chrome, browser) {"));
        assert!(out.contains(src));
        assert!(out.contains("globalThis.chrome"));
        assert!(out.contains("globalThis.browser"));
        // The try/catch keeps one bad script from halting siblings.
        assert!(out.contains("catch (err)"));
    }

    #[test]
    fn bg_prefix_matches_webview2_window_label_contract() {
        // Anti-regression: the skip in `handle_page_load` relies on the
        // `ext-bg-` prefix matching what `webview2::Webview2Backend::window_label`
        // produces. That fn is private; this test documents the coupling as
        // a literal-prefix equality check rather than reaching into the
        // windows-only module.
        assert_eq!(BG_WEBVIEW_LABEL_PREFIX, "ext-bg-");
    }
}
