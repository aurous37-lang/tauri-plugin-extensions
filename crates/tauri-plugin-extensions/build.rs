//! Tauri v2 plugin build script.
//!
//! Registers every Tauri-command this plugin exposes so Tauri's capability
//! system knows the plugin exists and which commands are invocable. Without
//! this, consumers hit `Plugin not found` at IPC time even though the
//! commands compile and register on the Rust side.
//!
//! The list here MUST match the `invoke_handler![...]` list in `src/lib.rs`
//! one-for-one. A drift either direction (command here but not registered,
//! or registered but not here) means the permission system won't issue the
//! right allowlist and the frontend's invoke fails.

const COMMANDS: &[&str] = &[
    // Lifecycle surface
    "extensions_load_unpacked",
    "extensions_unload",
    "extensions_list",
    "extensions_list_lifecycle",
    "extensions_reload",
    "extensions_enable",
    "extensions_disable",
    "extensions_reconcile_orphans",
    "extensions_diagnostics",
    // Runtime (content-script / background message plumbing)
    "extensions_content_ready",
    "extensions_scripting_register_content_scripts",
    "extensions_scripting_unregister_content_scripts",
    "extensions_scripting_get_registered_content_scripts",
    "extensions_runtime_send_message",
    "extensions_runtime_connect",
    "extensions_runtime_port_post",
    "extensions_runtime_port_disconnect",
    // Storage
    "extensions_storage_get",
    "extensions_storage_set",
    "extensions_storage_remove",
    "extensions_storage_clear",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();

    // Test binaries that link the tao/wry closure import
    // comctl32.dll!TaskDialogIndirect, which only exists in Common-Controls
    // v6. Real app binaries get a comctl32-v6 manifest from tauri-build;
    // bare test harness binaries don't, so the loader binds comctl32 v5 and
    // the process dies at load with STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139)
    // before main. Embed the manifest into test binaries only — MSVC's
    // linker merges multiple /MANIFESTINPUT files, so this composes with
    // rustc's own default manifest.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("tests")
            .join("win-comctl32-v6.manifest");
        // `rustc-link-arg` (not `-tests`) so the lib's own unittest binary
        // is covered too — `rustc-link-arg-tests` only reaches integration
        // test targets. The lib itself is an rlib (no link step) and the
        // crate ships no bins/examples, so in practice this only ever
        // affects test binaries; link args never propagate to consumers.
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
}
