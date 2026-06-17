//! `LifecycleManager` — the single owner of extension runtime state.
//!
//! Every mutation of an extension's lifecycle goes through one of the
//! manager's async methods. The registry is a read-only projection fed by
//! the manager; the backend is consulted for BG-webview spawn/shutdown and
//! never driven directly by the loader.
//!
//! ## Concurrency model
//!
//! - Top-level entry map is a [`DashMap`] — lookup-heavy, rarely iterated.
//! - Each entry sits behind an [`tokio::sync::Mutex`] so transitions that
//!   span an `await` (spawning / shutting down a BG webview) serialize per
//!   extension without blocking unrelated extensions.
//! - `install_or_reload` against the same path from two concurrent callers
//!   serializes on the entry mutex: one wins the reload, the other sees the
//!   fresh `Running` state and returns immediately.
//!
//! ## Orphan reconciliation
//!
//! At boot, [`LifecycleManager::reconcile_orphans`] walks every Tauri
//! `WebviewWindow` whose label starts with `ext-bg-`, compares against the
//! set of windows it knows about, and closes any that have no owner. This
//! is how we close the memory-hogging-zombie-window bug: a pre-lifecycle
//! build that leaked 92 hidden webviews gets cleaned up the first time the
//! rebuilt binary starts.
//!
//! ## Shutdown
//!
//! [`LifecycleManager::shutdown_all`] transitions every `Running` entry to
//! `Stopped { Shutdown }`, calling `BackgroundHandle::shutdown` on each.
//! Call from the host's `RunEvent::ExitRequested` / `RunEvent::Exit`
//! handler to guarantee no leaked WebView2 handles cross process exit.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::SystemTime,
};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, Runtime};
use tokio::sync::Mutex;

use crate::{
    manifest::{self, Manifest},
    matcher::{MatchPattern, MatchPatternSet},
    registry::{ContentScriptRule, ExtensionId, ExtensionRegistry, ExtensionSummary, LoadedExtension},
    runtime::{background::BackgroundHandle, Backend, RunAt, World},
    Error, Result,
};

use super::{
    events::{LifecycleEvent, EVENT_NAME},
    state::{ExtensionState, StateSnapshot, StopReason},
    store::{PersistedEntry, StateStore},
};

/// A single extension's lifecycle record. Wrapped in an `Arc<Mutex<_>>` in
/// the manager's entry map so per-entry transitions serialize.
#[derive(Debug)]
pub struct LifecycleEntry {
    /// Stable id.
    pub id: ExtensionId,
    /// Canonical absolute source directory.
    pub source_dir: PathBuf,
    /// Parsed manifest. Replaced on each reload.
    pub manifest: Arc<Manifest>,
    /// Compiled content-script rules. Replaced on each reload.
    pub content_scripts: Vec<ContentScriptRule>,
    /// Whether the extension should auto-start on boot / reload.
    pub enabled: bool,
    /// Current runtime state. Only the manager mutates this.
    pub state: ExtensionState,
}

/// IPC-safe summary of an entry. Returned by `list` commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleSummary {
    /// Extension id.
    pub id: ExtensionId,
    /// Manifest name.
    pub name: String,
    /// Manifest version (may be absent in MM's `_base.json`).
    pub version: Option<String>,
    /// Manifest description.
    pub description: Option<String>,
    /// Canonical absolute source dir as a string.
    pub source_dir: String,
    /// Whether the extension is enabled. Disabled extensions stay in
    /// `Stopped` and do not auto-start.
    pub enabled: bool,
    /// Runtime state snapshot.
    pub state: StateSnapshot,
}

/// A single invariant violation. Surfaced by
/// [`LifecycleManager::invariants`] and folded into
/// [`Diagnostics::invariant_violations`].
///
/// `rule` is a stable short name safe to branch on in host code / tests
/// (e.g. `"unique_source_dir"`); `detail` is a human-readable explanation
/// of which entries triggered it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvariantViolation {
    /// Stable rule identifier. Reserved tokens at time of writing:
    /// `unique_source_dir`, `unique_bg_label`, `state_consistency`,
    /// `enabled_matches_state`, `no_uninstalling_persist`.
    pub rule: String,
    /// Human-readable context — extension ids, window labels, or whatever
    /// is useful for a post-mortem.
    pub detail: String,
}

/// Observable snapshot of the lifecycle service. Returned by
/// [`LifecycleManager::diagnostics`] and the `extensions_diagnostics`
/// Tauri command. Consumed by the minimal host + watchdog tests to prove
/// the zombie-window bug cannot regress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Total entries currently tracked by the manager (any state).
    pub count_entries: usize,
    /// Entries currently in [`ExtensionState::Running`].
    pub count_running: usize,
    /// Total Tauri windows whose label starts with `ext-bg-`.
    pub count_bg_windows_total: usize,
    /// `ext-bg-*` windows that correspond to a Running entry the manager
    /// knows about.
    pub count_bg_windows_owned: usize,
    /// `ext-bg-*` windows that have no Running owner — the zombie-window
    /// signal. Non-zero here is what the watchdog fires its alert on.
    pub count_bg_windows_orphaned: usize,
    /// Total orphan windows the manager has reaped across the lifetime
    /// of this process.
    pub orphan_reaps_total: u64,
    /// Total watchdog sweeps this process has performed.
    pub watchdog_runs_total: u64,
    /// Any invariant violations detected on this call. Empty when healthy.
    pub invariant_violations: Vec<InvariantViolation>,
    /// Convenience flag — `invariant_violations.is_empty()`.
    pub invariants_ok: bool,
}

/// Extension-lifecycle manager. See the module docs for the concurrency
/// model, orphan-reconciliation flow, and shutdown contract.
pub struct LifecycleManager<R: Runtime = tauri::Wry> {
    entries: DashMap<ExtensionId, Arc<Mutex<LifecycleEntry>>>,
    backend: Option<Arc<dyn Backend>>,
    store: Arc<dyn StateStore>,
    app: tauri::AppHandle<R>,
    /// Cumulative orphan-window reap counter. Bumped inside
    /// [`LifecycleManager::reconcile_orphans`].
    reap_counter: AtomicU64,
    /// Cumulative watchdog-sweep counter. Bumped by
    /// [`LifecycleManager::bump_watchdog_counter`] from the background task.
    watchdog_counter: AtomicU64,
}

impl<R: Runtime> std::fmt::Debug for LifecycleManager<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleManager")
            .field("entries", &self.entries.len())
            .field("backend_present", &self.backend.is_some())
            .field("store", &self.store)
            .finish()
    }
}

impl<R: Runtime> LifecycleManager<R> {
    /// Construct bound to a Tauri app, optional backend, and a state store.
    /// The backend is optional so non-Windows / test hosts can still
    /// exercise the manager's state machine without a real Backend impl.
    pub fn new(
        app: tauri::AppHandle<R>,
        backend: Option<Arc<dyn Backend>>,
        store: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            entries: DashMap::new(),
            backend,
            store,
            app,
            reap_counter: AtomicU64::new(0),
            watchdog_counter: AtomicU64::new(0),
        }
    }

    // -------------------------------------------------------------------
    // Public transition API. Every method here is the ONLY supported way
    // to mutate extension state — the registry is read-only.
    // -------------------------------------------------------------------

    /// Install-or-reload an unpacked extension by directory path.
    ///
    /// Idempotent by `source_dir`: calling it twice against the same path
    /// reloads the existing entry rather than creating a duplicate. On
    /// reload, the current BG webview is shut down before the new one
    /// spawns — the observer sees a single `Reloaded` event, not
    /// `Stopped` + `Started`.
    pub async fn install_or_reload(&self, path: &Path) -> Result<ExtensionId> {
        let (manifest, content_scripts, bg_source, source_dir) = self.parse(path).await?;
        let id = derive_id(&manifest, &source_dir)?;

        let entry_arc = self
            .entries
            .entry(id.clone())
            .or_insert_with(|| {
                Arc::new(Mutex::new(LifecycleEntry {
                    id: id.clone(),
                    source_dir: source_dir.clone(),
                    manifest: Arc::clone(&manifest),
                    content_scripts: content_scripts.clone(),
                    enabled: true,
                    state: ExtensionState::Installed {
                        installed_at: SystemTime::now(),
                    },
                }))
            })
            .clone();

        let mut entry = entry_arc.lock().await;

        let is_reload = matches!(
            entry.state,
            ExtensionState::Running { .. } | ExtensionState::Stopped { .. }
        );
        let was_running = entry.state.is_running();

        // Replace manifest + rules regardless of previous state — the
        // on-disk manifest is the source of truth.
        entry.manifest = Arc::clone(&manifest);
        entry.content_scripts = content_scripts;
        entry.source_dir = source_dir.clone();

        // If it was running, stop it cleanly before spawning the new BG.
        if was_running {
            self.internal_stop(&mut entry, StopReason::Reload).await?;
        }

        // Publish the resource root synchronously BEFORE spawning so the BG
        // webview can fetch its own service worker from the resource origin
        // without racing the end-of-transition registry projection (D-008).
        self.upsert_resource_root(&id, &source_dir, &manifest);

        // Try to spawn the new BG. A missing backend or a missing SW
        // source leaves the entry in Stopped{Crashed}-on-explicit-error
        // or Running{bg_handle:None}-on-clean-no-SW.
        let new_handle = self
            .try_spawn(&id, bg_source.as_deref(), &manifest)
            .await?;

        entry.state = ExtensionState::Running {
            started_at: SystemTime::now(),
            bg_handle: new_handle,
        };
        entry.enabled = true;

        let bg_present = matches!(
            entry.state,
            ExtensionState::Running {
                bg_handle: Some(_),
                ..
            }
        );

        // Emit event + persist BEFORE dropping the mutex so observers never
        // see a half-transition.
        if is_reload {
            self.emit(LifecycleEvent::Reloaded {
                id: id.clone(),
                bg_present,
            });
        } else {
            self.emit(LifecycleEvent::Installed {
                id: id.clone(),
                name: entry.manifest.name.clone(),
                version: entry.manifest.version.clone(),
            });
            self.emit(LifecycleEvent::Started {
                id: id.clone(),
                bg_present,
            });
        }

        drop(entry);

        if let Err(e) = self.persist().await {
            tracing::warn!(error = %e, "state-store save failed after install_or_reload");
        }
        self.reproject().await;

        Ok(id)
    }

    /// Uninstall by id — shuts down BG, removes from registry, persists.
    pub async fn uninstall(&self, id: &ExtensionId) -> Result<()> {
        let entry_arc = self
            .entries
            .get(id)
            .map(|e| e.clone())
            .ok_or_else(|| Error::ExtensionNotFound(id.to_string()))?;

        let mut entry = entry_arc.lock().await;
        let prev = std::mem::replace(&mut entry.state, ExtensionState::Uninstalling);
        if let ExtensionState::Running {
            bg_handle: Some(handle),
            ..
        } = prev
        {
            if let Err(e) = handle.shutdown().await {
                tracing::warn!(extension = %id, error = %e, "bg shutdown during uninstall");
            }
        }
        drop(entry);

        self.entries.remove(id);
        // Stop serving the (now-gone) extension's files over the resource scheme.
        if let Some(rr) = self
            .app
            .try_state::<crate::runtime::resources::ResourceRegistry>()
        {
            rr.remove(id);
        }
        self.emit(LifecycleEvent::Uninstalled { id: id.clone() });
        if let Err(e) = self.persist().await {
            tracing::warn!(error = %e, "state-store save failed after uninstall");
        }
        self.reproject().await;
        Ok(())
    }

    /// Disable — stop the BG worker but keep the entry installed. Next
    /// `enable` / `reload` starts it again.
    pub async fn disable(&self, id: &ExtensionId) -> Result<()> {
        let entry_arc = self
            .entries
            .get(id)
            .map(|e| e.clone())
            .ok_or_else(|| Error::ExtensionNotFound(id.to_string()))?;
        let mut entry = entry_arc.lock().await;
        if entry.state.can_stop() {
            self.internal_stop(&mut entry, StopReason::UserRequested).await?;
        }
        entry.enabled = false;
        drop(entry);

        self.emit(LifecycleEvent::Disabled { id: id.clone() });
        if let Err(e) = self.persist().await {
            tracing::warn!(error = %e, "state-store save failed after disable");
        }
        self.reproject().await;
        Ok(())
    }

    /// Enable — start the BG worker for a currently-disabled or stopped
    /// entry.
    pub async fn enable(&self, id: &ExtensionId) -> Result<()> {
        let entry_arc = self
            .entries
            .get(id)
            .map(|e| e.clone())
            .ok_or_else(|| Error::ExtensionNotFound(id.to_string()))?;
        let mut entry = entry_arc.lock().await;
        if entry.state.is_running() {
            entry.enabled = true;
            return Ok(());
        }
        let bg_source = read_bg_source(&entry.source_dir, &entry.manifest).await?;
        let manifest = Arc::clone(&entry.manifest);
        // Publish the resource root before spawn (see install_or_reload).
        self.upsert_resource_root(id, &entry.source_dir, &manifest);
        let new_handle = self
            .try_spawn(id, bg_source.as_deref(), &manifest)
            .await?;
        entry.state = ExtensionState::Running {
            started_at: SystemTime::now(),
            bg_handle: new_handle,
        };
        entry.enabled = true;
        let bg_present = matches!(
            entry.state,
            ExtensionState::Running {
                bg_handle: Some(_),
                ..
            }
        );
        drop(entry);

        self.emit(LifecycleEvent::Enabled { id: id.clone() });
        self.emit(LifecycleEvent::Started {
            id: id.clone(),
            bg_present,
        });
        if let Err(e) = self.persist().await {
            tracing::warn!(error = %e, "state-store save failed after enable");
        }
        self.reproject().await;
        Ok(())
    }

    /// Reload — functionally equivalent to `install_or_reload` against the
    /// extension's current `source_dir`.
    pub async fn reload(&self, id: &ExtensionId) -> Result<()> {
        let source_dir = self
            .entries
            .get(id)
            .ok_or_else(|| Error::ExtensionNotFound(id.to_string()))?
            .lock()
            .await
            .source_dir
            .clone();
        self.install_or_reload(&source_dir).await?;
        Ok(())
    }

    // -------------------------------------------------------------------
    // Query surface — used by the registry projection + command layer.
    // -------------------------------------------------------------------

    /// Snapshot list for IPC / UI consumption.
    pub async fn list(&self) -> Vec<LifecycleSummary> {
        let ids: Vec<_> = self.entries.iter().map(|e| e.key().clone()).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(entry_arc) = self.entries.get(&id).map(|e| e.clone()) {
                let entry = entry_arc.lock().await;
                out.push(LifecycleSummary {
                    id: entry.id.clone(),
                    name: entry.manifest.name.clone(),
                    version: entry.manifest.version.clone(),
                    description: entry.manifest.description.clone(),
                    source_dir: entry.source_dir.to_string_lossy().into_owned(),
                    enabled: entry.enabled,
                    state: StateSnapshot::from(&entry.state),
                });
            }
        }
        out
    }

    /// Refresh a shared read-only registry projection. Called after any
    /// state-changing operation so external consumers (host frontend,
    /// plugin-internal matcher path) observe a consistent snapshot.
    pub async fn project_into_registry(&self, registry: &ExtensionRegistry) {
        let ids: Vec<_> = self.entries.iter().map(|e| e.key().clone()).collect();
        // Remove any ids the registry has that we no longer know about.
        for existing in registry.ids() {
            if !self.entries.contains_key(&existing) {
                registry.remove(&existing);
            }
        }
        // Insert / replace the current entries.
        for id in ids {
            let Some(entry_arc) = self.entries.get(&id).map(|e| e.clone()) else {
                continue;
            };
            let entry = entry_arc.lock().await;
            registry.insert(LoadedExtension {
                id: entry.id.clone(),
                source_dir: entry.source_dir.clone(),
                manifest: Arc::clone(&entry.manifest),
                content_scripts: entry.content_scripts.clone(),
                background_handle: parking_lot::Mutex::new(None),
            });
        }
    }

    /// IPC-safe list projection via the legacy registry shape.
    pub async fn legacy_registry_summaries(&self) -> Vec<ExtensionSummary> {
        self.list()
            .await
            .into_iter()
            .map(|e| ExtensionSummary {
                id: e.id,
                name: e.name,
                version: e.version.unwrap_or_default(),
                description: e.description,
            })
            .collect()
    }

    // -------------------------------------------------------------------
    // Boot-time operations.
    // -------------------------------------------------------------------

    /// Load every persisted entry that was last-known-enabled and run
    /// `install_or_reload` against it. Returns the count of entries
    /// successfully restored. Failures are logged at `warn` and skipped
    /// rather than aborting the whole boot.
    pub async fn boot_restore(&self) -> Result<usize> {
        let persisted = self.store.load().await?;
        let mut restored = 0;
        for row in persisted.into_iter().filter(|r| r.enabled) {
            match self.install_or_reload(&row.source_dir).await {
                Ok(_) => restored += 1,
                Err(e) => {
                    tracing::warn!(
                        extension = %row.id,
                        path = %row.source_dir.display(),
                        error = %e,
                        "boot restore failed; extension not re-installed"
                    );
                }
            }
        }
        Ok(restored)
    }

    /// Walk every `ext-bg-*` Tauri window and close any that don't
    /// correspond to a Running entry owned by this manager. This is the
    /// zombie-window cleanup that closes the pre-lifecycle memory leak.
    pub async fn reconcile_orphans(&self) -> Result<usize> {
        let mut known: HashSet<String> = HashSet::new();
        let ids: Vec<_> = self.entries.iter().map(|e| e.key().clone()).collect();
        for id in ids {
            if let Some(entry_arc) = self.entries.get(&id).map(|e| e.clone()) {
                let entry = entry_arc.lock().await;
                if let ExtensionState::Running {
                    bg_handle: Some(h), ..
                } = &entry.state
                {
                    known.insert(h.label.clone());
                }
            }
        }

        let all_labels: Vec<String> = self
            .app
            .webview_windows()
            .keys()
            .filter(|l| l.starts_with("ext-bg-"))
            .cloned()
            .collect();

        let mut reaped = 0usize;
        for label in all_labels {
            if known.contains(&label) {
                continue;
            }
            if let Some(window) = self.app.get_webview_window(&label) {
                match window.close() {
                    Ok(_) => {
                        reaped += 1;
                        self.emit(LifecycleEvent::OrphanReaped { label });
                    }
                    Err(e) => {
                        tracing::warn!(label = %window.label(), error = %e, "orphan close failed");
                    }
                }
            }
        }

        if reaped > 0 {
            tracing::info!(count = reaped, "reaped orphan ext-bg windows");
            self.reap_counter
                .fetch_add(reaped as u64, Ordering::Relaxed);
        }
        Ok(reaped)
    }

    /// Bump the watchdog-run counter. Intended to be called from the
    /// background watchdog task in `lib.rs::init().setup()` on every
    /// sweep so [`Diagnostics::watchdog_runs_total`] reflects liveness.
    pub fn bump_watchdog_counter(&self) {
        self.watchdog_counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Check every invariant the lifecycle service promises to maintain.
    /// Returns an empty vec when healthy. Does not mutate state.
    ///
    /// Rules checked:
    /// - `unique_source_dir`: no two entries share a canonical `source_dir`.
    /// - `unique_bg_label`: no two `Running` entries share a `bg_handle.label`.
    /// - `state_consistency`: every `Running { bg_handle: Some(_) }` entry's
    ///   window actually exists in the Tauri app.
    /// - `enabled_matches_state`: `enabled && Stopped` is permitted but
    ///   flagged (a manager transition is mid-flight or someone disabled
    ///   externally — useful diagnostic signal).
    /// - `no_uninstalling_persist`: any entry in `Uninstalling` must not
    ///   appear in the persisted store.
    pub async fn invariants(&self) -> Vec<InvariantViolation> {
        let facts = self.snapshot_facts().await;
        let live_windows: HashSet<String> =
            self.app.webview_windows().into_keys().collect();

        let mut violations = check_invariants(&facts, &live_windows);

        // no_uninstalling_persist — consulted against the real store.
        if facts.iter().any(|f| f.is_uninstalling) {
            match self.store.load().await {
                Ok(persisted) => {
                    let persisted_ids: HashSet<ExtensionId> =
                        persisted.iter().map(|p| p.id.clone()).collect();
                    for f in &facts {
                        if f.is_uninstalling && persisted_ids.contains(&f.id) {
                            violations.push(InvariantViolation {
                                rule: "no_uninstalling_persist".into(),
                                detail: format!(
                                    "extension {} is in Uninstalling state but still present in the persisted store",
                                    f.id
                                ),
                            });
                        }
                    }
                }
                Err(e) => {
                    // Don't promote a store error into a false-positive
                    // violation — log at warn and move on.
                    tracing::warn!(
                        error = %e,
                        "invariants: state-store load failed; no_uninstalling_persist skipped"
                    );
                }
            }
        }

        violations
    }

    /// Walk entries and produce a [`EntryFacts`] snapshot. Cross-cuts every
    /// per-entry mutex serially — safe, but don't call it hot-path.
    async fn snapshot_facts(&self) -> Vec<EntryFacts> {
        let ids: Vec<ExtensionId> = self.entries.iter().map(|e| e.key().clone()).collect();
        let mut facts: Vec<EntryFacts> = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(entry_arc) = self.entries.get(id).map(|e| e.clone()) {
                let entry = entry_arc.lock().await;
                let bg_label = match &entry.state {
                    ExtensionState::Running {
                        bg_handle: Some(h), ..
                    } => Some(h.label.clone()),
                    _ => None,
                };
                facts.push(EntryFacts {
                    id: entry.id.clone(),
                    source_dir: entry.source_dir.clone(),
                    state_name: entry.state.name(),
                    enabled: entry.enabled,
                    bg_label,
                    is_running: entry.state.is_running(),
                    is_uninstalling: matches!(entry.state, ExtensionState::Uninstalling),
                });
            }
        }
        facts
    }

    /// Build a full [`Diagnostics`] snapshot: counts, counters, invariant
    /// scan. Safe to call from any thread; does not mutate state.
    pub async fn diagnostics(&self) -> Diagnostics {
        let ids: Vec<ExtensionId> = self.entries.iter().map(|e| e.key().clone()).collect();
        let count_entries = ids.len();

        // Collect the set of owned labels + the running count in a single
        // pass so we don't re-walk the DashMap.
        let mut owned_labels: HashSet<String> = HashSet::new();
        let mut count_running = 0usize;
        for id in &ids {
            if let Some(entry_arc) = self.entries.get(id).map(|e| e.clone()) {
                let entry = entry_arc.lock().await;
                if entry.state.is_running() {
                    count_running += 1;
                }
                if let ExtensionState::Running {
                    bg_handle: Some(h), ..
                } = &entry.state
                {
                    owned_labels.insert(h.label.clone());
                }
            }
        }

        let all_bg_labels: Vec<String> = self
            .app
            .webview_windows()
            .into_keys()
            .filter(|l| l.starts_with("ext-bg-"))
            .collect();
        let count_bg_windows_total = all_bg_labels.len();
        let count_bg_windows_owned = all_bg_labels
            .iter()
            .filter(|l| owned_labels.contains(*l))
            .count();
        let count_bg_windows_orphaned = count_bg_windows_total - count_bg_windows_owned;

        let invariant_violations = self.invariants().await;
        let invariants_ok = invariant_violations.is_empty();

        Diagnostics {
            count_entries,
            count_running,
            count_bg_windows_total,
            count_bg_windows_owned,
            count_bg_windows_orphaned,
            orphan_reaps_total: self.reap_counter.load(Ordering::Relaxed),
            watchdog_runs_total: self.watchdog_counter.load(Ordering::Relaxed),
            invariant_violations,
            invariants_ok,
        }
    }

    /// Graceful shutdown — every `Running` entry transitions to
    /// `Stopped { Shutdown }` with its BG webview closed. Call from the
    /// host's `RunEvent::ExitRequested` handler.
    pub async fn shutdown_all(&self) -> Result<()> {
        let ids: Vec<_> = self.entries.iter().map(|e| e.key().clone()).collect();
        for id in ids {
            if let Some(entry_arc) = self.entries.get(&id).map(|e| e.clone()) {
                let mut entry = entry_arc.lock().await;
                if entry.state.can_stop() {
                    let _ = self.internal_stop(&mut entry, StopReason::Shutdown).await;
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // Internals.
    // -------------------------------------------------------------------

    /// Parse a manifest and derive rules without touching shared state.
    async fn parse(
        &self,
        path: &Path,
    ) -> Result<(Arc<Manifest>, Vec<ContentScriptRule>, Option<String>, PathBuf)> {
        if !path.is_dir() {
            return Err(Error::ExtensionDirectory {
                path: path.to_path_buf(),
                reason: "not a directory".into(),
            });
        }
        let source_dir = canonicalize(path)?;
        let manifest_path = source_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(Error::ExtensionDirectory {
                path: source_dir.clone(),
                reason: "missing manifest.json".into(),
            });
        }

        let bytes = tokio::fs::read(&manifest_path).await?;
        let manifest = manifest::parse(&bytes)?;
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        let rules = compile_content_scripts(&raw)?;
        let bg_source = read_bg_source_raw(&raw, &source_dir).await?;

        Ok((Arc::new(manifest), rules, bg_source, source_dir))
    }

    /// Try to spawn a background webview if we have a backend AND the
    /// extension declared a service worker. `manifest` rides along to the
    /// BG shim's configure call so `chrome.runtime.getManifest()` works.
    /// Backend failures degrade to `None` — loader still reports success,
    /// just without a BG handle.
    async fn try_spawn(
        &self,
        id: &ExtensionId,
        bg_source: Option<&str>,
        manifest: &Manifest,
    ) -> Result<Option<BackgroundHandle>> {
        let Some(backend) = self.backend.as_ref() else {
            return Ok(None);
        };
        // `bg_source` is the gate: a missing / unreadable worker means no BG.
        // The source bytes themselves are no longer forwarded — the backend
        // loads the worker from the resource origin (D-008), which the
        // `ResourceRegistry` upsert in the caller makes serveable before this.
        if bg_source.is_none() {
            return Ok(None);
        }
        let manifest_json = serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null);
        match backend
            .spawn_background(id.clone(), manifest_json)
            .await
        {
            Ok(h) => Ok(Some(h)),
            Err(e) => {
                tracing::warn!(
                    extension = %id,
                    error = %e,
                    "backend.spawn_background failed; entry will run without BG"
                );
                Ok(None)
            }
        }
    }

    /// Make an extension's files serveable over the resource scheme (D-008)
    /// *before* its background webview is spawned. The hidden BG webview fetches
    /// its own service worker (and the worker's `import`/`importScripts` chunks)
    /// from `extres://<id>/...` during boot — earlier than the async
    /// [`ExtensionRegistry`] projection (`reproject`) lands — so the synchronous
    /// URI-scheme handler must be able to resolve the root immediately. This
    /// upsert is the synchronous channel for that. Idempotent.
    fn upsert_resource_root(&self, id: &ExtensionId, source_dir: &Path, manifest: &Manifest) {
        let Some(rr) = self
            .app
            .try_state::<crate::runtime::resources::ResourceRegistry>()
        else {
            return;
        };
        let war_globs: Vec<String> = manifest
            .web_accessible_resources
            .iter()
            .flat_map(|w| w.resources.clone())
            .collect();
        rr.upsert(id.clone(), source_dir.to_path_buf(), war_globs);
    }

    async fn internal_stop(
        &self,
        entry: &mut LifecycleEntry,
        reason: StopReason,
    ) -> Result<()> {
        // `begin_stopping` flips the state to `Stopping` AND hands back the
        // running handle in one step — see its docs for the dropped-handle
        // bug this guards against. The entry stays `Stopping` across the
        // shutdown await (we hold the per-entry mutex throughout).
        if let Some(handle) = entry.state.begin_stopping() {
            // App exit runs `shutdown_all` under `block_on` on the main
            // thread (RunEvent::ExitRequested), where the event loop can't
            // pump the close — waiting for teardown there would stall exit
            // until the ceilings expire. Every other stop waits, so an
            // immediate respawn under the same label can't race the close.
            let result = if matches!(reason, StopReason::Shutdown) {
                handle.shutdown_no_wait().await
            } else {
                handle.shutdown().await
            };
            if let Err(e) = result {
                tracing::warn!(extension = %entry.id, error = %e, "BG shutdown errored");
            }
        }
        // Clear any dynamic content-script registrations the (now stopped) BG
        // worker made via chrome.scripting.registerContentScripts. The worker
        // re-registers them from scratch on its next boot (reload / enable), so
        // keeping the old ones would (a) collide on re-register — Chrome errors
        // on a duplicate registration id — and (b) strand a stale `source_dir`
        // if the extension's path changed. Mirrors what Chrome does when a
        // service worker is torn down for a non-persistent reload.
        if let Some(store) = self
            .app
            .try_state::<crate::runtime::dynamic_scripts::DynamicScriptStore>()
        {
            store.drop_extension(&entry.id);
        }
        entry.state = ExtensionState::Stopped {
            reason: reason.clone(),
            stopped_at: SystemTime::now(),
        };
        // Emit an explicit `Stopped` only for non-reload reasons. Reloads
        // emit a single `Reloaded` event at the end of the full transition.
        if !matches!(reason, StopReason::Reload) {
            self.emit(LifecycleEvent::Stopped {
                id: entry.id.clone(),
                reason,
            });
        }
        Ok(())
    }

    fn emit(&self, ev: LifecycleEvent) {
        if let Err(e) = self.app.emit(EVENT_NAME, ev) {
            tracing::warn!(error = %e, "lifecycle event emit failed");
        }
    }

    async fn persist(&self) -> Result<()> {
        let ids: Vec<_> = self.entries.iter().map(|e| e.key().clone()).collect();
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(entry_arc) = self.entries.get(&id).map(|e| e.clone()) {
                let entry = entry_arc.lock().await;
                rows.push(PersistedEntry::new(
                    entry.id.clone(),
                    entry.source_dir.clone(),
                    entry.enabled,
                    entry.manifest.version.clone(),
                ));
            }
        }
        self.store.save(&rows).await
    }

    /// Refresh the read-only [`ExtensionRegistry`] projection stored in Tauri
    /// state. Called after every state-changing transition so external
    /// consumers (host frontends, the content-script injection path) always
    /// observe a consistent snapshot without subscribing to events.
    async fn reproject(&self) {
        if let Some(registry) = self.app.try_state::<ExtensionRegistry>() {
            self.project_into_registry(&registry).await;
        }
    }

    /// Current on-disk root of an installed extension, if the manager knows it.
    ///
    /// The manager inserts the [`LifecycleEntry`] (with its `source_dir`) at the
    /// very start of `install_or_reload`, *before* the background worker is
    /// spawned, whereas the read-only [`ExtensionRegistry`] projection only
    /// catches up at the end of the transition. The dynamic content-script
    /// command (`chrome.scripting.registerContentScripts`) resolves a script's
    /// root through here as a fallback so a worker that registers during its
    /// boot — before the registry projection lands — still resolves correctly.
    pub async fn source_dir(&self, id: &ExtensionId) -> Option<PathBuf> {
        let entry_arc = self.entries.get(id).map(|e| e.clone())?;
        let entry = entry_arc.lock().await;
        Some(entry.source_dir.clone())
    }
}

// ---------------------------------------------------------------------------
// Invariant checker — pure logic, tested independently of Tauri.
// ---------------------------------------------------------------------------

/// Per-entry snapshot used by the invariant checker. Extracted from live
/// manager state via [`LifecycleManager::snapshot_facts`] so the pure rule
/// checker doesn't need to acquire any locks or touch the Tauri runtime.
///
/// Exposed via the `_test_hooks` module under `#[doc(hidden)]` so the
/// regression test suite can drive [`check_invariants`] directly without
/// needing a live `AppHandle` — the Win 11 26200 test harness can't load
/// a binary linked to the webview runtime. See `tests/lifecycle_regression.rs`.
#[derive(Debug, Clone)]
pub struct EntryFacts {
    /// Extension id of the entry.
    pub id: ExtensionId,
    /// Canonical source directory.
    pub source_dir: PathBuf,
    /// Name of the current state (`installed` / `running` / etc).
    pub state_name: &'static str,
    /// Whether the entry is `enabled`.
    pub enabled: bool,
    /// If `Running` with a `BackgroundHandle`, its window label.
    pub bg_label: Option<String>,
    /// Convenience flag — true when `state_name == "running"`.
    pub is_running: bool,
    /// Convenience flag — true when in the transient `Uninstalling` state.
    pub is_uninstalling: bool,
}

/// Pure-logic invariant checker. Takes a facts snapshot + the set of live
/// `ext-bg-*` labels Tauri currently knows about, and returns every rule
/// violation it finds. Does NOT check `no_uninstalling_persist` — that rule
/// needs the store, which lives on the manager; the manager folds its
/// result into the full [`LifecycleManager::invariants`] output.
///
/// Split out so the rule logic can be exercised by tests without linking
/// a full Tauri runtime (avoids the Win 11 26200 STATUS_ENTRYPOINT_NOT_FOUND
/// DLL-load issue that blocks any integration test touching `AppHandle`).
pub fn check_invariants(
    facts: &[EntryFacts],
    live_bg_windows: &HashSet<String>,
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();

    // unique_source_dir
    let mut by_src: HashMap<PathBuf, Vec<ExtensionId>> = HashMap::new();
    for f in facts {
        by_src
            .entry(f.source_dir.clone())
            .or_default()
            .push(f.id.clone());
    }
    for (path, ids) in &by_src {
        if ids.len() > 1 {
            let rendered = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            violations.push(InvariantViolation {
                rule: "unique_source_dir".into(),
                detail: format!(
                    "source_dir {} shared by {} entries: [{}]",
                    path.display(),
                    ids.len(),
                    rendered
                ),
            });
        }
    }

    // unique_bg_label
    let mut by_label: HashMap<String, Vec<ExtensionId>> = HashMap::new();
    for f in facts {
        if let Some(label) = &f.bg_label {
            by_label
                .entry(label.clone())
                .or_default()
                .push(f.id.clone());
        }
    }
    for (label, ids) in &by_label {
        if ids.len() > 1 {
            let rendered = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            violations.push(InvariantViolation {
                rule: "unique_bg_label".into(),
                detail: format!(
                    "bg window label {} claimed by {} entries: [{}]",
                    label,
                    ids.len(),
                    rendered
                ),
            });
        }
    }

    // state_consistency
    for f in facts {
        if let Some(label) = &f.bg_label {
            if !live_bg_windows.contains(label) {
                violations.push(InvariantViolation {
                    rule: "state_consistency".into(),
                    detail: format!(
                        "extension {} is Running with bg_handle label {}, but no such Tauri window exists",
                        f.id, label
                    ),
                });
            }
        }
    }

    // enabled_matches_state
    for f in facts {
        if f.enabled && f.state_name == "stopped" {
            violations.push(InvariantViolation {
                rule: "enabled_matches_state".into(),
                detail: format!(
                    "extension {} is enabled=true but state=stopped (transient expected during reload)",
                    f.id
                ),
            });
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Canonicalize a path and normalize trailing separators.
fn canonicalize(path: &Path) -> Result<PathBuf> {
    let canon = std::fs::canonicalize(path).map_err(|e| Error::ExtensionDirectory {
        path: path.to_path_buf(),
        reason: format!("canonicalize: {e}"),
    })?;
    Ok(canon)
}

/// Derive the stable extension id. Prefers the manifest's typed `key`
/// (Chromium-style 32-char a..p id, matching what Chrome itself assigns);
/// falls back to a SHA-256 of the canonical source dir.
///
/// Public so tests can pin the Chromium-faithful derivation against the
/// real checked-in manifests — the schema once grew a typed `key` field
/// while this function still read the raw-JSON `extra` map, and every
/// packaged extension silently fell back to a source-dir id.
pub fn derive_extension_id(manifest: &Manifest, source_dir: &Path) -> ExtensionId {
    manifest
        .key
        .as_deref()
        .and_then(ExtensionId::from_key)
        .unwrap_or_else(|| ExtensionId::from_source_dir(source_dir))
}

fn derive_id(manifest: &Manifest, source_dir: &Path) -> Result<ExtensionId> {
    Ok(derive_extension_id(manifest, source_dir))
}

/// Compile `content_scripts[]` into cached rules. Duplicated in the
/// lifecycle side because the legacy loader.rs implementation is kept
/// around for backwards compat — kept in lockstep via tests.
fn compile_content_scripts(raw: &serde_json::Value) -> Result<Vec<ContentScriptRule>> {
    let Some(arr) = raw.get("content_scripts").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let patterns: Vec<MatchPattern> = entry
            .get("matches")
            .and_then(|v| v.as_array())
            .map(|m| {
                m.iter()
                    .filter_map(|v| v.as_str())
                    .map(MatchPattern::parse)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let js_files: Vec<PathBuf> = entry
            .get("js")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        let run_at = match entry.get("run_at").and_then(|v| v.as_str()) {
            Some("document_start") => RunAt::DocumentStart,
            Some("document_end") => RunAt::DocumentEnd,
            Some("document_idle") | None => RunAt::DocumentIdle,
            Some(other) => {
                tracing::warn!(run_at = other, "unknown run_at; defaulting to document_idle");
                RunAt::DocumentIdle
            }
        };
        let world = match entry
            .get("world")
            .and_then(|v| v.as_str())
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("MAIN") => World::Main,
            _ => World::Isolated,
        };
        out.push(ContentScriptRule {
            matches: MatchPatternSet::new(patterns),
            js_files,
            run_at,
            world,
        });
    }
    Ok(out)
}

/// Read the `background.service_worker` source from disk, best-effort.
async fn read_bg_source_raw(
    raw: &serde_json::Value,
    root: &Path,
) -> Result<Option<String>> {
    let Some(rel) = raw
        .get("background")
        .and_then(|v| v.get("service_worker"))
        .and_then(|v| v.as_str())
    else {
        return Ok(None);
    };
    let full = root.join(rel);
    match tokio::fs::read_to_string(&full).await {
        Ok(s) => Ok(Some(s)),
        Err(e) => {
            tracing::warn!(
                file = %full.display(),
                error = %e,
                "background.service_worker unreadable; skipping BG spawn"
            );
            Ok(None)
        }
    }
}

/// Read the BG source given a parsed manifest (used by `enable`).
async fn read_bg_source(root: &Path, _manifest: &Manifest) -> Result<Option<String>> {
    // Re-read the raw manifest to get `background.service_worker` — Agent
    // A's schema doesn't yet type it, so we re-parse the on-disk JSON.
    let manifest_path = root.join("manifest.json");
    let bytes = tokio::fs::read(&manifest_path).await?;
    let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
    read_bg_source_raw(&raw, root).await
}
