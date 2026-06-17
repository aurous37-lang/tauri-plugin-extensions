# Test dapp — Phantom canary page

This is the smallest possible "dapp" used to prove that `tauri-plugin-extensions`
injects a wallet extension's content scripts into ordinary web pages.

## What it does

On load it polls for up to 5 seconds (every 100 ms) for the globals a Phantom
install is expected to hang on `window`:

- `window.phantom` (object)
- `window.phantom.solana` (primary Phantom surface, per Phantom docs)
- `window.phantom.ethereum` (Phantom's EVM surface)
- `window.ethereum` (standard EIP-1193 provider)
- `window.solana` (Solana wallet-standard shim)
- `*.isPhantom` discriminator

It exposes the probe result on `window.__dappProbeResult` (for ad-hoc dev
inspection) and, when loaded inside a Tauri webview, invokes
`minimal_host_record_probe` so the host's auto-acceptance run can read the
result back.

## Why it exists

Phantom is the D-005 v1 acceptance target. The single most load-bearing
observation early in the spike is: do Phantom's `content_scripts` inject at
all when the extension is loaded via `tauri_plugin_extensions::load_unpacked`?
If they do, the rest of the shim surface can be filled in incrementally. If
they don't, the project is blocked until Agent H's
`runtime::injection::register_hooks` path is producing real injection.

## Not a real wallet UI

There is no connect button, no signing, no RPC. The page's only job is to
print what it sees on `window.*` and hand the result back to the host.
Treat it as a test assertion rendered as HTML.

## Not vendored

Lives under `fixtures/test-dapp/` (NOT `fixtures/test-extensions/`) because it
is our code, not a vendor upload. It is checked in; the Phantom extension
itself is not (see `.gitignore`).
