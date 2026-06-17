//! Integration tests for the bus + storage subsystems. These tests avoid
//! constructing `LoadedExtension` / `ExtensionRegistry`, which transitively
//! drag the Tauri runtime into the test binary's link closure — and on Win11
//! 26200 that link closure triggers a STATUS_ENTRYPOINT_NOT_FOUND load
//! failure before `main` executes.
//!
//! Registry tests live in `c_registry.rs` and are compiled but ignored on
//! Windows pending resolution of that toolchain issue.

use std::collections::HashMap;

use serde_json::json;
use tauri_plugin_extensions::{
    ipc::{
        bus::Bus,
        command::{PortInfo, PortSurface},
        PortId,
    },
    registry::ExtensionId,
    storage::{LocalStorage, LocalStorageManager, SessionStorage, SessionStorageManager},
    Error,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// ExtensionId derivation (Chrome-style key hashing)
// ---------------------------------------------------------------------------

#[test]
fn extension_id_from_key_is_stable() {
    // Simple valid base64 — just the 4 bytes 0x00 0x10 0x83 = "ABCD" in
    // standard base64. The id derivation only needs the decoded bytes to
    // hash; we don't care that this isn't a real RSA DER.
    let key = "ABCD";
    let id = ExtensionId::from_key(key).expect("valid base64 decodes");
    let s = id.as_str();
    assert_eq!(s.len(), 32, "Chrome id is 32 chars");
    assert!(
        s.chars().all(|c| ('a'..='p').contains(&c)),
        "every char in `a..=p`: got {s}"
    );
    // Determinism: same key, same id.
    let again = ExtensionId::from_key(key).unwrap();
    assert_eq!(id, again);
    // Different key, different id.
    let other = ExtensionId::from_key("EFGH").unwrap();
    assert_ne!(id, other);
}

#[test]
fn extension_id_from_key_rejects_garbage() {
    assert!(ExtensionId::from_key("!!!not base64!!!").is_none());
}

#[test]
fn extension_id_from_source_dir_is_stable_and_path_sensitive() {
    use std::path::PathBuf;
    let a = ExtensionId::from_source_dir(&PathBuf::from("/tmp/ext-one"));
    let a2 = ExtensionId::from_source_dir(&PathBuf::from("/tmp/ext-one"));
    let b = ExtensionId::from_source_dir(&PathBuf::from("/tmp/ext-two"));
    assert_eq!(a, a2, "same path -> same id");
    assert_ne!(a, b, "different path -> different id");
    assert!(a.as_str().starts_with("unpacked-"));
    assert_eq!(a.as_str().len(), "unpacked-".len() + 32);
    // Trailing separator must not perturb the id.
    let trailing = ExtensionId::from_source_dir(&PathBuf::from("/tmp/ext-one/"));
    assert_eq!(a, trailing);
}

#[test]
#[cfg(target_os = "windows")]
fn extension_id_from_source_dir_is_case_insensitive_on_windows() {
    use std::path::PathBuf;
    let lower = ExtensionId::from_source_dir(&PathBuf::from("c:/users/me/ext"));
    let upper = ExtensionId::from_source_dir(&PathBuf::from("C:/Users/Me/EXT"));
    assert_eq!(lower, upper);
}

// Lifecycle state-machine + store unit tests live inline in
// `src/lifecycle/{state,store}.rs`. Running them requires a cargo-test env
// where WebView2Loader.dll resolves at load time (a real Tauri dev run or
// a future fix to the test-harness link closure) — pulling them into this
// integration file via `use tauri_plugin_extensions::lifecycle::*` trips
// STATUS_ENTRYPOINT_NOT_FOUND on Win11 26200 before any test executes.

// ---------------------------------------------------------------------------
// Session storage
// ---------------------------------------------------------------------------

#[test]
fn session_storage_roundtrip() {
    let s = SessionStorage::new();
    s.set("a".into(), json!(1));
    s.set("b".into(), json!("two"));
    assert_eq!(s.get("a"), Some(json!(1)));

    let many = s.get_many(Some(&["a".to_string(), "missing".to_string()]));
    assert_eq!(many.len(), 1);
    let all = s.get_many(None);
    assert_eq!(all.len(), 2);

    let mut batch = HashMap::new();
    batch.insert("c".to_string(), json!([1, 2, 3]));
    batch.insert("b".to_string(), json!("overwrite"));
    s.set_many(batch);
    assert_eq!(s.get("b"), Some(json!("overwrite")));

    s.remove_many(&["a".to_string(), "b".to_string()]);
    assert!(s.get("a").is_none());
    assert!(s.get("c").is_some());
    s.clear();
    assert!(s.get("c").is_none());
}

#[test]
fn session_storage_manager_isolates_extensions() {
    let mgr = SessionStorageManager::new();
    let a = mgr.for_extension(&ExtensionId::new("a"));
    let b = mgr.for_extension(&ExtensionId::new("b"));
    a.set("shared".into(), json!("from-a"));
    assert!(b.get("shared").is_none());
    mgr.drop_extension(&ExtensionId::new("a"));
    let a2 = mgr.for_extension(&ExtensionId::new("a"));
    assert!(a2.get("shared").is_none());
}

// ---------------------------------------------------------------------------
// Local (disk-backed) storage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_storage_persists_across_handles() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("storage-local.json");
    let store = LocalStorage::new(path.clone());

    let mut batch = HashMap::new();
    batch.insert("a".to_string(), json!(1));
    batch.insert("b".to_string(), json!("two"));
    store.set_many(batch).await.unwrap();

    // Fresh handle reads from disk.
    let fresh = LocalStorage::new(path.clone());
    let all = fresh.get_many(None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all.get("a"), Some(&json!(1)));

    store.remove_many(&["a".to_string()]).await.unwrap();
    let after = LocalStorage::new(path.clone()).get_many(None).await.unwrap();
    assert_eq!(after.len(), 1);
    assert!(after.contains_key("b"));

    store.clear().await.unwrap();
    assert!(LocalStorage::new(path).get_many(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn local_storage_manager_isolates_extensions() {
    let dir = tempdir().unwrap();
    let mgr = LocalStorageManager::new(dir.path());
    let a = mgr.for_extension(&ExtensionId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    let b = mgr.for_extension(&ExtensionId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));

    let mut batch = HashMap::new();
    batch.insert("x".to_string(), json!("from-a"));
    a.set_many(batch).await.unwrap();

    assert!(b.get_many(None).await.unwrap().is_empty());
    assert_eq!(
        a.get_many(None).await.unwrap().get("x"),
        Some(&json!("from-a"))
    );
}

// ---------------------------------------------------------------------------
// IPC bus
// ---------------------------------------------------------------------------

fn info() -> PortInfo {
    PortInfo {
        extension_id: ExtensionId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        surface: PortSurface::Background,
        connector_name: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bus_send_with_reply_round_trips() {
    let bus = Bus::new();
    let from = PortId::new();
    let to = PortId::new();
    let mut rx = bus.register_port(to.clone(), info());

    let _responder = tokio::spawn(async move {
        if let Some(msg) = rx.recv().await {
            if let Some(reply) = msg.reply {
                let _ = reply.send(json!({"echo": msg.payload}));
            }
        }
    });

    let resp = bus
        .send(from, to, json!({"op": "ping"}))
        .await
        .expect("send succeeds");
    assert_eq!(resp, json!({"echo": {"op": "ping"}}));
}

#[tokio::test]
async fn bus_send_to_unknown_port_errors() {
    let bus = Bus::new();
    let err = bus
        .send(PortId::new(), PortId::new(), json!(null))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Ipc(_)), "got: {err:?}");
}

#[tokio::test]
async fn bus_unregister_drops_sender() {
    let bus = Bus::new();
    let from = PortId::new();
    let to = PortId::new();
    let _rx = bus.register_port(to.clone(), info());
    bus.unregister_port(&to);
    let err = bus.send(from, to, json!(null)).await.unwrap_err();
    assert!(matches!(err, Error::Ipc(_)));
}

#[tokio::test]
async fn bus_post_is_fire_and_forget() {
    let bus = Bus::new();
    let to = PortId::new();
    let mut rx = bus.register_port(to.clone(), info());
    bus.post(PortId::new(), to, json!({"p": 1}))
        .expect("post ok");
    let msg = rx.recv().await.expect("message delivered");
    assert!(msg.reply.is_none());
    assert_eq!(msg.payload, json!({"p": 1}));
}
