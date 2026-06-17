//! Integration coverage for the faithful background-host serving + load path
//! (D-008). The hidden background webview is the extension's privileged origin:
//! it loads its service worker from the resource scheme and may read any
//! packaged file (the SW + its `import`/`importScripts` chunks are not
//! `web_accessible_resources`), while ordinary pages stay WAR-gated.
//!
//! These tests exercise the public `runtime::resources` contract the scheme
//! handler is built from — against a real temp extension dir — so the
//! privileged-vs-page decision, the traversal guards, the service-worker load
//! plan, and the synchronously-maintained [`ResourceRegistry`] are all locked
//! without needing a live webview (the actual module/import execution is proven
//! live by the minimal-host acceptance matrix).

use std::fs;

use tauri_plugin_extensions::registry::ExtensionId;
use tauri_plugin_extensions::runtime::{
    bg_window_label,
    resources::{
        resolve_resource, resolve_resource_privileged, serve_mode, split_request_path,
        sw_load_plan, sw_loader_script, ResourceError, ResourceRegistry, ServeMode,
    },
};

/// Lay down a minimal unpacked extension: a private module SW + import chunk,
/// plus one page-exposed (WAR) file.
fn make_ext() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("background")).unwrap();
    fs::write(
        dir.path().join("background/sw.js"),
        "import { x } from '../chunk.js'; globalThis.__ok = x;",
    )
    .unwrap();
    fs::write(dir.path().join("chunk.js"), "export const x = 1;").unwrap();
    fs::write(dir.path().join("pageProvider.js"), "window.__war = true;").unwrap();
    dir
}

#[test]
fn background_webview_may_read_its_private_service_worker_but_a_page_may_not() {
    let dir = make_ext();
    let war = vec!["pageProvider.js".to_string()];

    // The SW is private (NOT in web_accessible_resources). A page request is
    // refused...
    assert_eq!(
        resolve_resource(dir.path(), &war, "background/sw.js"),
        Err(ResourceError::NotWebAccessible),
    );
    // ...but the extension's own background webview reads it through the
    // privileged path, and the bytes are the real file.
    let resolved = resolve_resource_privileged(dir.path(), "background/sw.js").unwrap();
    let body = fs::read_to_string(&resolved).unwrap();
    assert!(body.contains("globalThis.__ok"));

    // The WAR file is reachable both ways.
    assert!(resolve_resource(dir.path(), &war, "pageProvider.js").is_ok());
    assert!(resolve_resource_privileged(dir.path(), "pageProvider.js").is_ok());

    // Traversal is rejected on the privileged path too.
    assert_eq!(
        resolve_resource_privileged(dir.path(), "../escape.js"),
        Err(ResourceError::Traversal),
    );
}

#[test]
fn serve_mode_keys_off_the_owning_background_label() {
    let id = ExtensionId::new("bfnaelmomeimhlpmgjnjophhpkkoljpa");
    let owner = bg_window_label(&id);

    // The extension's own BG window is privileged for its own id.
    assert_eq!(serve_mode(&owner, &owner), ServeMode::Privileged);
    // The host's main window and a dapp window are not.
    assert_eq!(serve_mode("main", &owner), ServeMode::WebAccessible);
    assert_eq!(serve_mode("test-dapp", &owner), ServeMode::WebAccessible);
    // A different extension's BG window cannot read this id's private files.
    let other = bg_window_label(&ExtensionId::new("some-other-extension-id"));
    assert_eq!(serve_mode(&other, &owner), ServeMode::WebAccessible);
}

#[test]
fn sw_load_plan_and_loader_compose_for_a_module_worker() {
    let id = "bfnaelmomeimhlpmgjnjophhpkkoljpa";
    let plan = sw_load_plan("http://extres.localhost", id, "background/sw.js", true);
    assert_eq!(
        plan.url,
        format!("http://extres.localhost/{id}/background/sw.js")
    );
    assert_eq!(plan.dir, format!("http://extres.localhost/{id}/background/"));

    // The split the scheme handler performs round-trips the planned URL back to
    // (id, rel) so privileged resolve can find the file on disk.
    let path = format!("/{id}/background/sw.js");
    let (got_id, rel) = split_request_path(&path).unwrap();
    assert_eq!(got_id, id);
    assert_eq!(rel, "background/sw.js");

    let js = sw_loader_script(&plan);
    // Module worker: dynamic import, no eval needed; entry source is NOT inlined.
    assert!(js.contains("import(SW_URL)"));
    assert!(js.contains(&plan.url));
    assert!(!js.contains("globalThis.__ok"), "module source must not be inlined");
}

#[test]
fn sw_loader_classic_path_drives_the_synchronous_importscripts_shim() {
    let plan = sw_load_plan("http://extres.localhost", "id", "sw.js", false);
    let js = sw_loader_script(&plan);
    assert!(js.contains("self.importScripts(SW_URL)"));
    assert!(js.contains("xhr.open('GET', u, false)"));
}

#[test]
fn resource_registry_resolves_a_just_upserted_root_before_projection() {
    // Models the boot race: the BG fetches its SW before the async
    // ExtensionRegistry projection lands, so the handler must resolve through
    // the synchronously-upserted ResourceRegistry.
    let dir = make_ext();
    let reg = ResourceRegistry::new();
    let id = ExtensionId::new("ext-boot-race");
    reg.upsert(
        id.clone(),
        dir.path().to_path_buf(),
        vec!["pageProvider.js".to_string()],
    );

    let root = reg.get(&id).expect("root available immediately after upsert");
    // Privileged read of the private SW resolves under the upserted root.
    let resolved = resolve_resource_privileged(&root.source_dir, "background/sw.js").unwrap();
    assert!(resolved.starts_with(dir.path()));
    assert!(fs::read_to_string(&resolved).unwrap().contains("__ok"));

    reg.remove(&id);
    assert!(reg.get(&id).is_none());
}
