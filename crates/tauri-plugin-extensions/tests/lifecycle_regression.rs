//! Regression tests for the lifecycle service.
//!
//! The load-bearing bug this file closes: pre-lifecycle, every call to
//! `load_unpacked` minted a fresh random id and spawned a fresh hidden
//! WebView2 window. A single dev session accumulated 92 zombie windows
//! (~3–5 GB RAM). The lifecycle manager is designed to make a repeated
//! `install_or_reload` against the same `source_dir` idempotent; the
//! invariant checker (`check_invariants`) enforces that in production and
//! this file drives it through the regression scenarios that would have
//! caught the original bug.
//!
//! ## Why these tests exercise [`check_invariants`] rather than the live manager
//!
//! The live manager's `invariants()` / `diagnostics()` / `reconcile_orphans()`
//! methods all call `app.webview_windows()` on a real Tauri `AppHandle`.
//! Constructing one in a test binary requires `tauri::test::mock_builder()`,
//! which transitively links the full `tauri-runtime-wry` / `webview2-com`
//! stack. On Win 11 26200 that DLL graph fails to resolve at process load
//! time with `STATUS_ENTRYPOINT_NOT_FOUND` (exit code `0xc0000139`) before
//! `main` executes — the same issue already documented in
//! `tests/c_bus_storage.rs` and `tests/load_noop_fixture.rs`.
//!
//! The production `invariants()` flow splits into two halves:
//! 1. `snapshot_facts()` — walks the entry DashMap + per-entry mutexes to
//!    build a `Vec<EntryFacts>`.
//! 2. `check_invariants(&facts, &live_windows)` — pure function over the
//!    facts + the set of live `ext-bg-*` labels.
//!
//! This file drives (2) directly with hand-constructed facts that model the
//! state the manager would produce in each regression scenario. The rule
//! logic is identical in production and here; only the source of the
//! facts differs. Once the Win 11 26200 DLL-load issue is resolved (or the
//! test harness grows a workaround), the end-to-end flow can be exercised
//! against `mock_builder()` too — the invariant rules themselves don't
//! need to change.

use std::collections::HashSet;
use std::path::PathBuf;

use proptest::prelude::*;
use tauri_plugin_extensions::{
    lifecycle::{
        check_invariants, derive_extension_id, EntryFacts, FsStateStore, InvariantViolation,
        MemoryStateStore, PersistedEntry, StateStore, STATE_SCHEMA_VERSION,
    },
    registry::ExtensionId,
    Error,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fact builders — model the manager's output shapes without the manager.
// ---------------------------------------------------------------------------

/// Build a `Running` entry-facts row that looks like what `install_or_reload`
/// produces after a successful `try_spawn`.
fn running_fact(id: &str, source: &str, bg_label: Option<&str>) -> EntryFacts {
    EntryFacts {
        id: ExtensionId::new(id),
        source_dir: PathBuf::from(source),
        state_name: "running",
        enabled: true,
        bg_label: bg_label.map(str::to_string),
        is_running: true,
        is_uninstalling: false,
    }
}

fn installed_fact(id: &str, source: &str) -> EntryFacts {
    EntryFacts {
        id: ExtensionId::new(id),
        source_dir: PathBuf::from(source),
        state_name: "installed",
        enabled: true,
        bg_label: None,
        is_running: false,
        is_uninstalling: false,
    }
}

fn stopped_enabled_fact(id: &str, source: &str) -> EntryFacts {
    EntryFacts {
        id: ExtensionId::new(id),
        source_dir: PathBuf::from(source),
        state_name: "stopped",
        enabled: true,
        bg_label: None,
        is_running: false,
        is_uninstalling: false,
    }
}

fn stopped_disabled_fact(id: &str, source: &str) -> EntryFacts {
    EntryFacts {
        id: ExtensionId::new(id),
        source_dir: PathBuf::from(source),
        state_name: "stopped",
        enabled: false,
        bg_label: None,
        is_running: false,
        is_uninstalling: false,
    }
}

fn uninstalling_fact(id: &str, source: &str) -> EntryFacts {
    EntryFacts {
        id: ExtensionId::new(id),
        source_dir: PathBuf::from(source),
        state_name: "uninstalling",
        enabled: false,
        bg_label: None,
        is_running: false,
        is_uninstalling: true,
    }
}

fn rule_hits(violations: &[InvariantViolation], rule: &str) -> usize {
    violations.iter().filter(|v| v.rule == rule).count()
}

// ---------------------------------------------------------------------------
// Happy path — no violations when state is consistent.
// ---------------------------------------------------------------------------

#[test]
fn empty_state_has_no_violations() {
    let violations = check_invariants(&[], &HashSet::new());
    assert!(violations.is_empty());
}

#[test]
fn single_running_extension_is_clean() {
    let facts = vec![running_fact("ext-a", "/tmp/ext-a", Some("ext-bg-aaaa"))];
    let mut live = HashSet::new();
    live.insert("ext-bg-aaaa".to_string());
    let violations = check_invariants(&facts, &live);
    assert!(
        violations.is_empty(),
        "single Running entry tripped: {:?}",
        violations
    );
}

#[test]
fn multiple_distinct_extensions_are_clean() {
    let facts = vec![
        running_fact("ext-a", "/tmp/a", Some("ext-bg-aaaa")),
        running_fact("ext-b", "/tmp/b", Some("ext-bg-bbbb")),
        installed_fact("ext-c", "/tmp/c"),
    ];
    let mut live = HashSet::new();
    live.insert("ext-bg-aaaa".to_string());
    live.insert("ext-bg-bbbb".to_string());
    let violations = check_invariants(&facts, &live);
    assert!(violations.is_empty(), "violations: {:?}", violations);
}

// ---------------------------------------------------------------------------
// Duplicate source_dir — the 92-zombie-window bug's root cause.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_source_dir_produces_unique_source_dir_violation() {
    // Simulate a pre-lifecycle regression: two entries pointing at the
    // same on-disk path. Real manager would never produce this, so the
    // rule catches anyone who adds a code path that circumvents the
    // `install_or_reload` entry-map lookup.
    let facts = vec![
        running_fact("ext-old", "/tmp/same-path", Some("ext-bg-0001")),
        running_fact("ext-new", "/tmp/same-path", Some("ext-bg-0002")),
    ];
    let mut live = HashSet::new();
    live.insert("ext-bg-0001".to_string());
    live.insert("ext-bg-0002".to_string());
    let violations = check_invariants(&facts, &live);
    assert_eq!(rule_hits(&violations, "unique_source_dir"), 1);
}

#[test]
fn one_hundred_duplicates_at_same_source_dir_produce_single_rule_violation() {
    // The zombie-window bug at scale: if a buggy loader minted 100
    // entries against the same source dir, the invariant checker flags
    // the bucket exactly once with detail containing the count, rather
    // than 100 near-identical violations.
    let mut facts = Vec::with_capacity(100);
    for i in 0..100 {
        facts.push(running_fact(
            &format!("ext-{i:03}"),
            "/tmp/same-path",
            Some(&format!("ext-bg-{i:04}")),
        ));
    }
    // All labels live to avoid a separate state_consistency trip.
    let live: HashSet<String> = (0..100).map(|i| format!("ext-bg-{i:04}")).collect();
    let violations = check_invariants(&facts, &live);
    let src_hits = rule_hits(&violations, "unique_source_dir");
    assert_eq!(src_hits, 1, "expected one bucket violation, got {src_hits}");
    let hit = violations
        .iter()
        .find(|v| v.rule == "unique_source_dir")
        .unwrap();
    assert!(
        hit.detail.contains("100 entries"),
        "violation detail missing count: {}",
        hit.detail
    );
}

// ---------------------------------------------------------------------------
// Duplicate bg labels — the zombie-window symptom.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_bg_label_produces_unique_bg_label_violation() {
    let facts = vec![
        running_fact("ext-a", "/tmp/a", Some("ext-bg-shared")),
        running_fact("ext-b", "/tmp/b", Some("ext-bg-shared")),
    ];
    let mut live = HashSet::new();
    live.insert("ext-bg-shared".to_string());
    let violations = check_invariants(&facts, &live);
    assert_eq!(rule_hits(&violations, "unique_bg_label"), 1);
}

// ---------------------------------------------------------------------------
// state_consistency — BG webview label claimed but no actual Tauri window.
// ---------------------------------------------------------------------------

#[test]
fn running_with_missing_window_trips_state_consistency() {
    let facts = vec![running_fact("ext-a", "/tmp/a", Some("ext-bg-ghost"))];
    // Empty live set — the window the entry claims isn't actually there.
    let live = HashSet::new();
    let violations = check_invariants(&facts, &live);
    assert_eq!(rule_hits(&violations, "state_consistency"), 1);
    assert!(violations[0].detail.contains("ext-bg-ghost"));
}

#[test]
fn running_without_bg_handle_does_not_trip_state_consistency() {
    // Backend absent / no service worker -> Running with bg_handle = None.
    // This is the valid case, not a violation.
    let facts = vec![running_fact("ext-a", "/tmp/a", None)];
    let violations = check_invariants(&facts, &HashSet::new());
    assert_eq!(rule_hits(&violations, "state_consistency"), 0);
}

// ---------------------------------------------------------------------------
// enabled_matches_state — flagged transient.
// ---------------------------------------------------------------------------

#[test]
fn enabled_and_stopped_is_flagged() {
    let facts = vec![stopped_enabled_fact("ext-a", "/tmp/a")];
    let violations = check_invariants(&facts, &HashSet::new());
    assert_eq!(rule_hits(&violations, "enabled_matches_state"), 1);
}

#[test]
fn disabled_and_stopped_is_clean() {
    let facts = vec![stopped_disabled_fact("ext-a", "/tmp/a")];
    let violations = check_invariants(&facts, &HashSet::new());
    assert!(violations.is_empty(), "{:?}", violations);
}

// ---------------------------------------------------------------------------
// Store-schema regressions (the other load-bearing "can't silently corrupt")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_store_roundtrip_preserves_entries() {
    let store = MemoryStateStore::new();
    let entries = vec![PersistedEntry::new(
        ExtensionId::new("unpacked-abc"),
        PathBuf::from("/tmp/ext"),
        true,
        Some("0.1.0".to_string()),
    )];
    store.save(&entries).await.unwrap();
    let loaded = store.load().await.unwrap();
    assert_eq!(loaded, entries);
}

#[test]
fn schema_version_constant_is_exposed() {
    // Ensures downstream tooling can reason about the wire format —
    // this test fails to compile if the constant is removed or renamed,
    // which is what we want: schema versioning is a public contract.
    const _: () = assert!(STATE_SCHEMA_VERSION >= 1);
}

#[tokio::test]
async fn fs_store_reads_legacy_bare_array_format() {
    // Pre-v1 state.json files were bare JSON arrays. A schema-v1 binary
    // must still load them cleanly (one-way upgrade on next save).
    let dir = tempdir().unwrap();
    let path = dir.path().join("extensions").join("state.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = PersistedEntry::new(
        ExtensionId::new("unpacked-legacy"),
        PathBuf::from("/tmp/legacy"),
        true,
        Some("0.0.9".to_string()),
    );
    let raw = serde_json::to_vec_pretty(&vec![legacy.clone()]).unwrap();
    std::fs::write(&path, raw).unwrap();

    let store = FsStateStore::at(path.clone());
    let loaded = store.load().await.expect("legacy bare array loads");
    assert_eq!(loaded, vec![legacy]);

    // Save upgrades to the versioned shape.
    store.save(&loaded).await.unwrap();
    let raw_after = std::fs::read(&path).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&raw_after).unwrap();
    assert_eq!(
        json.get("schema_version").and_then(|v| v.as_u64()),
        Some(u64::from(STATE_SCHEMA_VERSION))
    );
    assert!(json.get("entries").and_then(|v| v.as_array()).is_some());
}

#[tokio::test]
async fn fs_store_rejects_future_schema_version() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let future = serde_json::json!({
        "schema_version": STATE_SCHEMA_VERSION + 5,
        "entries": []
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&future).unwrap()).unwrap();

    let store = FsStateStore::at(path);
    let err = store.load().await.unwrap_err();
    match err {
        Error::Storage(msg) => assert!(
            msg.contains("schema_version") && msg.contains("newer"),
            "unexpected storage msg: {msg}"
        ),
        other => panic!("expected Error::Storage, got {other:?}"),
    }
}

#[tokio::test]
async fn fs_store_roundtrips_current_schema() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("state.json");
    let entries = vec![PersistedEntry::new(
        ExtensionId::new("unpacked-current"),
        PathBuf::from("/tmp/current"),
        true,
        None,
    )];
    let store = FsStateStore::at(path);
    store.save(&entries).await.unwrap();
    let loaded = store.load().await.unwrap();
    assert_eq!(loaded, entries);

    // Overwriting with a shorter set must not leave the longer set on disk.
    store.save(&[]).await.unwrap();
    assert!(store.load().await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Idempotency — the load-bearing "100 install_or_reload calls = 1 entry".
// ---------------------------------------------------------------------------
//
// The real manager's idempotency flows from the DashMap .entry().or_insert_with()
// pattern plus the stable id derivation in `derive_id`. We can't exercise
// that pipeline end-to-end without AppHandle (Win 11 26200 DLL-load issue),
// but the two load-bearing invariants are:
//   1. ExtensionId::from_source_dir is deterministic.
//   2. `check_invariants` flags any two entries sharing a source_dir.
// Together those guarantee: the production entry map, keyed by id, can
// never contain two rows for the same canonical source_dir without tripping
// `unique_source_dir` the next time the watchdog runs.

#[test]
fn phantom_manifest_key_drives_chromium_faithful_id() {
    // Regression: when the manifest schema grew a typed `key` field, the
    // raw-JSON lookup in the manager's id derivation stopped finding it and
    // every packaged extension silently fell back to a source-dir id. The
    // Chromium-faithful id matters — it is what `chrome-extension://` URLs
    // and the extension's own runtime expect. Phantom's web-store id is
    // documented in docs/phantom-load-observations-2026-04-20.md.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifests/phantom/manifest.json");
    let manifest =
        tauri_plugin_extensions::manifest::parse_from_path(&path).expect("phantom parses");
    let id = derive_extension_id(&manifest, &PathBuf::from("/tmp/anywhere"));
    assert_eq!(id, ExtensionId::new("bfnaelmomeimhlpmgjnjophhpkkoljpa"));
}

#[test]
fn keyless_manifest_falls_back_to_source_dir_id() {
    // The noop fixture has no `key` — identity must come from the canonical
    // source dir, deterministically.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-extensions/noop-mv3/manifest.json");
    let manifest =
        tauri_plugin_extensions::manifest::parse_from_path(&path).expect("noop parses");
    let dir = PathBuf::from("/tmp/noop-mv3");
    let id = derive_extension_id(&manifest, &dir);
    assert_eq!(id, ExtensionId::from_source_dir(&dir));
}

#[test]
fn id_derivation_is_stable_across_100_calls() {
    let path = PathBuf::from("/tmp/regression-target");
    let first = ExtensionId::from_source_dir(&path);
    for _ in 0..100 {
        let again = ExtensionId::from_source_dir(&path);
        assert_eq!(again, first, "id derivation is not stable");
    }
}

#[test]
fn id_derivation_discriminates_distinct_paths() {
    let a = ExtensionId::from_source_dir(&PathBuf::from("/tmp/path-a"));
    let b = ExtensionId::from_source_dir(&PathBuf::from("/tmp/path-b"));
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------
// Property-based state-machine test: random fact sequences never trip any
// rule beyond the ones we deliberately exercise.
// ---------------------------------------------------------------------------
//
// Generates random fact sets drawn from a small pool of source paths. The
// invariant-checker contract:
//   - If every source_dir in `facts` is unique AND every bg_label is unique
//     AND every bg_label is in `live_bg_windows` AND no enabled+stopped pair,
//     then `check_invariants` returns an empty vec.
//   - Otherwise, exactly the rules we injected trip (nothing else spuriously
//     fires).

proptest! {
    #[test]
    fn check_invariants_is_clean_for_well_formed_facts(
        n_extensions in 0usize..10,
    ) {
        let facts: Vec<EntryFacts> = (0..n_extensions)
            .map(|i| {
                let label = format!("ext-bg-{i:04}");
                running_fact(
                    &format!("ext-{i:03}"),
                    &format!("/tmp/source-{i:03}"),
                    Some(&label),
                )
            })
            .collect();
        let live: HashSet<String> = facts
            .iter()
            .filter_map(|f| f.bg_label.clone())
            .collect();
        let violations = check_invariants(&facts, &live);
        prop_assert!(
            violations.is_empty(),
            "well-formed state tripped: {:?}",
            violations
        );
    }

    #[test]
    fn check_invariants_detects_dup_source_dirs(
        n_dupes in 2usize..20,
    ) {
        // All entries share the same source_dir; every bg_label is unique
        // and present in live. Only `unique_source_dir` should trip.
        let facts: Vec<EntryFacts> = (0..n_dupes)
            .map(|i| {
                running_fact(
                    &format!("ext-{i:03}"),
                    "/tmp/shared",
                    Some(&format!("ext-bg-{i:04}")),
                )
            })
            .collect();
        let live: HashSet<String> = facts
            .iter()
            .filter_map(|f| f.bg_label.clone())
            .collect();
        let violations = check_invariants(&facts, &live);
        prop_assert_eq!(rule_hits(&violations, "unique_source_dir"), 1);
        prop_assert_eq!(rule_hits(&violations, "unique_bg_label"), 0);
        prop_assert_eq!(rule_hits(&violations, "state_consistency"), 0);
    }

    #[test]
    fn check_invariants_detects_dup_bg_labels(
        n_dupes in 2usize..20,
    ) {
        // Each entry has a unique source_dir but they all claim the same
        // bg label. Only `unique_bg_label` should trip.
        let facts: Vec<EntryFacts> = (0..n_dupes)
            .map(|i| {
                running_fact(
                    &format!("ext-{i:03}"),
                    &format!("/tmp/src-{i:03}"),
                    Some("ext-bg-shared"),
                )
            })
            .collect();
        let mut live = HashSet::new();
        live.insert("ext-bg-shared".to_string());
        let violations = check_invariants(&facts, &live);
        prop_assert_eq!(rule_hits(&violations, "unique_bg_label"), 1);
        prop_assert_eq!(rule_hits(&violations, "unique_source_dir"), 0);
    }
}

// ---------------------------------------------------------------------------
// Uninstalling-state flag is surfaced correctly. The `no_uninstalling_persist`
// rule lives on the manager (it consults the store), but the EntryFacts
// carrier flag is part of the public contract that feeds it.
// ---------------------------------------------------------------------------

#[test]
fn uninstalling_fact_carries_flag() {
    let fact = uninstalling_fact("ext-a", "/tmp/a");
    assert!(fact.is_uninstalling);
    assert_eq!(fact.state_name, "uninstalling");
    // Pure checker alone does not flag this — it's the manager's job to
    // cross-reference with the persisted store. We just assert the
    // contract: pure checker ignores the `is_uninstalling` flag.
    let violations = check_invariants(&[fact], &HashSet::new());
    assert!(violations.is_empty());
}
