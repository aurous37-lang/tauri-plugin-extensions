# Spike notes — 2026-04-20

Captures the state at the end of the initial six-agent scaffold + integration
pass. Reference for picking this up cold.

## What's shipped

Six parallel workstreams landed and integrated:

| Workstream | Files | Status |
|---|---|---|
| A. Manifest parser | `src/manifest/` + `tests/fixtures/manifests/` | Complete |
| B. URL / glob matchers | `src/matcher/` | Complete — 34 tests |
| C. Plugin / loader / IPC / storage | `src/{lib,loader,registry}.rs`, `src/{ipc,storage}/` | Complete — 10 tests |
| D. WebView2 backend + hidden BG webview | `src/runtime/` | Compile-only; runtime TBD |
| E. TypeScript `chrome.*` shim | `js-runtime/`, `embedded-js/*.js` | Complete — 43KB IIFE bundles |
| F. Minimal host + Phantom fetch | `examples/minimal-host/`, `scripts/fetch-*.ps1` | Complete |

Verification on this host (2026-04-20, Win 11 26200, Rust 1.95 MSVC):

- `cargo check --workspace` → clean (0.23s warm).
- `cargo clippy -p tauri-plugin-extensions --all-targets -- -D warnings` → clean.
- `cargo test -p tauri-plugin-extensions --test c_bus_storage --test manifest_real_extensions --test load_noop_fixture` → **15/15 pass**.
- `examples/minimal-host/src-tauri && cargo check` → clean.
- `js-runtime/ && pnpm build` → `content-bootstrap.js` (21.5 KB) + `background-bootstrap.js` (21.8 KB).

Real MV3 manifests checked in for all three acceptance targets: Phantom
(v26.13.0 from the live Chrome Web Store CRX3), MetaMask
(`_base.json` from `main`), Rabby (`chrome-mv3/manifest.json` from `develop`).

## Pending the next session

### Spike-acceptance end-to-end run

The final spike-exit gate per `docs/spike-plan.md` is:

```powershell
cd C:\Users\Me\Desktop\MV3\examples\minimal-host
pnpm install
pnpm tauri dev
```

In the minimal-host window:

1. Click **ping IPC** → expect "pong" (proves plugin init + commands reach).
2. Click **load noop-mv3** → expect `loaded ok → id=local-<uuid>` (proves
   loader + manifest parser + matcher + registry).
3. Click **list extensions** → expect JSON with the noop fixture.
4. Click **ping background** → currently expected to surface the
   "not yet wired" error; full content→BG round-trip is spike-incomplete.

The hidden BG webview should become visible in the Tauri window-list (and
it is — pass `--inspect` or attach devtools to verify the bootstrap evaled
the service-worker source).

### Open items

1. **cargo test --lib fails at load time** with `STATUS_ENTRYPOINT_NOT_FOUND`
   (`0xc0000139`). Any lib-test binary transitively pulls in
   `WebView2Loader.dll` via `wry` / `tao` / `webview2-com`, and the DLL
   isn't on PATH for the test harness. Integration-test binaries that
   don't touch `AppHandle<Wry>` are fine — that's why the 15 tests above
   pass. Fix ideas (pick one when next you need lib-test coverage):
   - Set a `.cargo/config.toml` `[env]` entry pointing at
     `%ProgramFiles(x86)%\Microsoft\EdgeWebView\Application\<ver>\` and
     have it come first on PATH.
   - Copy `WebView2Loader.dll` into `target/debug/deps/` before tests
     (build-script hack).
   - Move every inline `#[cfg(test)]` test that touches the Tauri link
     closure into `tests/*.rs` integration files, the way Agent C did.
     Simplest; covers current needs without fighting the toolchain.

2. **`Webview2Backend::inject` is stubbed.** Real content-script injection
   needs `WebviewWindow::on_page_load` registered on every consumer-
   created window, plus a registry lookup resolving
   `content_scripts_for_url(url)` into `evaluate_script` calls at the
   correct lifecycle phase. Half-day of work. Drop-in by Tauri's
   `on_page_load` callback pattern.

3. **content→BG message routing.** `extensions_runtime_port_post` is a
   log-and-return stub. The Bus can route between registered ports
   (`c_bus_storage::bus_send_with_reply_round_trips` proves it), but the
   plumbing from JS `port.postMessage` → Bus isn't yet wired. v1 work.

4. **Agent A's `Manifest` struct doesn't yet type `content_scripts[]` or
   `background.service_worker`.** The loader re-parses raw JSON for those
   paths as a shim. When Agent A's schema grows those fields the
   re-parse collapses into a typed accessor. Non-blocking.

5. **MetaMask + Rabby fetch scripts exist but weren't live-run** this
   session (Phantom only). Same CRX3 header-strip core as
   `fetch-phantom.ps1`; should work against the respective extension ids
   when invoked.

## Architectural reminders for the next session

The four locks from `DECISIONS.md` stay in force — don't revisit without
new evidence:

- **D-001** — background runner is a hidden `WebviewWindow`. boa /
  deno_core rejected.
- **D-002** — Tauri plugin, not a wry fork.
- **D-003** — Windows-only v1. macOS / Linux stubs return
  `PlatformUnsupported`.
- **D-005** — Phantom is v1 acceptance; MetaMask + Rabby come together
  as v2 (overrides `engineering-summary.md`).

## Resume prompt — for Claude or a human

1. Read `docs/DECISIONS.md`, `docs/ARCHITECTURE.md`, this file.
2. Run the minimal-host end-to-end (see above) to establish the current
   runtime baseline. Note anything that deviates from the expected
   button behavior.
3. Pick the next item off the Open items list. Likely order:
   inject-path → content→BG routing → DLL-on-PATH so lib tests run →
   live-run MM + Rabby fetch scripts.
4. Only after the above four are in place: load Phantom and iterate
   until the acceptance flow works.
