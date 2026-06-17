//! Integration test: the `noop-mv3` fixture parses cleanly via the plugin's
//! public manifest API.
//!
//! Owned by Agent F.
//!
//! ## What this test covers
//!
//! - The fixture directory at `fixtures/test-extensions/noop-mv3/` exists and
//!   contains a valid `manifest.json`.
//! - That manifest round-trips through [`tauri_plugin_extensions::manifest`]
//!   without error and produces the fields the rest of the plugin depends on
//!   (name, version, content-script rule, storage permission).
//!
//! ## What this test deliberately does NOT do
//!
//! It does not exercise [`tauri_plugin_extensions::registry::ExtensionRegistry`]
//! here. The registry module transitively pulls in `runtime::background` /
//! `runtime::webview2`, and on Windows linking the wry → WebView2 DLL graph
//! into an integration-test binary produces a `STATUS_ENTRYPOINT_NOT_FOUND`
//! at startup (the same failure mode that breaks the plugin's own
//! `#[cfg(test)] mod tests` unit tests today). Registry-level coverage lives
//! inside the plugin's own unit-test module where it can be exercised
//! without crossing the integration-binary linkage boundary once Agent D's
//! backend lands and the unit tests can run again.
//!
//! Until then, the primary integration harness for the registry is the
//! `examples/minimal-host/` Tauri app (which links Tauri at build time as
//! normal and runs the registry inside a real `AppHandle`). See
//! `tests/integration/README.md` for the wider acceptance flow.

use tauri_plugin_extensions::manifest::{self, BackgroundType, RunAt, World};

/// Repo-root-relative path to the noop fixture.
fn noop_fixture_manifest() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-extensions/noop-mv3/manifest.json")
}

#[test]
fn noop_fixture_is_present() {
    let path = noop_fixture_manifest();
    assert!(
        path.is_file(),
        "noop-mv3 fixture manifest missing at {} — did the repo layout change?",
        path.display()
    );
}

#[test]
fn noop_fixture_manifest_parses_with_expected_shape() {
    let path = noop_fixture_manifest();
    let bytes = std::fs::read(&path).expect("read manifest");
    let m = manifest::parse(&bytes).expect("noop-mv3 manifest parses");

    assert_eq!(m.manifest_version, 3);
    assert_eq!(m.name, "Noop MV3 (spike fixture)");
    // Agent A models `version` as `Option<String>` (MetaMask's in-tree
    // template omits it at authoring time). The noop fixture hard-codes it.
    assert_eq!(m.version.as_deref(), Some("0.1.0"));
    assert!(m
        .description
        .as_ref()
        .expect("noop fixture declares a description")
        .contains("Minimal MV3"));

    // Background — MV3 shape, module type, single service-worker entry.
    let bg = m.background.as_ref().expect("background block present");
    assert_eq!(bg.service_worker.as_deref(), Some("background.js"));
    assert_eq!(bg.r#type, BackgroundType::Module);

    // Exactly one content script, matching <all_urls> at document_end in the
    // isolated world.
    assert_eq!(m.content_scripts.len(), 1);
    let cs = &m.content_scripts[0];
    assert_eq!(cs.matches, vec!["<all_urls>".to_string()]);
    assert_eq!(cs.js, vec!["content.js".to_string()]);
    assert_eq!(cs.run_at, RunAt::DocumentEnd);
    assert_eq!(cs.world, World::Isolated);
    assert!(!cs.all_frames);

    // Storage permission is declared — the whole point of the noop fixture
    // is exercising chrome.storage.local during the spike.
    assert!(m.permissions.contains(&"storage".to_string()));
    assert!(m.host_permissions.is_empty());
}
