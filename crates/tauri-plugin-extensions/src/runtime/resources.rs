//! `web_accessible_resources` serving for the extension-resource URI scheme.
//!
//! Chrome exposes an extension's `web_accessible_resources` at
//! `chrome-extension://<id>/<path>`, and content scripts inject their MAIN-world
//! provider by appending `<script src=chrome.runtime.getURL("inpage.js")>`.
//! Rabby's EVM provider (`pageProvider.js`) and the page-side half of many
//! wallets work exactly this way.
//!
//! This module is the platform-agnostic core behind the custom URI scheme
//! registered in `lib.rs`: given an extension's on-disk root, its declared
//! `web_accessible_resources` resource globs, and the request path, it decides
//! which file (if any) may be served. The scheme handler in `lib.rs` wires this
//! to `tauri::plugin::Builder::register_uri_scheme_protocol`.
//!
//! Security: two independent guards. (1) Lexical — any `..` segment is rejected
//! before touching the filesystem. (2) `web_accessible_resources` membership —
//! a file is served only if it matches a declared resource glob, mirroring
//! Chrome (an extension's non-WAR files are not page-reachable). The scheme
//! handler adds a third, canonicalized "still under root" check as
//! defense-in-depth.

use std::path::{Path, PathBuf};

use dashmap::DashMap;

use crate::matcher::Glob;
use crate::registry::ExtensionId;

/// Custom URI scheme name under which `web_accessible_resources` are served.
/// On Windows/WebView2 this is reachable at `http://<scheme>.localhost/...`.
pub const RESOURCE_SCHEME: &str = "extres";

/// Origin (scheme + authority, no trailing slash) at which the resource scheme
/// is reachable in the current platform's webview. `chrome.runtime.getURL`
/// builds `<origin>/<ext-id>/<path>` from this. WebView2 (Windows, the v1
/// target) serves custom schemes at `http(s)://<scheme>.localhost`; macOS/Linux
/// use `<scheme>://localhost`.
///
/// `use_https` MUST match the host app's window scheme (Tauri's
/// `useHttpsScheme`): WebView2 serves both the app and custom schemes at `https`
/// when the host opts in, and an `https` page cannot load an `http` resource
/// (mixed content). Callers pass [`crate::runtime::host_uses_https`].
pub fn resource_base_origin(use_https: bool) -> String {
    if cfg!(target_os = "windows") {
        let scheme = if use_https { "https" } else { "http" };
        format!("{scheme}://{RESOURCE_SCHEME}.localhost")
    } else {
        format!("{RESOURCE_SCHEME}://localhost")
    }
}

/// Why an extension-resource request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ResourceError {
    /// The path contained a `..` traversal segment.
    Traversal,
    /// The file is not declared in `web_accessible_resources`.
    NotWebAccessible,
    /// The request path was empty or had no extension-id segment.
    BadRequest,
}

/// Whether `rel_path` (extension-root-relative, forward-slashed) is allowed by
/// the `web_accessible_resources` resource entries. Each entry is a Chrome glob
/// (`*` = any sequence). An exact-string entry also matches verbatim.
pub fn is_web_accessible(resource_globs: &[String], rel_path: &str) -> bool {
    resource_globs.iter().any(|g| {
        g == rel_path
            || Glob::parse(g)
                .map(|glob| glob.matches(rel_path))
                .unwrap_or(false)
    })
}

/// Split a scheme request path of the form `/<ext-id>/<resource/path>` into its
/// id and the extension-root-relative resource path. Returns
/// [`ResourceError::BadRequest`] when either part is missing.
pub fn split_request_path(request_path: &str) -> Result<(String, String), ResourceError> {
    let trimmed = request_path.trim_start_matches('/');
    let (id, rest) = trimmed.split_once('/').ok_or(ResourceError::BadRequest)?;
    if id.is_empty() || rest.is_empty() {
        return Err(ResourceError::BadRequest);
    }
    Ok((id.to_string(), rest.to_string()))
}

/// Resolve a root-relative resource path to an absolute file under `source_dir`,
/// enforcing no-traversal and `web_accessible_resources` membership.
///
/// Does not itself check that the file exists — the scheme handler 404s on a
/// read error. It does reject traversal and non-WAR paths so a malicious page
/// cannot read arbitrary disk or non-exposed extension files.
pub fn resolve_resource(
    source_dir: &Path,
    resource_globs: &[String],
    rel_path: &str,
) -> Result<PathBuf, ResourceError> {
    let normalized = normalize_rel(rel_path)?;
    if !is_web_accessible(resource_globs, &normalized) {
        return Err(ResourceError::NotWebAccessible);
    }
    Ok(source_dir.join(normalized))
}

/// Resolve a **privileged** resource request: the extension's own background
/// webview reading a packaged file. Traversal-guarded like
/// [`resolve_resource`], but NOT `web_accessible_resources`-gated — a Chrome
/// service worker can `fetch`/`importScripts` any file in its own package,
/// even ones not exposed to web pages. The scheme handler only takes this
/// path when the requesting webview IS the extension's own `ext-bg-*` window
/// (see [`serve_mode`]); every other caller stays WAR-gated.
pub fn resolve_resource_privileged(
    source_dir: &Path,
    rel_path: &str,
) -> Result<PathBuf, ResourceError> {
    let normalized = normalize_rel(rel_path)?;
    Ok(source_dir.join(normalized))
}

/// Shared lexical normalization + traversal guard for the resolve fns.
fn normalize_rel(rel_path: &str) -> Result<String, ResourceError> {
    let normalized = rel_path.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    if normalized.is_empty() {
        return Err(ResourceError::BadRequest);
    }
    // Lexical traversal guard — reject before touching the filesystem.
    if normalized.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(ResourceError::Traversal);
    }
    Ok(normalized.to_string())
}

/// How the resource scheme should treat a request, decided by which webview
/// issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeMode {
    /// The request came from the extension's own background webview. That
    /// webview is the extension's privileged origin (the service-worker analog
    /// per D-001), so it may read any packaged file — including the background
    /// script itself and its `import`/`importScripts` chunks, which are not
    /// `web_accessible_resources`.
    Privileged,
    /// The request came from any other webview — a dapp page or the host's own
    /// frontend. Only `web_accessible_resources` are served, exactly as Chrome
    /// gates `chrome-extension://` reads from web pages.
    WebAccessible,
}

/// Decide the serve mode for a request: [`ServeMode::Privileged`] iff the
/// requesting webview label is the extension's own background-window label.
/// `owner_bg_label` is [`crate::runtime::bg_window_label`] of the extension the
/// request addresses; a BG webview that requests a *different* extension's id
/// is treated as an ordinary (WAR-gated) caller, so one extension's background
/// cannot read another's private files.
pub fn serve_mode(requesting_label: &str, owner_bg_label: &str) -> ServeMode {
    if !requesting_label.is_empty() && requesting_label == owner_bg_label {
        ServeMode::Privileged
    } else {
        ServeMode::WebAccessible
    }
}

/// Extension-root-relative resource path for a request that originated from the
/// extension's own background webview.
///
/// A service worker may reference its files **origin-absolute** (`/background.js`,
/// `/vendor/x.js` — exactly what Rabby's `importScripts("/background.js")` and
/// many bundlers emit). Chrome resolves those against `chrome-extension://<id>/`,
/// where the extension *is* the origin. Our scheme shares one origin across
/// extensions (`extres.localhost/<id>/...`), so a leading-slash path would drop
/// the `<id>` segment. Because a BG request is identified by its webview label
/// (not the URL), we know the owning extension and can treat the whole path as
/// root-relative: strip an `<id>/` prefix if the worker used `getURL` (which
/// includes it), otherwise take the path as-is under the extension root.
pub fn bg_request_rel<'a>(ext_id: &str, request_path: &'a str) -> &'a str {
    let p = request_path.trim_start_matches('/');
    let prefix = format!("{ext_id}/");
    p.strip_prefix(&prefix).unwrap_or(p)
}

/// Plan for loading an extension's background service worker over the resource
/// origin. Built by [`sw_load_plan`]; rendered to JS by [`sw_loader_script`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwLoadPlan {
    /// Absolute URL of the service-worker entry on the resource origin, e.g.
    /// `http://extres.localhost/<id>/background/sw.js`.
    pub url: String,
    /// Directory portion of `url` (with a trailing slash) — the base the
    /// `importScripts` shim resolves relative specifiers against, matching a
    /// worker's "resolve relative to the worker URL" rule.
    pub dir: String,
    /// Whether the worker is an ES module (`background.type == "module"`).
    pub module: bool,
}

/// Build a [`SwLoadPlan`] from the resource origin, extension id, the manifest's
/// `background.service_worker` relative path, and whether it is a module.
pub fn sw_load_plan(origin: &str, ext_id: &str, sw_rel: &str, module: bool) -> SwLoadPlan {
    // Normalize the relative entry the same way the scheme handler will: drop a
    // leading `./` or `/` and backslash-to-forward so the URL is well formed.
    let rel = sw_rel.replace('\\', "/");
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    let url = format!("{}/{}/{}", origin.trim_end_matches('/'), ext_id, rel);
    let dir = match url.rfind('/') {
        Some(i) => url[..=i].to_string(),
        None => url.clone(),
    };
    SwLoadPlan { url, dir, module }
}

/// Render the JS that bootstraps an extension's background service worker in the
/// hidden webview. It runs AFTER the chrome.* shim + bridge are in place and:
///
/// 1. Installs a synchronous `importScripts` shim (absent on a Window) that
///    fetches over the resource origin and evaluates in global scope, resolving
///    relative specifiers against the worker's directory — classic-worker
///    semantics. Requires the host CSP to allow `'unsafe-eval'` (see the
///    DECISIONS D-008 amendment); module workers below need no eval.
/// 2. Defines benign `ServiceWorkerGlobalScope` stubs (`skipWaiting`,
///    `clients`, `registration`) so worker bundles that touch them on the way
///    up don't throw.
/// 3. Starts the worker: `import(url)` for a module (native ES-module graph,
///    relative `import`s resolve against the resource origin), or
///    `importScripts(url)` for classic.
///
/// All values are embedded as JSON string/boolean literals, so an extension id
/// or path with quotes cannot break out of the script.
pub fn sw_loader_script(plan: &SwLoadPlan) -> String {
    let url_lit = serde_json::to_string(&plan.url).unwrap_or_else(|_| "\"\"".into());
    let dir_lit = serde_json::to_string(&plan.dir).unwrap_or_else(|_| "\"\"".into());
    let module_lit = if plan.module { "true" } else { "false" };
    format!(
        r#"
;(function () {{
  var SW_URL = {url_lit};
  var SW_DIR = {dir_lit};
  var IS_MODULE = {module_lit};

  // (1) Synchronous importScripts shim for classic workers. A Window has no
  // importScripts; emulate it with a synchronous fetch over the resource
  // origin + global-scope eval, resolving relative to the worker directory.
  var SW_ORIGIN = (function () {{ try {{ return new URL(SW_URL).origin; }} catch (e) {{ return ''; }} }})();
  if (typeof self.importScripts !== 'function') {{
    var importScriptsShim = function () {{
      for (var i = 0; i < arguments.length; i++) {{
        var spec = String(arguments[i]);
        var u;
        try {{ u = new URL(spec, SW_DIR).href; }} catch (e) {{ u = spec; }}
        // Re-root to the extension's resource origin when a worker's chunk URL
        // points elsewhere. Webpack auto-`publicPath` (and code keying off
        // `self.location`) resolves against the HOST DOCUMENT origin in this
        // document-based SW host — not the SW's extres URL — so chunk URLs come
        // out on the app origin and miss. The extres handler resolves a BG
        // request by its `ext-bg-*` webview label, so the path alone suffices.
        try {{
          var uu = new URL(u);
          if (SW_ORIGIN && uu.origin !== SW_ORIGIN) {{
            u = SW_ORIGIN + uu.pathname + uu.search;
          }}
        }} catch (e) {{}}
        var xhr = new XMLHttpRequest();
        xhr.open('GET', u, false); // synchronous — matches worker importScripts
        xhr.send(null);
        if (xhr.status && (xhr.status < 200 || xhr.status >= 300)) {{
          throw new Error('importScripts failed (' + xhr.status + '): ' + u);
        }}
        (0, eval)(xhr.responseText + '\n//# sourceURL=' + u);
      }}
    }};
    try {{
      Object.defineProperty(self, 'importScripts', {{
        value: importScriptsShim, configurable: true, writable: true,
      }});
    }} catch (e) {{ self.importScripts = importScriptsShim; }}
  }}

  // (2) Minimal ServiceWorkerGlobalScope stubs so worker bundles that poke at
  // lifecycle surfaces on the way up keep going. These are inert: this host
  // has no real SW lifecycle, but nothing the wallets do depends on it.
  if (typeof self.skipWaiting !== 'function') {{ self.skipWaiting = function () {{ return Promise.resolve(); }}; }}
  if (!self.clients) {{ self.clients = {{ claim: function () {{ return Promise.resolve(); }}, matchAll: function () {{ return Promise.resolve([]); }}, get: function () {{ return Promise.resolve(undefined); }} }}; }}
  // `self.serviceWorker` (the SW's own ServiceWorker self-reference, with
  // `.state`) exists in a ServiceWorkerGlobalScope but not on a Window. Report
  // it as already 'activated' so workers that gate boot on
  // `self.serviceWorker.state === 'activated'` (MetaMask) proceed.
  if (!self.serviceWorker) {{ self.serviceWorker = {{ state: 'activated', scriptURL: SW_URL, onstatechange: null, addEventListener: function () {{}}, removeEventListener: function () {{}}, postMessage: function () {{}} }}; }}
  if (!self.registration) {{ self.registration = {{ scope: SW_DIR, active: self.serviceWorker, installing: null, waiting: null, update: function () {{ return Promise.resolve(); }}, unregister: function () {{ return Promise.resolve(true); }} }}; }}

  // (2b) Synthetic service-worker lifecycle. A real SW receives `install` then
  // `activate`; many workers do their eager setup there (e.g. importScripts the
  // real controller) rather than at top level. This document host has no native
  // SW lifecycle, so fire them once the entry has registered its listeners.
  // ExtendableEvent.waitUntil is stubbed so handlers that call it don't throw.
  var fireLifecycle = function () {{
    ['install', 'activate'].forEach(function (type) {{
      try {{
        var ev = new Event(type);
        ev.waitUntil = function () {{}};
        self.dispatchEvent(ev);
      }} catch (e) {{}}
    }});
  }};

  // (3) Start the worker, then fire the lifecycle events.
  if (IS_MODULE) {{
    // Native ES-module graph; relative `import` specifiers resolve against the
    // resource origin. No eval needed — works under a strict (eval-free) CSP.
    import(SW_URL).then(fireLifecycle).catch(function (e) {{
      try {{ console.error('[tauri-plugin-extensions] module SW failed:', SW_URL, (e && e.message) || e); }} catch (_e) {{}}
    }});
  }} else {{
    try {{ self.importScripts(SW_URL); fireLifecycle(); }}
    catch (e) {{ try {{ console.error('[tauri-plugin-extensions] classic SW threw:', (e && e.message) || e, (e && e.stack) ? ('| ' + String(e.stack).slice(0, 400)) : ''); }} catch (_e) {{}} }}
  }}
}})();
"#
    )
}

/// Synchronously-maintained map from extension id to the data the resource
/// scheme handler needs: the on-disk root and the `web_accessible_resources`
/// globs. Managed in Tauri state alongside the read-only [`ExtensionRegistry`].
///
/// Why a second map instead of reusing the registry: the URI-scheme handler is
/// synchronous and the background webview fetches its service worker from the
/// resource origin *during boot* — before the async [`ExtensionRegistry`]
/// projection (`reproject`) catches up at the end of a load transition. The
/// [`LifecycleManager`] upserts here synchronously *before* it spawns the BG, so
/// the handler can always resolve a just-spawned worker's own files without
/// racing the projection.
#[derive(Default)]
pub struct ResourceRegistry {
    roots: DashMap<ExtensionId, ResourceRoot>,
}

/// One extension's serving data — its canonical root and WAR globs.
#[derive(Debug, Clone)]
pub struct ResourceRoot {
    /// Canonical on-disk root of the unpacked extension.
    pub source_dir: PathBuf,
    /// Flattened `web_accessible_resources` globs (page-reachable files).
    pub war_globs: Vec<String>,
}

impl ResourceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            roots: DashMap::new(),
        }
    }

    /// Insert or replace an extension's serving data.
    pub fn upsert(&self, id: ExtensionId, source_dir: PathBuf, war_globs: Vec<String>) {
        self.roots.insert(
            id,
            ResourceRoot {
                source_dir,
                war_globs,
            },
        );
    }

    /// Drop an extension's serving data (uninstall / unknown-on-resync).
    pub fn remove(&self, id: &ExtensionId) {
        self.roots.remove(id);
    }

    /// Look up an extension's serving data, cloned for use outside the lock.
    pub fn get(&self, id: &ExtensionId) -> Option<ResourceRoot> {
        self.roots.get(id).map(|r| r.value().clone())
    }

    /// Find the extension whose background-window label is `label`, returning
    /// its id + serving data. Lets the resource handler resolve a request by the
    /// *requesting BG webview* rather than the URL path — the basis for serving
    /// a worker's origin-absolute paths (see [`bg_request_rel`]). The BG label
    /// is `ext-bg-<first 12 chars of id>`; on the implausible 12-char-prefix
    /// collision, the first match wins (same caveat as the label scheme itself).
    pub fn find_by_bg_label(&self, label: &str) -> Option<(ExtensionId, ResourceRoot)> {
        self.roots
            .iter()
            .find(|r| crate::runtime::bg_window_label(r.key()) == label)
            .map(|r| (r.key().clone(), r.value().clone()))
    }

    /// Ids currently tracked — used by the lifecycle resync to prune stale ones.
    pub fn ids(&self) -> Vec<ExtensionId> {
        self.roots.iter().map(|r| r.key().clone()).collect()
    }
}

/// Best-effort MIME type from a file extension, for the `Content-Type` header.
/// Falls back to `application/octet-stream`. JS/WASM matter most for wallets.
pub fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("js") | Some("mjs") | Some("cjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("wat") | Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_glob_war_entries_match() {
        let war = vec!["pageProvider.js".to_string(), "assets/*.js".to_string()];
        assert!(is_web_accessible(&war, "pageProvider.js"));
        assert!(is_web_accessible(&war, "assets/inpage.js"));
        assert!(!is_web_accessible(&war, "background/serviceWorker.js"));
        assert!(!is_web_accessible(&war, "secrets.json"));
    }

    #[test]
    fn traversal_is_rejected() {
        let war = vec!["*".to_string()];
        assert_eq!(
            resolve_resource(Path::new("/ext"), &war, "../../etc/passwd"),
            Err(ResourceError::Traversal),
        );
        assert_eq!(
            resolve_resource(Path::new("/ext"), &war, "a/../../b.js"),
            Err(ResourceError::Traversal),
        );
    }

    #[test]
    fn non_war_path_is_forbidden_even_without_traversal() {
        let war = vec!["pageProvider.js".to_string()];
        assert_eq!(
            resolve_resource(Path::new("/ext"), &war, "background/serviceWorker.js"),
            Err(ResourceError::NotWebAccessible),
        );
    }

    #[test]
    fn allowed_resource_resolves_under_root() {
        let war = vec!["pageProvider.js".to_string()];
        let got = resolve_resource(Path::new("/ext"), &war, "pageProvider.js").unwrap();
        assert_eq!(got, Path::new("/ext").join("pageProvider.js"));
    }

    #[test]
    fn wildcard_war_allows_any_non_traversing_path() {
        let war = vec!["*".to_string()];
        assert!(resolve_resource(Path::new("/ext"), &war, "chunk-ABC.js").is_ok());
        assert!(resolve_resource(Path::new("/ext"), &war, "deep/nested/x.wasm").is_ok());
    }

    #[test]
    fn split_request_path_separates_id_and_resource() {
        assert_eq!(
            split_request_path("/bfnaelmomeim/evmAsk.js"),
            Ok(("bfnaelmomeim".to_string(), "evmAsk.js".to_string())),
        );
        assert_eq!(
            split_request_path("/id/dir/file.js"),
            Ok(("id".to_string(), "dir/file.js".to_string())),
        );
        assert_eq!(split_request_path("/only-id"), Err(ResourceError::BadRequest));
        assert_eq!(split_request_path("/"), Err(ResourceError::BadRequest));
    }

    #[test]
    fn content_type_covers_wallet_asset_kinds() {
        assert_eq!(content_type_for(Path::new("a.js")), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for(Path::new("a.wasm")), "application/wasm");
        assert_eq!(content_type_for(Path::new("a.json")), "application/json; charset=utf-8");
        assert_eq!(content_type_for(Path::new("a.unknown")), "application/octet-stream");
    }

    #[test]
    fn privileged_resolve_serves_non_war_files_but_still_guards_traversal() {
        // A background webview may read a file that is NOT web-accessible
        // (its own service worker, import chunks) — privileged resolve skips
        // the WAR gate...
        let got = resolve_resource_privileged(Path::new("/ext"), "background/sw.js").unwrap();
        assert_eq!(got, Path::new("/ext").join("background/sw.js"));
        // ...but traversal is still rejected.
        assert_eq!(
            resolve_resource_privileged(Path::new("/ext"), "../../etc/passwd"),
            Err(ResourceError::Traversal),
        );
        // Where the WAR-gated path would have refused the same private file.
        assert_eq!(
            resolve_resource(Path::new("/ext"), &["pageProvider.js".to_string()], "background/sw.js"),
            Err(ResourceError::NotWebAccessible),
        );
    }

    #[test]
    fn serve_mode_privileged_only_for_own_bg_label() {
        // Same label as the extension's own BG window → privileged.
        assert_eq!(serve_mode("ext-bg-abc123", "ext-bg-abc123"), ServeMode::Privileged);
        // A dapp / host window → WAR-gated.
        assert_eq!(serve_mode("main", "ext-bg-abc123"), ServeMode::WebAccessible);
        // A *different* extension's BG window asking for this id → WAR-gated
        // (no cross-extension private reads).
        assert_eq!(serve_mode("ext-bg-other0", "ext-bg-abc123"), ServeMode::WebAccessible);
        // Empty requesting label never grants privilege.
        assert_eq!(serve_mode("", "ext-bg-abc123"), ServeMode::WebAccessible);
        assert_eq!(serve_mode("", ""), ServeMode::WebAccessible);
    }

    #[test]
    fn resource_base_origin_matches_host_scheme() {
        // Must match the host app's useHttpsScheme: an https page can't load an
        // http resource (mixed content), and some wallets (Phantom EVM) only
        // inject on https/localhost origins.
        #[cfg(target_os = "windows")]
        {
            assert_eq!(resource_base_origin(false), "http://extres.localhost");
            assert_eq!(resource_base_origin(true), "https://extres.localhost");
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(resource_base_origin(false), "extres://localhost");
            assert_eq!(resource_base_origin(true), "extres://localhost");
        }
    }

    #[test]
    fn sw_load_plan_builds_resource_url_and_dir() {
        let plan = sw_load_plan("http://extres.localhost", "abc123", "background/sw.js", true);
        assert_eq!(plan.url, "http://extres.localhost/abc123/background/sw.js");
        assert_eq!(plan.dir, "http://extres.localhost/abc123/background/");
        assert!(plan.module);

        // A flat entry yields the extension root as the importScripts base.
        let flat = sw_load_plan("http://extres.localhost", "abc123", "sw.js", false);
        assert_eq!(flat.url, "http://extres.localhost/abc123/sw.js");
        assert_eq!(flat.dir, "http://extres.localhost/abc123/");
        assert!(!flat.module);

        // Leading `./` and `/` on the manifest path are normalized away.
        let dotted = sw_load_plan("http://extres.localhost", "id", "./bg.js", false);
        assert_eq!(dotted.url, "http://extres.localhost/id/bg.js");
    }

    #[test]
    fn sw_loader_script_module_uses_dynamic_import_no_eval() {
        let plan = sw_load_plan("http://extres.localhost", "id", "background/sw.js", true);
        let js = sw_loader_script(&plan);
        // Module path drives a dynamic import of the entry URL.
        assert!(js.contains("import(SW_URL)"));
        assert!(js.contains("http://extres.localhost/id/background/sw.js"));
        // The importScripts shim is defined but the module path doesn't call it.
        assert!(js.contains("importScriptsShim"));
    }

    #[test]
    fn sw_loader_script_classic_imports_entry_via_shim() {
        let plan = sw_load_plan("http://extres.localhost", "id", "sw.js", false);
        let js = sw_loader_script(&plan);
        // Classic path invokes the synchronous importScripts shim on the entry.
        assert!(js.contains("self.importScripts(SW_URL)"));
        assert!(js.contains("xhr.open('GET', u, false)"));
        // The importScripts base is the worker directory.
        assert!(js.contains("http://extres.localhost/id/"));
    }

    #[test]
    fn sw_loader_script_escapes_embedded_values() {
        // A hostile id/path can't break out of the string literal.
        let plan = sw_load_plan("http://extres.localhost", "x\"; window.pwned=1; //", "a.js", true);
        let js = sw_loader_script(&plan);
        assert!(!js.contains("window.pwned=1; //\""));
        assert!(js.contains("window.pwned=1"));
    }

    #[test]
    fn bg_request_rel_handles_absolute_and_geturl_paths() {
        let id = "bfnaelmomeimhlpmgjnjophhpkkoljpa";
        // Origin-absolute worker paths (Rabby's importScripts("/background.js"))
        // become root-relative — the leading slash does NOT drop into the
        // shared scheme origin.
        assert_eq!(bg_request_rel(id, "/background.js"), "background.js");
        assert_eq!(bg_request_rel(id, "/vendor/trezor/x.js"), "vendor/trezor/x.js");
        // A getURL-style path (which includes the id) has the id prefix stripped.
        assert_eq!(bg_request_rel(id, &format!("/{id}/background/sw.js")), "background/sw.js");
        // A relative-resolved chunk under the worker dir keeps its subpath.
        assert_eq!(bg_request_rel(id, &format!("/{id}/chunk.js")), "chunk.js");
    }

    #[test]
    fn resource_registry_find_by_bg_label() {
        use crate::runtime::bg_window_label;
        let reg = ResourceRegistry::new();
        let id = ExtensionId::new("bfnaelmomeimhlpmgjnjophhpkkoljpa");
        reg.upsert(id.clone(), PathBuf::from("/ext"), vec![]);
        let label = bg_window_label(&id);
        let (found_id, root) = reg.find_by_bg_label(&label).expect("found by label");
        assert_eq!(found_id, id);
        assert_eq!(root.source_dir, PathBuf::from("/ext"));
        assert!(reg.find_by_bg_label("ext-bg-nonexistent0").is_none());
    }

    #[test]
    fn sw_loader_fires_synthetic_lifecycle_events() {
        let plan = sw_load_plan("http://extres.localhost", "id", "sw.js", false);
        let js = sw_loader_script(&plan);
        assert!(js.contains("fireLifecycle"));
        assert!(js.contains("'install'") && js.contains("'activate'"));
    }

    #[test]
    fn resource_registry_upsert_get_remove_roundtrip() {
        let reg = ResourceRegistry::new();
        let id = ExtensionId::new("ext-1");
        assert!(reg.get(&id).is_none());
        reg.upsert(id.clone(), PathBuf::from("/ext"), vec!["a.js".to_string()]);
        let got = reg.get(&id).expect("present after upsert");
        assert_eq!(got.source_dir, PathBuf::from("/ext"));
        assert_eq!(got.war_globs, vec!["a.js".to_string()]);
        // Upsert replaces.
        reg.upsert(id.clone(), PathBuf::from("/ext2"), vec![]);
        assert_eq!(reg.get(&id).unwrap().source_dir, PathBuf::from("/ext2"));
        assert_eq!(reg.ids(), vec![id.clone()]);
        reg.remove(&id);
        assert!(reg.get(&id).is_none());
    }
}
