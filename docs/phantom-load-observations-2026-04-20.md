# Phantom-load observations — 2026-04-20 (Agent I, scaffold-only pass)

This is the first Agent-I pass. Scope here is **instrumentation**, not a
live-load run. The Phantom phase has been wired into the minimal-host
auto-acceptance flow and compiles clean, but I did not execute
`pnpm tauri dev` against a fresh Phantom fixture — that's the next session's
first task, and it requires all three parallel agents (G, H, I) to have
landed. Observations below are what the scaffold is now capable of
reporting the first time it runs end-to-end.

## What landed

- `fixtures/test-dapp/index.html` — a canary dapp that polls for
  `window.phantom.*`, `window.ethereum`, `window.solana` for 5 s and posts
  the result back via `minimal_host_record_probe`. Exposes
  `window.__dappProbeResult` for ad-hoc debugging. Dark-themed to match the
  minimal-host aesthetic.
- `fixtures/test-dapp/README.md` — explains the dapp's purpose and why it
  is not vendored.
- `examples/minimal-host/src-tauri/src/lib.rs` — six new Tauri commands:
  - `minimal_host_phantom_fixture_path` — resolves `fixtures/test-extensions/phantom/`
    and errors out cleanly when the gitignored vendor drop hasn't been
    fetched.
  - `minimal_host_test_dapp_url` — builds a `file:///` URL for the canary
    HTML; percent-encodes spaces.
  - `minimal_host_open_test_dapp_window` — opens a second
    `WebviewWindow` labelled `"test-dapp"` pointed at that URL. Closes an
    existing window of the same label first so reload works.
  - `minimal_host_record_probe` — invoked by the dapp page itself to hand
    its observation back to the host.
  - `minimal_host_probe_dapp` — the main window reads the probe back out.
  - `resolve_repo_root` — internal helper, factored out of the noop resolver.
- `examples/minimal-host/src-tauri/capabilities/default.json` — added
  `"test-dapp"` to the `windows` list so the new webview gets the core
  invoke permissions it needs for `minimal_host_record_probe`.
- `examples/minimal-host/dist/index.html` — a new "Phantom phase" panel
  between the log pane and the existing controls, with slots for the
  fixture path, loaded id, dapp URL, probe JSON, and lifecycle events.
- `examples/minimal-host/dist/main.js` — added `runPhantomPhase(report)`
  called after the existing noop phase. Subscribes to
  `extensions://lifecycle/changed` before load so both `Installed` and
  `Started` events are captured. Calls `extensions_diagnostics`
  opportunistically (errors are recorded, not fatal).
- `examples/minimal-host/dist/styles.css` — a minor `.sublabel` rule for
  the new section's inline h3s.

## Phantom manifest — static observations

Before running anything, the manifest file already tells us what to expect.
From `fixtures/test-extensions/phantom/manifest.json`:

- **MV3** — `manifest_version: 3`, so the MV2 rejection path in
  `manifest::parser` doesn't fire. Good.
- **Background** is `background/serviceWorker.js` with `"type": "module"`.
  The plugin's `spawn_background` composes a bootstrap that evaluates the
  source string; module imports from the service worker file may or may
  not resolve through WebView2's `initialization_script` path. This is a
  likely first failure surface — the hidden BG window will load but the
  ES-module `import` statements inside Phantom's worker bundle could 404
  because there's no HTTP origin for relative imports. Worth watching.
- **Content scripts** declare two entries at `document_start`,
  `all_frames: true`:
  - MAIN world: `solana.js` + `phantom.js`.
  - ISOLATED world: `contentScript.js`.
  Both target `file://*/*`, `http://*/*`, `https://*/*` — so our
  `file:///.../test-dapp/index.html` canary URL IS in scope. Phantom
  exercises the MAIN/ISOLATED world distinction immediately; Agent H's
  `runtime::injection::register_hooks` must honor `world`. If it only
  injects into a single world, `window.phantom.solana` will be absent
  (MAIN-world-only) and the probe will come back with all-false.
- **`web_accessible_resources`** — Phantom lists `solana.js`, `evmAsk.js`,
  `evmPhantom.js`, `evmMetamask.js`, `btc.js`, `sui.js`, `phantom.js`
  against `<all_urls>`. The MAIN-world script Phantom ships almost
  certainly does a `document.createElement('script')` + `src=chrome-extension://.../evmPhantom.js`
  dance; if the plugin doesn't resolve the `chrome-extension:` URL scheme
  (or a Tauri-localhost equivalent), the EVM surface won't inject even if
  the initial Solana scripts do. Second-most-likely failure surface.
- **Permissions** include `webRequest`, which is explicitly out of scope
  per `ARCHITECTURE.md` ("blocks on intercepting network traffic at the
  wry / CEF layer"). The manifest parser should tolerate this gracefully
  (unknown permission names are typically ignored in MV3). If
  `manifest::parser` hard-rejects unknown permissions, that's an
  immediate fix.
- **Key present** (`"key": "MIIBIj..."`) — so `ExtensionId::from_key`
  fires and the stable id matches Chromium's. This is why the load is
  idempotent.

## Phantom manifest — expected outcome on first run

If Agents G and H have landed cleanly:

- `phantom_fixture_path` — ok (the extension is unpacked at the expected
  location).
- `phantom_load` — expected to succeed. The manifest is valid MV3; the
  plugin's manifest parser has handled `content_scripts.world`,
  `host_permissions`, `web_accessible_resources` since the noop fixture.
- `list_windows_after_phantom_load` — expected to show `main`, and an
  `ext-bg-<first 12 chars of phantom id>` label. The Phantom extension id
  derived from the manifest key is `bfnaelmomeimhlpmgjnjophhpkkoljpa` (per
  Chrome Web Store), so the expected BG label is
  `ext-bg-bfnaelmomei`.
- `phantom_ping_background` — reports `bgWindowPresent: true` when
  `spawn_background` worked; `false` otherwise.
- `test_dapp_url` — ok; a `file:///C:/Users/Me/Desktop/MV3/fixtures/test-dapp/index.html`
  string.
- `open_test_dapp_window` — ok; a second window labelled `test-dapp`.
- **`read_probe` — this is the load-bearing observation.** When Agent H's
  `on_page_load` injection hook fires for the `test-dapp` window, Phantom's
  MAIN-world scripts SHOULD run and the probe SHOULD come back with at
  least `phantomPresent: true` and `phantomSolana: true`. If instead the
  probe is all-false, Phantom's content scripts did not inject — which
  means Agent H's injection path is either not firing for external
  (non-tauri://) URLs, not honoring `world: MAIN`, or not running at
  `document_start` as requested.
- `extensions_diagnostics` — surfaces Agent G's diagnostic blob; captured
  in the acceptance-report.json regardless of whether the command exists.
- `lifecycleEvents` — expected to contain at minimum
  `{kind: "installed", ...}` and `{kind: "started", ..., bg_present: ?}`.
  If an orphan was reaped at boot (unlikely for a fresh run) there will
  also be an `orphan_reaped`.

## What the frontend probe records

The canary dapp writes a JSON like:

```json
{
  "phantomPresent": false,
  "phantomSolana": false,
  "phantomEthereum": false,
  "ethereum": false,
  "solana": false,
  "isPhantom": false,
  "ethereumIsPhantom": false,
  "solanaIsPhantom": false,
  "phantomSolanaIsPhantom": false,
  "phantomKeys": []
}
```

When the full chain works, this becomes at minimum
`phantomPresent: true, phantomSolana: true, phantomSolanaIsPhantom: true`.

## What is likely to break on the first live run

In decreasing order of suspicion:

1. **Content scripts don't inject into `file://` URLs.** Agent H's
   injection hook may only wire up for `tauri://localhost` and `http(s)://`
   origins. The probe dapp is served off `file:///`, which matches
   Phantom's manifest but may not match the plugin's hook whitelist.
   Remediation: either serve the dapp off a `tauri://` custom URI protocol
   (cleanest — we'd add a `tauri::Builder::register_uri_scheme_protocol`),
   or loosen the injection hook's URL filter.
2. **World separation not honored.** If the plugin drops `world: "MAIN"`
   and injects everything into the isolated world, `window.phantom.*`
   shows in the isolated context but never lands on the page's `window`,
   so the probe (which runs in the main world) sees nothing.
3. **Background `type: "module"` bootstrap fails.** Phantom's
   `background/serviceWorker.js` is an ES module that imports from sibling
   files. The plugin's `spawn_background` wraps the source string into an
   initialization script; module resolution inside `initialization_script`
   has no origin to resolve relative imports against. The BG webview
   spawns but the worker never fully initializes. Remediation: load the
   BG webview at a real data or custom-scheme URL that can resolve
   `import "./foo.js"` properly.
4. **web_accessible_resources not served.** Phantom does `chrome-extension://`
   fetches for its EVM sub-bundles. Plugin must either serve these off a
   mapped custom scheme or rewrite them inline. Deferred problem — the
   Solana-only surface doesn't need them for initial probe.
5. **Manifest parser rejects unknown permissions.** `sidePanel`, `identity`,
   `webRequest` are all in the Phantom manifest. If the parser is strict,
   load fails before BG even spawns.

## Specific follow-ups for the next session

1. Run `pnpm tauri dev` inside `examples/minimal-host/` on a machine with
   Phantom already fetched (`scripts/fetch-phantom.ps1` has been run).
2. Read the generated `examples/minimal-host/acceptance-report.json` and
   attach it to the session log. The Phantom phase object at
   `report.phantom` summarizes the three load-bearing observations: probe
   result, lifecycle events, loaded id.
3. If the probe is all-false, try the three remediations in order:
   a. Inject a `tauri://` scheme for the test dapp instead of `file://`.
   b. Check Agent H's hook URL filter — grep for any origin gate.
   c. Check world-separation wiring in `runtime/injection.rs`.
4. Regardless of Phantom outcome, examine the
   `extensions://lifecycle/changed` event stream. If we see `Installed`
   but never `Started`, the load_unpacked path is finishing before
   lifecycle's `start` transition — that's a lifecycle-manager bug to
   flag to Agent G.
5. If `extensions_diagnostics` returns "command not found," Agent G's
   output hasn't landed — un-block by either merging G's branch or
   inlining a placeholder diagnostics command into the plugin.

## Process notes

- `cargo check --workspace` and `cargo check` (minimal-host) both clean
  after the changes. Only pre-existing warning is in
  `runtime/injection.rs:191` (Agent H's domain, not touched).
- Scaffolding did not require touching plugin src, `js-runtime/`, or any
  `fixtures/test-extensions/*` directory — stayed within the Agent-I scope.
- Acceptance-report.json from the last noop-only run is preserved until
  the next run overwrites it; the format is backward-compatible (phantom
  phase adds a top-level `phantom` key and a `startedAt`/steps list that
  includes the new `phantom_*` step names).
