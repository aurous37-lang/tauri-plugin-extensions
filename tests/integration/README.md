# tests/integration

Spike-phase integration coverage for `tauri-plugin-extensions`.

## Where the tests live

- **`examples/minimal-host/`** — the one-window Tauri app that loads the
  `noop-mv3` fixture, calls every public command on the plugin's host
  wrappers, and surfaces the results in the UI. This is the **primary
  integration harness** for the spike. Manual acceptance is:

  1. `cd examples/minimal-host && pnpm install`
  2. `pnpm tauri dev`
  3. Click **ping IPC** — log shows `pong`.
  4. Click **load noop-mv3** — log shows `loaded ok -> id=...`.
  5. Click **list extensions** — JSON dump shows one entry whose
     `name` is `Noop MV3 (spike fixture)`.
  6. Click **ping background** — once Agent C wires the plugin-side
     IPC bus command (`extensions_runtime_send_message`), this should
     produce a pong round-trip; until then, the button surfaces the
     "command not yet registered" error verbatim. That's the expected
     dev-time state.

- **`crates/tauri-plugin-extensions/tests/load_noop_fixture.rs`** —
  Rust-level test that parses the noop manifest and drives the
  registry through its public API (insert → list → get → remove).
  Run with:

  ```
  cargo test -p tauri-plugin-extensions --test load_noop_fixture
  ```

- **`crates/tauri-plugin-extensions/tests/manifest_real_extensions.rs`**
  — exercises Agent A's parser against real Phantom / MetaMask / Rabby
  manifests committed under `tests/fixtures/manifests/`.

## Punt items

- **`tauri::test::mock_builder`-based acceptance.** The current
  `load_noop_fixture` test doesn't spin up a mock Tauri app because
  `tauri::test` requires the `tauri/test` feature to be enabled in the
  plugin crate's dev-dependencies, which would drag the full Tauri
  build graph into every `cargo test` invocation on the plugin. Once
  Agent D's hidden-webview background runner lands and the test needs
  to exercise the webview lifecycle, this test should be upgraded to
  `mock_builder` and the dev-dep adjusted.

- **Automated UI drive of the minimal-host.** No Playwright / wry-level
  automation is set up. The button flow above is manual. A future
  harness could drive it through the webview's IPC channel directly;
  out of scope for the spike.

## How this directory grows

Workspace-level integration tests that span multiple crates (e.g. a
run-time smoke test that exercises parser → matcher → registry → IPC)
should land here. Per-crate integration tests belong under the owning
crate's `tests/` directory.
