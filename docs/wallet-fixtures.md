# Wallet fixtures — fetching unpacked MetaMask / Rabby / Phantom on Windows

The EVM-wallet acceptance work (D-005 v2 + the EVM provider matrix) loads three
real wallets unpacked: **MetaMask**, **Rabby**, and **Phantom**. Their vendor
code is `.gitignore`d under `fixtures/test-extensions/<name>/` and is populated
by the CRX3 fetch scripts in `scripts/`.

## One-time fetch

From the repo root, in PowerShell:

```powershell
# Phantom (Solana + EVM multichain) — v1 acceptance wallet
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-phantom.ps1

# MetaMask (EVM) — v2
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-metamask.ps1

# Rabby (EVM) — v2
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-rabby.ps1
```

Each script:

1. Hits the Chrome Web Store update endpoint for the extension id
   (`response=redirect`) to get a signed CRX3 artifact.
2. Strips the CRX3 header (`Cr24` magic + protobuf header) to expose the inner
   ZIP, and `Expand-Archive`s it into `fixtures/test-extensions/<name>/`.
3. Verifies `manifest.json` exists and is MV3.

Chrome Web Store ids used:

| Wallet   | CWS id                               | Fetch script              |
|----------|--------------------------------------|---------------------------|
| Phantom  | `bfnaelmomeimhlpmgjnjophhpkkoljpa`    | `scripts/fetch-phantom.ps1`  |
| MetaMask | `nkbihfbeogaeaoehlefnkodbefgpgknn`    | `scripts/fetch-metamask.ps1` |
| Rabby    | `acmacodkjbdgmoleebolmdjonilkdbch`*   | `scripts/fetch-rabby.ps1`    |

\* See `scripts/fetch-rabby.ps1` for the exact id it pins.

The directories are gitignored — a fresh clone has only the committed
`noop-mv3` fixture until you run the scripts. `examples/minimal-host` resolves
each wallet by walking up from `CARGO_MANIFEST_DIR` to the repo root, so no env
var is needed; if a fixture is missing, the host's
`minimal_host_<wallet>_fixture_path` command returns a "run scripts/fetch-…"
error rather than a confusing "directory not found".

## How each wallet injects `window.ethereum` (and why it matters here)

These three wallets use three *different* mechanisms to put an EIP-1193 provider
on the page — each exercises a different part of this runtime:

- **MetaMask** declares `scripts/inpage.js` as a **static `world: "MAIN"`
  content script** in its manifest. The runtime's content-script pipeline
  injects it directly (same path as Phantom's `solana.js`). The provider object
  does not depend on the background worker; `ethereum.request()` round-trips do.
  Note MetaMask runs SES/LavaMoat `lockdown()` in the page realm, which hardens
  the page and breaks the host page's Tauri IPC — see
  `docs/evm-injection-findings.md`.

- **Rabby** ships its provider as a **`web_accessible_resource`
  (`pageProvider.js`)** and its content script injects it with
  `document.createElement("script"); src = chrome.runtime.getURL("pageProvider.js")`.
  This requires the runtime to *serve* extension resources — see the
  `extres://` URI scheme (`src/runtime/resources.rs`). The embedding app's CSP
  must allow scripts from that origin (the minimal host's `tauri.conf.json`
  adds `http://extres.localhost` to `script-src`).

- **Phantom** registers its EVM inpage bundle **at runtime from the background
  service worker** via `chrome.scripting.registerContentScripts([... world:
  "MAIN" ...])`. This requires `chrome.scripting.registerContentScripts` (see
  `src/runtime/dynamic_scripts.rs`) *and* a working module service worker
  (Phantom's `background.type: "module"`). The latter is the current blocker —
  see `docs/evm-injection-findings.md`.

## Re-running the acceptance / EVM matrix

See `CLAUDE.md` → "Development commands" for the headless run. The minimal host
auto-loads `noop-mv3` + Phantom and then runs an **EVM provider matrix** that,
for each wallet present, loads it in isolation, introspects its background
worker (errors + `chrome.*` api-gaps), opens the canary
(`fixtures/test-dapp/index.html`), and records the provider probe. Results land
in `examples/minimal-host/acceptance-report.json` under `evm.<wallet>`.
