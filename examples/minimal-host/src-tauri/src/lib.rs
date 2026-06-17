//! MV3 Minimal Host — smallest Tauri v2 application that loads
//! `tauri-plugin-extensions` and exercises the noop-mv3 fixture.
//!
//! The host intentionally stays tiny: one window, a handful of commands that
//! wrap the plugin's public Rust surface, and a static HTML frontend. It is
//! the spike's manual acceptance harness — if the button flow here works, the
//! plugin works end-to-end.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_extensions::registry::{ExtensionId, ExtensionRegistry, ExtensionSummary};
use tracing::{info, warn, Level};

/// Cached absolute path to the noop-mv3 fixture directory, resolved at
/// startup so the frontend button doesn't have to guess where the fixture
/// lives.
#[derive(Debug, Clone)]
struct NoopFixturePath(PathBuf);

/// Cached absolute path to the repo root. Used to resolve sibling fixture
/// directories (Phantom, test-dapp) without repeating the walk-up dance in
/// every command.
#[derive(Debug, Clone)]
struct RepoRoot(PathBuf);

/// Shared slot that receives the test-dapp's probe result via the
/// [`minimal_host_record_probe`] command. Read back by the main window's
/// auto-acceptance run via [`minimal_host_probe_dapp`].
#[derive(Debug, Default)]
struct ProbeSlot(Mutex<Option<serde_json::Value>>);

/// Slot that receives a dapp snapshot over the `mv3report://` URI scheme — an
/// `Image().src`/`fetch` network request, which SURVIVES SES `lockdown()` (it
/// does not freeze DOM/network ops, unlike `location.hash` + `btoa`, which
/// MetaMask's page-realm hardening breaks). The dapp introspection posts the
/// base64'd snapshot to `https://mv3report.localhost/<b64>` and Rust reads it
/// here. `hits`/`last_raw` are diagnostics: they record EVERY request the
/// scheme sees (even undecodable ones) so we can tell "beacon never fired" from
/// "beacon fired but payload was malformed".
#[derive(Debug, Default)]
struct ReportSlot {
    value: Mutex<Option<serde_json::Value>>,
    hits: std::sync::atomic::AtomicU32,
    last_raw: Mutex<Option<String>>,
}

/// Serializable wrapper over [`PluginError`] for IPC. The plugin already
/// implements `Serialize` as a flat string, but we wrap it once more to keep
/// the host's IPC contract independent of the plugin's error changes.
fn err_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Return the absolute path to the committed noop-mv3 fixture directory.
///
/// Used by the frontend to populate the "Load noop-mv3" button argument
/// without hardcoding a relative path that breaks between `pnpm tauri dev`
/// and a bundled release.
#[tauri::command]
fn minimal_host_noop_fixture_path(state: State<'_, NoopFixturePath>) -> String {
    state.0.to_string_lossy().into_owned()
}

/// Summary envelope returned by [`minimal_host_load_unpacked`] — the IPC layer
/// wants `serde_json::Value`-friendly types, so we re-surface the plugin's
/// [`ExtensionId`] alongside a [`ExtensionSummary`] snapshot.
#[derive(Debug, Serialize)]
struct LoadResult {
    id: ExtensionId,
    summary: Option<ExtensionSummary>,
}

/// Load an unpacked MV3 extension by absolute path. Returns the minted
/// [`ExtensionId`] and a compact summary pulled out of the registry after the
/// insert lands.
#[tauri::command]
async fn minimal_host_load_unpacked(
    app: AppHandle,
    path: String,
) -> Result<LoadResult, String> {
    let abs = Path::new(&path);
    let id = tauri_plugin_extensions::load_unpacked(&app, abs)
        .await
        .map_err(err_string)?;

    let registry = app.state::<ExtensionRegistry>();
    let summary = registry
        .list()
        .into_iter()
        .find(|s| s.id == id);

    Ok(LoadResult { id, summary })
}

/// List every loaded extension — a thin pass-through over the plugin's
/// [`ExtensionRegistry::list`]. Exposed so the frontend can reconcile state
/// after a hot-reload.
#[tauri::command]
fn minimal_host_list_extensions(
    registry: State<'_, ExtensionRegistry>,
) -> Vec<ExtensionSummary> {
    registry.list()
}

/// Health/ping probe for the IPC layer. Lets the frontend verify its
/// plumbing before it attempts a real load.
#[tauri::command]
fn minimal_host_ping() -> String {
    "pong".to_string()
}

/// End-to-end background-surface probe. Three layers, strongest last:
///
///   1. The plugin's registry has the extension cached — the loader
///      completed.
///   2. The hidden per-extension `WebviewWindow` exists (label
///      `ext-bg-<short>`) — `spawn_background` ran.
///   3. A real `chrome.runtime` message round-trip: the host sends
///      `{kind: "ping"}` into the BG worker via the plugin's message router
///      and awaits the worker's `sendResponse` — the full
///      eval-dispatch → shim onMessage → response-invoke path.
///
/// Returns a structured summary (serialized to JSON) describing all three.
#[tauri::command]
async fn minimal_host_ping_background(
    app: AppHandle,
    extension_id: String,
) -> Result<String, String> {
    let id = ExtensionId::new(extension_id.clone());
    let registry = app.state::<ExtensionRegistry>();
    let in_registry = registry.list().iter().any(|s| s.id == id);

    // The backend labels hidden windows as `ext-bg-<first 12 chars of id>`.
    let expected_label_prefix = format!("ext-bg-{}", &extension_id.chars().take(12).collect::<String>());
    let bg_window_label = app
        .webview_windows()
        .keys()
        .find(|k| k.starts_with(&expected_label_prefix))
        .cloned();

    let round_trip = match tauri_plugin_extensions::send_message_to_background(
        &app,
        &id,
        serde_json::json!({ "kind": "ping", "from": "minimal-host" }),
    )
    .await
    {
        Ok(response) => serde_json::json!({ "ok": true, "response": response }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };

    serde_json::to_string(&serde_json::json!({
        "extensionId": extension_id,
        "inRegistry": in_registry,
        "bgWindowLabel": bg_window_label,
        "bgWindowPresent": bg_window_label.is_some(),
        "roundTrip": round_trip,
    }))
    .map_err(err_string)
}

/// Send an arbitrary `chrome.runtime` message to an extension's background and
/// return its response. Used by the acceptance harness to drive wallet setup
/// (e.g. Rabby's `{type:"controller", method:"boot"/"importWatchAddress"}`) —
/// the realistic onboarding a user does before a dapp can query the wallet.
#[tauri::command]
async fn minimal_host_send_bg_message(
    app: AppHandle,
    extension_id: String,
    message: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = ExtensionId::new(extension_id);
    tauri_plugin_extensions::send_message_to_background(&app, &id, message)
        .await
        .map_err(err_string)
}

/// Eval an introspection snapshot inside an extension's hidden BG webview.
/// The snapshot (shim wiring flags + the BG error buffer the plugin's
/// bridge maintains) is handed back through [`minimal_host_record_probe`];
/// callers read it via [`minimal_host_probe_dapp`] after a short delay.
/// Diagnostic tooling for headless BG workers — eval cannot return values.
#[tauri::command]
async fn minimal_host_introspect_bg(
    app: AppHandle,
    extension_id: String,
) -> Result<String, String> {
    let prefix = format!(
        "ext-bg-{}",
        extension_id.chars().take(12).collect::<String>()
    );
    let label = app
        .webview_windows()
        .keys()
        .find(|k| k.starts_with(&prefix))
        .cloned()
        .ok_or_else(|| format!("no BG window with prefix {prefix}"))?;
    let win = app
        .get_webview_window(&label)
        .ok_or_else(|| format!("BG window '{label}' vanished"))?;
    win.eval(
        r#"
(function () {
  var snap = {
    bgIntrospect: true,
    href: location.href,
    hasDispatch: !!window.__extEventDispatch,
    hasEventApi: !!(window.__TAURI_EVENT__ && window.__TAURI_EVENT__.listen),
    hasInternals: !!(window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke),
    bootstrapped: !!window.__tauri_ext_background_bootstrapped,
    configureType: typeof window.__tauri_ext_configure,
    chromeRuntimeType: typeof (window.chrome && window.chrome.runtime),
    extId: (window.chrome && window.chrome.runtime) ? window.chrome.runtime.id : null,
    errors: window.__extLastErrors || null,
    apiGaps: window.__extApiGaps || null,
    fixtureResult: (typeof window.__fixtureResult !== 'undefined') ? window.__fixtureResult : null,
    inboundCount: window.__extInboundCount || null,
    onConnectExternalType: (window.chrome && window.chrome.runtime) ? typeof window.chrome.runtime.onConnectExternal : 'no-runtime',
    browserOCE: (function(){try{return typeof self.browser.runtime.onConnectExternal;}catch(e){return 'err:'+(e&&e.message||e);}})(),
    browserIsChrome: (function(){try{return self.browser===self.chrome;}catch(e){return 'err';}})(),
    selfServiceWorker: (typeof self.serviceWorker !== 'undefined') ? (self.serviceWorker && self.serviceWorker.state) : 'absent',
  };
  if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
    window.__TAURI_INTERNALS__.invoke('minimal_host_record_probe', { probe: snap })
      .catch(function () {});
  }
})();
"#,
    )
    .map_err(err_string)?;
    Ok(label)
}

/// Authoritative provider readback via the **document.title channel**.
///
/// A wallet's MAIN-world inpage script can break the page's Tauri IPC:
/// MetaMask runs SES/LavaMoat `lockdown()`, which hardens the realm and
/// stops `__TAURI_INTERNALS__.invoke` from working — so neither the page's
/// own probe nor an invoke-based eval-readback can report. `document.title`
/// is a DOM property SES does not freeze, and Rust can read it directly with
/// `WebviewWindow::title()`. We eval a snapshot of `window.ethereum` + the
/// EVM/Solana globals into the page, stamp it onto `document.title` behind a
/// sentinel prefix, then poll the title back out. Bypasses IPC entirely.
#[tauri::command]
async fn minimal_host_introspect_dapp(app: AppHandle) -> Result<serde_json::Value, String> {
    use base64::Engine as _;
    const MARK: &str = "MV3PROBE=";
    let label = "test-dapp".to_string();
    let win = app
        .get_webview_window(&label)
        .ok_or_else(|| format!("no '{label}' window"))?;
    // Drain any stale report/diagnostics from a previous probe so we only read
    // this run's (hits then counts THIS page's beacons only).
    if let Some(slot) = app.try_state::<ReportSlot>() {
        if let Ok(mut g) = slot.value.lock() {
            *g = None;
        }
        slot.hits.store(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut g) = slot.last_raw.lock() {
            *g = None;
        }
    }
    // Channel: the page base64-encodes the snapshot into `location.hash`, which
    // Rust reads back via `webview.url()`. Unlike document.title (which is the
    // native window title in Tauri, not the page's), and unlike an IPC invoke
    // (which MetaMask's SES lockdown breaks), `location.hash` survives both and
    // is reflected in the webview URL.
    win.eval(
        r#"
(async function () {
  // Catch-all error beacon: if ANYTHING below throws on a locked-down page,
  // report the error itself over the network beacon (b64url of a JSON blob).
  function beaconErr(tag, e) {
    try {
      var j = JSON.stringify({ dappIntrospect: true, evalError: tag + ': ' + ((e && e.message) || String(e)), chainId: null });
      var b = btoa(j).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
      (new Image()).src = 'https://mv3report.localhost/' + b + '?e=1';
    } catch (e2) {}
  }
  try {
  // Minimal presence probe FIRST, before any code SES/LavaMoat lockdown could
  // interfere with, so a locked-down page (MetaMask) at least reports whether a
  // provider exists even if the richer snapshot below can't be built/published.
  try {
    location.hash = 'MV3PROBE=' + btoa(JSON.stringify({
      dappIntrospect: true, early: true, chainId: { pending: true },
      ethereum: (function () { try { return !!window.ethereum; } catch (e) { return false; } })(),
      ethereumIsMetaMask: (function () { try { return !!(window.ethereum && window.ethereum.isMetaMask); } catch (e) { return false; } })(),
      eip6963Count: (window.__eip6963Count | 0),
      pageErrors: (function () { try { return (window.__extPageErrors || []).slice(0, 6); } catch (e) { return ['tap-err']; } })(),
      winHint: (function () { try { return Object.getOwnPropertyNames(window).filter(function (k) { return /ethereum|metamask|evmAsk|inpage|mmStream|__extPage/i.test(k); }).slice(0, 10); } catch (e) { return ['keys-err']; } })()
    }));
  } catch (e) {}
  function flag(o, k) { try { return !!(o && o[k]); } catch (e) { return false; } }
  var eth = null, sol = null, phantom = null;
  try { eth = window.ethereum; } catch (e) {}
  try { sol = window.solana; } catch (e) {}
  try { phantom = window.phantom; } catch (e) {}
  var snap = {
    dappIntrospect: true,
    href: location.href,
    hasInternals: !!(window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke),
    ethereum: flag(window, 'ethereum'),
    ethereumIsMetaMask: flag(eth, 'isMetaMask'),
    ethereumIsRabby: flag(eth, 'isRabby'),
    ethereumIsPhantom: flag(eth, 'isPhantom'),
    rabbyGlobal: flag(window, 'rabby'),
    phantomPresent: flag(window, 'phantom'),
    phantomEthereum: flag(phantom, 'ethereum'),
    phantomSolana: flag(phantom, 'solana'),
    solana: flag(window, 'solana'),
    mainWorldFixture: flag(window, '__fixtureMainWorldInjected'),
    eip6963Count: (window.__eip6963Count | 0),
    eip6963Rdns: (window.__eip6963Rdns || []),
    pageErrors: (window.__extPageErrors || []).slice(0, 8)
  };
  // Transport diagnostics: which content-script/provider artifacts reached the
  // page, and what the provider exposes — pinpoints where a wallet's
  // page↔content↔BG round-trip stalls.
  try {
    snap.diag = {
      winKeys: Object.keys(window).filter(function (k) {
        return /rabby|metamask|browser|inpage|content|stream|ethereum|solana|phantom|__ext/i.test(k);
      }).slice(0, 40),
      ethereumKeys: (function () { try { return window.ethereum ? Object.keys(window.ethereum).slice(0, 30) : null; } catch (e) { return null; } })(),
      hasBrowser: (typeof window.browser !== 'undefined'),
      extDispatch: !!window.__extEventDispatch,
      outboundCount: window.__extOutboundCount || null,
      provider: (function () {
        try {
          var e = window.ethereum; if (!e) return null;
          return {
            isReady: e._isReady, isConnected: e._isConnected, initialized: e._initialized,
            isUnlocked: e._isUnlocked, chainId: e.chainId, networkVersion: e.networkVersion,
            cachedReqs: (e._cacheRequestsBeforeReady && e._cacheRequestsBeforeReady.length),
            state: e._state,
          };
        } catch (x) { return { provErr: String(x) }; }
      })(),
    };
  } catch (e) { snap.diag = { diagError: String(e) }; }
  // Publish helper — reports the snapshot back over (1) location.hash and
  // (2) an Image/fetch to mv3report:// . The latter SURVIVES SES lockdown
  // (DOM/network ops aren't frozen), which MetaMask's page-realm hardening
  // breaks for the location.hash + btoa path.
  var __rc = 0;
  // URL-safe base64 (no '+' '/' '=') — those get percent-encoded in a WebView2
  // URL path, which corrupts the payload before Rust can decode it. The hash
  // channel keeps standard base64 (read from the fragment, not the path).
  function b64url(b) { return b.replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''); }
  function publish(s) {
    var data;
    try { data = btoa(JSON.stringify(s)); }
    catch (e) { data = btoa('{"dappIntrospect":true,"snapError":true,"ethereum":' + flag(window, 'ethereum') + '}'); }
    var bdata = b64url(data);
    try { location.hash = 'MV3PROBE=' + data; } catch (e) {}
    try { (new Image()).src = 'https://mv3report.localhost/' + bdata + '?t=' + (++__rc); } catch (e) {}
    try { fetch('https://mv3report.localhost/' + bdata + '?f=' + (++__rc)).catch(function(){}); } catch (e) {}
  }
  // Publish provider PRESENCE synchronously first — so a hung eth_chainId (a
  // provider whose background can't answer) doesn't strand the whole readback.
  snap.chainId = { pending: true };
  publish(snap);
  // Then attempt the eth_chainId round-trip and re-publish with the result.
  (async function () {
    try {
      var p = null;
      try { p = window.ethereum; } catch (e) {}
      if (p && typeof p.request === 'function') {
        var chainId = await Promise.race([
          p.request({ method: 'eth_chainId' }),
          new Promise(function (_res, rej) { setTimeout(function () { rej(new Error('eth_chainId timed out after 6s')); }, 6000); })
        ]);
        snap.chainId = { ok: true, value: chainId };
      } else {
        snap.chainId = null;
      }
    } catch (e) {
      snap.chainId = { ok: false, error: (e && e.message) || String(e) };
    }
    publish(snap);
  })();
  } catch (e) { beaconErr('outer', e); }
})();
"#,
    )
    .map_err(err_string)?;

    // Poll the webview URL fragment for the snapshot. The page publishes
    // presence immediately (chainId `{pending:true}`), then re-publishes once
    // eth_chainId resolves; keep the latest and prefer a resolved chainId, but
    // never lose the presence snapshot to a hung RPC. Deadline outlasts the 6s
    // eth_chainId race.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(9);
    let mut latest: Option<serde_json::Value> = None;
    let report_slot = app.try_state::<ReportSlot>();
    // A `chainId` is "resolved" once it is no longer the synchronous
    // `{pending:true}` placeholder — i.e. eth_chainId returned, errored, or the
    // provider was absent (`null`).
    let is_resolved = |val: &serde_json::Value| -> bool {
        val.get("chainId")
            .map(|c| c.get("pending").is_none())
            .unwrap_or(true)
    };
    loop {
        // Channel A — SES-surviving mv3report:// network beacon. Preferred,
        // because MetaMask's lockdown() leaves location.hash + btoa unusable.
        if let Some(slot) = report_slot.as_ref() {
            if let Some(val) = slot.value.lock().ok().and_then(|g| g.clone()) {
                let resolved = is_resolved(&val);
                latest = Some(val);
                if resolved {
                    return Ok(latest.unwrap());
                }
            }
        }
        // Channel B — location.hash fragment (works on non-locked-down pages).
        if let Ok(url) = win.url() {
            if let Some(frag) = url.fragment() {
                if let Some(b64) = frag.strip_prefix(MARK) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
                        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            let resolved = is_resolved(&val);
                            latest = Some(val);
                            if resolved {
                                return Ok(latest.unwrap());
                            }
                        }
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            if let Some(val) = latest {
                return Ok(val); // presence snapshot, even if eth_chainId hung
            }
            let url = win.url().map(|u| u.to_string()).unwrap_or_default();
            // Beacon diagnostics — distinguishes "mv3report:// never fired" (CSP
            // / scheme problem) from "fired but undecodable" (payload problem).
            let (beacon_hits, beacon_last_raw) = match report_slot.as_ref() {
                Some(slot) => (
                    slot.hits.load(std::sync::atomic::Ordering::Relaxed),
                    slot.last_raw.lock().ok().and_then(|g| g.clone()),
                ),
                None => (0, None),
            };
            return Ok(serde_json::json!({
                "dappIntrospect": false,
                "hashTimeout": true,
                "windowUrl": url,
                "beaconHits": beacon_hits,
                "beaconLastRaw": beacon_last_raw,
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
}

/// Return the labels of every `WebviewWindow` currently open. Useful for
/// the acceptance-report step to prove a hidden `ext-bg-*` window materialized.
#[tauri::command]
fn minimal_host_list_webview_windows(app: AppHandle) -> Vec<String> {
    app.webview_windows().into_keys().collect()
}

/// Read the mv3report beacon slot WITHOUT clearing it — diagnostics for pages
/// whose beacons land before `minimal_host_introspect_dapp` (which clears the
/// slot when it starts) gets called.
#[tauri::command]
fn minimal_host_read_report(app: AppHandle) -> serde_json::Value {
    match app.try_state::<ReportSlot>() {
        Some(slot) => serde_json::json!({
            "hits": slot.hits.load(std::sync::atomic::Ordering::Relaxed),
            "lastRaw": slot.last_raw.lock().ok().and_then(|g| g.clone()),
            "value": slot.value.lock().ok().and_then(|g| g.clone()),
        }),
        None => serde_json::json!({ "noSlot": true }),
    }
}

/// Persist an acceptance-report JSON blob to a known path on disk so external
/// tooling (including the developer driving this session) can read results
/// without needing to screenshot the log pane.
///
/// Writes to `<repo>/examples/minimal-host/acceptance-report.json` — i.e. one
/// level up from this crate's `CARGO_MANIFEST_DIR`.
#[tauri::command]
fn minimal_host_write_acceptance_report(report_json: String) -> Result<String, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dest = manifest_dir
        .parent()
        .ok_or_else(|| "no parent for CARGO_MANIFEST_DIR".to_string())?
        .join("acceptance-report.json");
    std::fs::write(&dest, report_json).map_err(err_string)?;
    info!(report = %dest.display(), "acceptance report written");
    Ok(dest.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Phantom phase — D-005 v1 acceptance wiring.
//
// The noop-mv3 phase above exercises the plugin against our own synthetic
// fixture. The commands below exercise it against Phantom — the real wallet
// that v1 acceptance is gated on. They resolve the Phantom fixture path,
// open a second WebviewWindow pointed at our `fixtures/test-dapp/index.html`
// canary, and collect the probe result the dapp posts back via
// `minimal_host_record_probe`.
// ---------------------------------------------------------------------------

/// Resolve the absolute path to the unpacked Phantom fixture directory.
///
/// Returns an error string when the fixture is absent (the vendor extension
/// is gitignored, so a fresh clone needs `scripts/fetch-phantom.ps1` to
/// populate it). This lets the frontend distinguish "Phantom not fetched"
/// from "Phantom load failed," both of which would otherwise look like a
/// missing directory at `load_unpacked` time.
#[tauri::command]
fn minimal_host_phantom_fixture_path(repo_root: State<'_, RepoRoot>) -> Result<String, String> {
    let candidate = repo_root
        .0
        .join("fixtures")
        .join("test-extensions")
        .join("phantom");
    let manifest = candidate.join("manifest.json");
    if !manifest.exists() {
        return Err(format!(
            "Phantom fixture not populated — expected manifest at {}. Run scripts/fetch-phantom.ps1 to fetch.",
            manifest.display()
        ));
    }
    Ok(candidate.to_string_lossy().into_owned())
}

/// Resolve the absolute path to an unpacked wallet fixture directory under
/// `fixtures/test-extensions/<name>`. Shared by the MetaMask / Rabby
/// resolvers below; mirrors the Phantom resolver's "not fetched" error so the
/// frontend can tell "wallet not fetched" from "wallet load failed".
fn resolve_wallet_fixture(repo_root: &Path, name: &str) -> Result<String, String> {
    let candidate = repo_root
        .join("fixtures")
        .join("test-extensions")
        .join(name);
    let manifest = candidate.join("manifest.json");
    if !manifest.exists() {
        return Err(format!(
            "{name} fixture not populated — expected manifest at {}. Run scripts/fetch-{name}.ps1 to fetch.",
            manifest.display()
        ));
    }
    Ok(candidate.to_string_lossy().into_owned())
}

/// Resolve the absolute path to the unpacked MetaMask fixture directory.
#[tauri::command]
fn minimal_host_metamask_fixture_path(repo_root: State<'_, RepoRoot>) -> Result<String, String> {
    resolve_wallet_fixture(&repo_root.0, "metamask")
}

/// Resolve the absolute path to the unpacked Rabby fixture directory.
#[tauri::command]
fn minimal_host_rabby_fixture_path(repo_root: State<'_, RepoRoot>) -> Result<String, String> {
    resolve_wallet_fixture(&repo_root.0, "rabby")
}

/// Resolve the absolute path to a committed BG-host test fixture under
/// `fixtures/test-extensions/<name>`. These (module-import / classic-importscripts
/// / module-register-content) are checked in, so unlike the wallet resolvers a
/// missing one is a genuine error, not a "not fetched" hint. The `name` is
/// validated to a single path segment so it can't escape the fixtures dir.
#[tauri::command]
fn minimal_host_bg_fixture_path(
    repo_root: State<'_, RepoRoot>,
    name: String,
) -> Result<String, String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!("invalid fixture name: {name}"));
    }
    let candidate = repo_root
        .0
        .join("fixtures")
        .join("test-extensions")
        .join(&name);
    let manifest = candidate.join("manifest.json");
    if !manifest.exists() {
        return Err(format!(
            "BG-host fixture '{name}' missing — expected manifest at {}",
            manifest.display()
        ));
    }
    Ok(candidate.to_string_lossy().into_owned())
}

/// Return the app-origin path of the canary test-dapp page. Served from the
/// host's own dist (`test-dapp.html`, a synced copy of
/// `fixtures/test-dapp/index.html`) rather than a `file://` URL: Tauri only
/// injects `__TAURI_INTERNALS__` into app-origin pages, and the dapp needs
/// it to post its probe back via [`minimal_host_record_probe`]. The earlier
/// `file://` variant ran the probe fine but could never report
/// ("probe: null" in old acceptance runs).
#[tauri::command]
fn minimal_host_test_dapp_url() -> Result<String, String> {
    Ok("test-dapp.html".to_string())
}

/// Open (or focus) a second `WebviewWindow` labelled `"test-dapp"` pointed
/// at the given URL. Leaves the main minimal-host window alone so the
/// acceptance log pane stays visible while the probe runs.
///
/// Returns the window label so the frontend can correlate subsequent
/// observations (e.g. `list_webview_windows`) with the window it just
/// opened.
#[tauri::command]
async fn minimal_host_open_test_dapp_window(
    app: AppHandle,
    url: String,
) -> Result<String, String> {
    let label = "test-dapp".to_string();

    // Idempotency: if the label already exists, close it first so reloads
    // navigate the fresh URL rather than showing stale content. The eval-
    // navigate-in-place variant tripped a wry `InvalidUri(InvalidFormat)`
    // panic on 2026-04-20; sticking with close+sleep+rebuild. Probe slot
    // is cleared so we don't read stale data from the previous session.
    // Clear the probe slot unconditionally — other diagnostics (the BG
    // introspection step) share it, and a stale snapshot here would be
    // mistaken for the dapp's report by the read_probe poll.
    if let Ok(mut guard) = app.state::<ProbeSlot>().0.lock() {
        *guard = None;
    }
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.close();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    // App-relative paths (no scheme) resolve against the Tauri app origin —
    // the test-dapp's normal home. Full URLs still open as external pages
    // so ad-hoc probing of remote dapps keeps working.
    let webview_url = if url.contains("://") {
        WebviewUrl::External(
            url.parse()
                .map_err(|e| format!("invalid test-dapp URL '{url}': {e}"))?,
        )
    } else {
        WebviewUrl::App(PathBuf::from(&url))
    };

    // Tauri v2 requires WebviewWindow build calls on the main thread. Use
    // the same std mpsc pattern the plugin's webview2 backend uses so we
    // return the build result cleanly from an async command.
    let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<(), tauri::Error>>();
    let app_for_closure = app.clone();
    let label_for_build = label.clone();
    app.run_on_main_thread(move || {
        let builder =
            WebviewWindowBuilder::new(&app_for_closure, &label_for_build, webview_url)
                .title("MV3 Test Dapp (Phantom canary)")
                .inner_size(960.0, 720.0)
                .resizable(true)
                // Match the main window's https scheme so the dapp shares the app
                // origin (`https://tauri.localhost`) — Phantom's EVM provider only
                // injects on https/localhost origins.
                .use_https_scheme(true)
                .decorations(true);
        let outcome = builder.build().map(|_| ());
        let _ = tx.send(outcome);
    })
    .map_err(err_string)?;

    rx.recv()
        .map_err(|e| format!("main-thread channel dropped: {e}"))?
        .map_err(err_string)?;

    Ok(label)
}

/// Called by the test-dapp page itself (running inside the `"test-dapp"`
/// webview window) to hand its probe result back to the host. The main
/// window's auto-acceptance run then reads this via
/// [`minimal_host_probe_dapp`].
#[tauri::command]
fn minimal_host_record_probe(
    probe: serde_json::Value,
    slot: State<'_, ProbeSlot>,
) -> Result<(), String> {
    let mut guard = slot.0.lock().map_err(|e| format!("probe slot poisoned: {e}"))?;
    info!(probe = %probe, "probe recorded from test-dapp");
    *guard = Some(probe);
    Ok(())
}

/// Read back whatever the test-dapp last handed to
/// [`minimal_host_record_probe`]. Returns `null` when the dapp hasn't
/// reported yet (i.e. the webview didn't load, or the probe timed out
/// before its `recordToHost` invoke finished).
#[tauri::command]
fn minimal_host_probe_dapp(
    slot: State<'_, ProbeSlot>,
) -> Result<serde_json::Value, String> {
    let guard = slot.0.lock().map_err(|e| format!("probe slot poisoned: {e}"))?;
    Ok(guard.clone().unwrap_or(serde_json::Value::Null))
}

// Note: the frontend invokes Agent G's `extensions_diagnostics` command
// directly (see dist/main.js). If Agent G hasn't landed when this binary
// runs, Tauri surfaces "command not found" and the acceptance report
// records that verbatim — no Rust-side shim needed here.

/// Resolve the noop-mv3 fixture path.
///
/// Order of resolution:
///   1. `$MV3_NOOP_FIXTURE_PATH` env override (useful for CI and the
///      integration tests that copy the fixture into a temp dir).
///   2. `<CARGO_MANIFEST_DIR>/../../../fixtures/test-extensions/noop-mv3`
///      when the binary was built from the repo's `examples/minimal-host`
///      workspace — this is the normal `pnpm tauri dev` / `pnpm tauri build`
///      path.
///   3. `<current_dir>/fixtures/test-extensions/noop-mv3` as a last-ditch
///      fallback for a bundled release installed alongside the fixtures
///      directory.
fn resolve_noop_fixture_path() -> PathBuf {
    if let Ok(p) = std::env::var("MV3_NOOP_FIXTURE_PATH") {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }

    // CARGO_MANIFEST_DIR is resolved at compile time and points at
    // `examples/minimal-host/src-tauri/`. Walk up three times to reach the
    // repo root, then into `fixtures/...`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let from_manifest = manifest_dir
        .parent() // examples/minimal-host
        .and_then(Path::parent) // examples
        .and_then(Path::parent) // repo root
        .map(|root| root.join("fixtures").join("test-extensions").join("noop-mv3"));
    if let Some(p) = from_manifest {
        if p.exists() {
            return p;
        }
    }

    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("fixtures")
        .join("test-extensions")
        .join("noop-mv3")
}

/// Resolve the repo root by walking up from `CARGO_MANIFEST_DIR`. Same walk
/// the noop fixture resolver does — factored out so Phantom + test-dapp
/// fixtures don't each duplicate it.
fn resolve_repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // examples/minimal-host
        .and_then(Path::parent) // examples
        .and_then(Path::parent) // repo root
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            warn!("CARGO_MANIFEST_DIR has no grandparent; falling back to cwd");
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        })
}

/// Start the Tauri event loop. Call from `main.rs`.
pub fn run() {
    // Structured logging to stderr. Respects RUST_LOG.
    let _ = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let fixture = resolve_noop_fixture_path();
    info!(noop_fixture = %fixture.display(), "resolved noop-mv3 fixture path");

    let repo_root = resolve_repo_root();
    info!(repo_root = %repo_root.display(), "resolved repo root");

    tauri::Builder::default()
        .plugin(tauri_plugin_extensions::init())
        .manage(NoopFixturePath(fixture))
        .manage(RepoRoot(repo_root))
        .manage(ProbeSlot::default())
        .manage(ReportSlot::default())
        // SES-surviving readback: the dapp posts its snapshot as
        // `https://mv3report.localhost/<base64-json>` via Image/fetch; capture
        // and decode it into ReportSlot. Used to read MetaMask's page (whose
        // lockdown() breaks the location.hash channel).
        .register_uri_scheme_protocol("mv3report", |ctx, request| {
            use base64::Engine as _;
            use tauri::http::{header, Response, StatusCode};
            let ok = |status: StatusCode| {
                Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "image/gif")
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .body(Vec::<u8>::new())
                    .unwrap_or_else(|_| Response::new(Vec::new()))
            };
            // Path is `/<base64>` (a `?t=` cache-buster may trail in the query).
            let raw = request.uri().path().trim_start_matches('/').to_string();
            if let Some(slot) = ctx.app_handle().try_state::<ReportSlot>() {
                slot.hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut g) = slot.last_raw.lock() {
                    *g = Some(raw.chars().take(64).collect());
                }
                // The page sends URL-safe base64 (no '+' '/' '='); decode with
                // the matching alphabet. Strip any stray '=' padding defensively.
                let cleaned = raw.trim_end_matches('=');
                match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(cleaned) {
                    Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(val) => {
                            if let Ok(mut g) = slot.value.lock() {
                                *g = Some(val);
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = slot.last_raw.lock() {
                                *g = Some(format!("JSON_ERR:{e}"));
                            }
                        }
                    },
                    Err(e) => {
                        if let Ok(mut g) = slot.last_raw.lock() {
                            *g = Some(format!(
                                "B64_ERR:{e}:{}",
                                raw.chars().take(48).collect::<String>()
                            ));
                        }
                    }
                }
            }
            ok(StatusCode::OK)
        })
        .setup(|app| {
            // Surface the main window label for debugging. Tauri v2 lifts
            // window creation to `tauri.conf.json`, so there's nothing to
            // build here beyond observation.
            if let Some(win) = app.get_webview_window("main") {
                info!(label = %win.label(), "main window ready");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            minimal_host_ping,
            minimal_host_noop_fixture_path,
            minimal_host_load_unpacked,
            minimal_host_list_extensions,
            minimal_host_ping_background,
            minimal_host_introspect_bg,
            minimal_host_send_bg_message,
            minimal_host_introspect_dapp,
            minimal_host_list_webview_windows,
            minimal_host_read_report,
            minimal_host_write_acceptance_report,
            minimal_host_phantom_fixture_path,
            minimal_host_bg_fixture_path,
            minimal_host_metamask_fixture_path,
            minimal_host_rabby_fixture_path,
            minimal_host_test_dapp_url,
            minimal_host_open_test_dapp_window,
            minimal_host_record_probe,
            minimal_host_probe_dapp,
        ])
        .run(tauri::generate_context!())
        .expect("tauri application error");
}
