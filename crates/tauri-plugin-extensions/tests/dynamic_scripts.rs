//! Integration test for `chrome.scripting.registerContentScripts` →
//! [`DynamicScriptStore`]. Wallets (Phantom's EVM provider, Rabby) register
//! their inpage/provider scripts at runtime from the background service
//! worker rather than declaring them statically in the manifest. The store is
//! the runtime home for those dynamic registrations; the on_page_load
//! injection flow merges its [`InjectionRequest`]s with the manifest-declared
//! ones.
//!
//! This exercises the store directly (no Tauri webview) — same rationale as
//! `injection_rule_resolution.rs`: the store reads script sources from disk by
//! the `source_dir` captured at registration time, so it needs only a temp
//! dir, not a live extension registry.

use std::fs;

use tauri_plugin_extensions::{
    registry::ExtensionId,
    runtime::{
        dynamic_scripts::{DynamicScriptStore, RegisteredScript},
        RunAt, World,
    },
};

fn script(id: &str, dir: &std::path::Path, matches: &[&str], js: &[&str], world: World) -> RegisteredScript {
    RegisteredScript {
        id: id.to_string(),
        source_dir: dir.to_path_buf(),
        matches: tauri_plugin_extensions::matcher::MatchPatternSet::parse_many(matches)
            .expect("parse matches"),
        match_strings: matches.iter().map(|s| s.to_string()).collect(),
        js_files: js.iter().map(std::path::PathBuf::from).collect(),
        run_at: RunAt::DocumentStart,
        world,
        all_frames: true,
        persist_across_sessions: true,
    }
}

#[test]
fn registered_main_world_script_injects_into_matching_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("evmAsk.js"), "window.__evmProvider = true;").unwrap();

    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-phantom");
    store
        .register(
            &ext,
            vec![script(
                "conditionalInpageScripts",
                dir.path(),
                &["http://*/*", "https://*/*"],
                &["evmAsk.js"],
                World::Main,
            )],
        )
        .expect("register");

    // Matching URL yields exactly one request, MAIN world, with the file's
    // source read from disk.
    let url = url::Url::parse("https://app.example.com/swap").unwrap();
    let reqs = store.requests_for_url(&url);
    assert_eq!(reqs.len(), 1, "one dynamic script should match");
    assert_eq!(reqs[0].extension, ext);
    assert_eq!(reqs[0].world, World::Main);
    assert_eq!(reqs[0].run_at, RunAt::DocumentStart);
    assert!(reqs[0].source.contains("window.__evmProvider"));

    // Non-matching scheme yields nothing.
    let none = store.requests_for_url(&url::Url::parse("ftp://x/").unwrap());
    assert!(none.is_empty(), "ftp should not match http/https patterns");
}

#[test]
fn duplicate_registration_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "1;").unwrap();
    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-1");
    store
        .register(&ext, vec![script("dup", dir.path(), &["https://*/*"], &["a.js"], World::Main)])
        .expect("first register ok");
    let err = store.register(
        &ext,
        vec![script("dup", dir.path(), &["https://*/*"], &["a.js"], World::Main)],
    );
    assert!(err.is_err(), "re-registering an existing id must error (Chrome contract)");
}

#[test]
fn unregister_by_id_then_all() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "1;").unwrap();
    fs::write(dir.path().join("b.js"), "2;").unwrap();
    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-1");
    store
        .register(
            &ext,
            vec![
                script("a", dir.path(), &["https://*/*"], &["a.js"], World::Main),
                script("b", dir.path(), &["https://*/*"], &["b.js"], World::Isolated),
            ],
        )
        .unwrap();
    let url = url::Url::parse("https://x.example/").unwrap();
    assert_eq!(store.requests_for_url(&url).len(), 2);

    store.unregister(&ext, Some(&["a".to_string()]));
    assert_eq!(store.get_registered(&ext, None).len(), 1);
    assert_eq!(store.requests_for_url(&url).len(), 1);

    store.unregister(&ext, None);
    assert!(store.get_registered(&ext, None).is_empty());
    assert!(store.requests_for_url(&url).is_empty());
}

#[test]
fn get_registered_round_trips_chrome_metadata() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "1;").unwrap();
    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-1");
    store
        .register(&ext, vec![script("only", dir.path(), &["https://a/*", "https://b/*"], &["a.js"], World::Main)])
        .unwrap();
    let got = store.get_registered(&ext, Some(&["only".to_string()]));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "only");
    assert_eq!(got[0].match_strings, vec!["https://a/*".to_string(), "https://b/*".to_string()]);
    assert_eq!(got[0].js_files, vec![std::path::PathBuf::from("a.js")]);
}

#[test]
fn reregister_same_id_after_drop_succeeds_like_a_reload() {
    // Models the reload path the lifecycle manager now takes: stopping the BG
    // drops the extension's dynamic scripts (internal_stop -> drop_extension),
    // so the respawned worker re-registers the SAME id (Phantom's
    // "conditionalInpageScripts") cleanly instead of hitting the duplicate-id
    // error that would otherwise strand the freshly-built EVM provider.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("evm.js"), "1;").unwrap();
    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-1");
    let mk = || {
        vec![script(
            "conditionalInpageScripts",
            dir.path(),
            &["https://*/*"],
            &["evm.js"],
            World::Main,
        )]
    };
    store.register(&ext, mk()).expect("first register");
    // Re-registering the same id without a drop errors (Chrome contract).
    assert!(store.register(&ext, mk()).is_err());
    // The reload path drops first, so the respawn re-registers cleanly.
    store.drop_extension(&ext);
    store.register(&ext, mk()).expect("re-register after drop");
    assert_eq!(
        store.requests_for_url(&url::Url::parse("https://x.example/").unwrap()).len(),
        1,
    );
}

#[test]
fn drop_extension_removes_all_dynamic_scripts() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.js"), "1;").unwrap();
    let store = DynamicScriptStore::new();
    let ext = ExtensionId::new("ext-1");
    store
        .register(&ext, vec![script("a", dir.path(), &["https://*/*"], &["a.js"], World::Main)])
        .unwrap();
    store.drop_extension(&ext);
    assert!(store.requests_for_url(&url::Url::parse("https://x/").unwrap()).is_empty());
}
