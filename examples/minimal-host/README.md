# mv3 minimal-host

Smallest Tauri v2 application that depends on `tauri-plugin-extensions`
and exercises the `noop-mv3` fixture. This is the spike-phase manual
integration harness — see `../../tests/integration/README.md` for where
this fits in the overall test picture.

## Prerequisites

The same Windows toolchain the plugin crate itself needs:

- Rust `stable-x86_64-pc-windows-msvc` (pinned in `rust-toolchain.toml`).
- Node LTS + pnpm (for the `@tauri-apps/cli`).
- WebView2 runtime (evergreen, ships with Windows 11 by default).
- VS 2022 Build Tools with the MSVC toolchain + Win11 SDK.

## Install

From this directory:

```powershell
pnpm install
```

That only installs `@tauri-apps/cli`; there is no Vite / SvelteKit step
because the frontend is a pre-written static `dist/` directory
(plain HTML + CSS + ES module). No build step runs before `pnpm tauri
dev` / `pnpm tauri build`.

## Run (dev)

```powershell
pnpm tauri dev
```

First launch cold-builds the plugin crate + the host crate; on a cold
cache expect several minutes. Subsequent runs reuse the `target/`
cache under `examples/minimal-host/src-tauri/target/`.

## Build (release)

```powershell
pnpm tauri build --debug    # skip LTO / symbol stripping for faster builds
pnpm tauri build            # full release, produces an MSI under
                            # src-tauri/target/release/bundle/msi/
```

The `tauri.conf.json` sets `bundle.targets` to `["msi"]`, matching
the Tauri defaults for a Windows host. Add `"nsis"` to that list if
you need an NSIS installer in addition.

## What to click

1. **ping IPC** — sanity-checks that the host's Tauri command handler
   is wired. Expect `pong`.
2. **load noop-mv3** — calls `minimal_host_load_unpacked` with the
   absolute path to `fixtures/test-extensions/noop-mv3/` (resolved
   by the host at startup via `CARGO_MANIFEST_DIR`). Expect a log
   line of the form `loaded ok -> id=local-<uuid>`.
3. **list extensions** — calls `minimal_host_list_extensions`,
   which is a direct pass-through over
   `ExtensionRegistry::list()`. Expect one entry named
   `Noop MV3 (spike fixture)`.
4. **ping background** — calls a host-side shim that eventually will
   wrap the plugin's `extensions_runtime_send_message`. Until Agent C
   registers that plugin command, the button surfaces the
   "command not yet registered" error verbatim.

## Layout

```
examples/minimal-host/
├── README.md               (this file)
├── package.json            (@tauri-apps/cli only)
├── dist/                   frontend, hand-written
│   ├── index.html
│   ├── styles.css
│   └── main.js
└── src-tauri/
    ├── Cargo.toml          isolated workspace; `path = ../../../crates/...`
    ├── build.rs            tauri-build invocation
    ├── tauri.conf.json
    ├── capabilities/
    │   └── default.json    core invoke only
    ├── icons/
    │   ├── icon.png
    │   └── icon.ico
    └── src/
        ├── main.rs
        └── lib.rs          host commands + plugin.init()
```

## Why the host exposes its own commands

`tauri-plugin-extensions::init()` does not yet register any plugin-side
Tauri commands — that layer is Agent C's in-flight work. Until those
land, the minimal host exposes a small set of `minimal_host_*` commands
that wrap the plugin's public **Rust** API (`load_unpacked`,
`ExtensionRegistry::list`). Once Agent C ships
`plugin:extensions|extensions_load_unpacked` and friends, the frontend
can switch to invoking those plugin commands directly; the host
wrappers become optional sugar.

## Isolated workspace

`src-tauri/Cargo.toml` declares `[workspace]` (empty table), which
turns the host crate into its own Cargo workspace root. The repo-level
`Cargo.toml` also `exclude`s this directory explicitly so
`cargo check --workspace` from the repo root stays fast.
