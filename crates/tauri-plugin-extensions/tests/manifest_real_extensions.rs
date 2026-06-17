//! Integration tests: parse real MV3 manifests from Phantom, MetaMask, and
//! Rabby. Fixtures live alongside this file under
//! `tests/fixtures/manifests/<extension>/manifest.json`.
//!
//! Source provenance (captured 2026-04-20):
//!
//! - **Phantom:** extracted from the Chrome Web Store CRX3 bundle for extension
//!   id `bfnaelmomeimhlpmgjnjophhpkkoljpa`. Real packaged manifest (v26.13.0).
//! - **MetaMask:** `MetaMask/metamask-extension` repo, `main` branch,
//!   `app/manifest/v3/_base.json`. Platform overrides not merged — the base
//!   alone covers every field we model.
//! - **Rabby:** `RabbyHub/Rabby`, `develop` branch,
//!   `src/manifest/chrome-mv3/manifest.json` (v0.93.85).
//!
//! If Chrome ships a new manifest field one of these extensions starts using,
//! the test that fails first will be in this file — that's the early-warning
//! signal for Agent A to extend the schema.

use std::path::PathBuf;

use tauri_plugin_extensions::manifest::{self, BackgroundType, RunAt, World};

fn fixture(extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifests")
        .join(extension)
        .join("manifest.json")
}

#[test]
fn phantom_manifest_parses() {
    let path = fixture("phantom");
    let m = manifest::parse_from_path(&path).expect("Phantom manifest must parse");

    assert_eq!(m.manifest_version, 3);
    assert_eq!(m.name, "Phantom");
    assert_eq!(m.version.as_deref(), Some("26.13.0"));

    // Phantom declares MAIN + ISOLATED content scripts with uppercase
    // `world` — the load-bearing case for our case-insensitive deserializer.
    assert!(m.content_scripts.len() >= 2);
    let worlds: Vec<World> = m.content_scripts.iter().map(|c| c.world).collect();
    assert!(worlds.contains(&World::Main));
    assert!(worlds.contains(&World::Isolated));

    // Every content script should have matches + js present.
    for cs in &m.content_scripts {
        assert!(!cs.matches.is_empty());
        assert!(!cs.js.is_empty());
        assert_eq!(cs.run_at, RunAt::DocumentStart);
    }

    // Service worker module background.
    let bg = m.background.as_ref().expect("Phantom has a background");
    assert_eq!(
        bg.service_worker.as_deref(),
        Some("background/serviceWorker.js")
    );
    assert_eq!(bg.r#type, BackgroundType::Module);

    // Packaged extension carries a `key` (used to derive the stable ext id).
    assert!(m.key.is_some());

    // MV3 WAR shape — object-array, not bare strings.
    assert!(!m.web_accessible_resources.is_empty());
    assert!(m
        .web_accessible_resources
        .iter()
        .all(|w| !w.resources.is_empty() && !w.matches.is_empty()));

    // CSP present in object form.
    assert!(m
        .content_security_policy
        .as_ref()
        .and_then(|c| c.extension_pages.as_deref())
        .is_some());

    // `commands` and `side_panel` and `update_url` aren't typed fields in
    // v1 — they should round-trip through `extra` and not crash the parse.
    assert!(m.extra.contains_key("commands") || m.extra.contains_key("side_panel"));
}

#[test]
fn metamask_manifest_parses() {
    let path = fixture("metamask");
    let m = manifest::parse_from_path(&path).expect("MetaMask manifest must parse");

    assert_eq!(m.manifest_version, 3);
    // MetaMask uses locale-placeholder names; string should be intact.
    assert!(m.name.starts_with("__MSG_"));
    // The `_base.json` template intentionally omits `version` so build-time
    // platform overrides can supply it. Our parser models version as
    // optional for exactly this case — see schema.rs doc on `Manifest::version`.
    assert!(m.version.is_none(), "template _base omits version");

    // Three content scripts — contentscript.js (default world), inpage.js
    // (MAIN world for EIP-1193 provider injection), trezor popup bridge.
    assert_eq!(m.content_scripts.len(), 3);
    let main_world_count = m
        .content_scripts
        .iter()
        .filter(|c| c.world == World::Main)
        .count();
    assert_eq!(
        main_world_count, 1,
        "MetaMask should have exactly one MAIN-world inpage script"
    );

    // Service worker, classic (no `type` → default).
    let bg = m.background.as_ref().expect("MetaMask has a background");
    assert_eq!(bg.service_worker.as_deref(), Some("service-worker.ts"));
    assert_eq!(bg.r#type, BackgroundType::Classic);

    // Toolbar action with icons map + popup path.
    let action = m.action.as_ref().expect("action block present");
    assert_eq!(action.default_title.as_deref(), Some("MetaMask"));
    assert!(action.default_popup.is_some());

    // Author as a string (URL form) — exercises the `Author` untagged enum.
    match m.author.as_ref() {
        Some(tauri_plugin_extensions::manifest::Author::String(s)) => {
            assert_eq!(s, "https://metamask.io");
        }
        other => panic!("expected string author, got {other:?}"),
    }

    // Optional permissions populated — exercises the default-empty-vec path.
    assert_eq!(m.optional_permissions, vec!["clipboardRead".to_string()]);

    // `commands`, `sandbox`, `short_name` fields — extras round-trip.
    assert!(m.extra.contains_key("commands"));
    assert!(m.extra.contains_key("sandbox"));
}

#[test]
fn rabby_manifest_parses() {
    let path = fixture("rabby");
    let m = manifest::parse_from_path(&path).expect("Rabby manifest must parse");

    assert_eq!(m.manifest_version, 3);
    assert!(m.name.starts_with("__MSG_"));
    assert_eq!(m.version.as_deref(), Some("0.93.85"));
    assert_eq!(m.default_locale.as_deref(), Some("en"));

    // Rabby declares `host_permissions: ["<all_urls>"]`.
    assert_eq!(m.host_permissions, vec!["<all_urls>".to_string()]);

    // Service worker, classic, path sw.js.
    let bg = m.background.as_ref().expect("Rabby has a background");
    assert_eq!(bg.service_worker.as_deref(), Some("sw.js"));
    assert_eq!(bg.r#type, BackgroundType::Classic);

    // Two content scripts: main polyfill script + Trezor/OneKey popup bridge.
    assert_eq!(m.content_scripts.len(), 2);
    // The first is all_frames, the second is not.
    assert!(m.content_scripts[0].all_frames);

    // WAR shape: object form, single resource exposed.
    assert_eq!(m.web_accessible_resources.len(), 1);
    assert_eq!(
        m.web_accessible_resources[0].resources,
        vec!["pageProvider.js".to_string()]
    );

    // CSP present with extension_pages.
    assert!(m
        .content_security_policy
        .as_ref()
        .and_then(|c| c.extension_pages.as_deref())
        .is_some());
}
