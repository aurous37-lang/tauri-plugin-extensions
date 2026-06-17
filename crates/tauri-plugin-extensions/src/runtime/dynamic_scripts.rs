//! Dynamic content-script registry backing `chrome.scripting.registerContentScripts`.
//!
//! MV3 wallets register some of their inpage scripts at runtime from the
//! background service worker instead of declaring them statically in
//! `manifest.json`. Phantom is the canonical case: its EVM provider
//! (`evmAsk.js` / `evmPhantom.js` / `evmMetamask.js`, all `world: "MAIN"`,
//! `runAt: "document_start"`) is never in `content_scripts[]` — the service
//! worker calls
//! `chrome.scripting.registerContentScripts([{ id: "conditionalInpageScripts",
//! js: [<the chosen evm bundle>], world: "MAIN", matches: [...] }])` on init.
//! Rabby registers scripts the same way.
//!
//! This store is the runtime home for those registrations. The on_page_load
//! injection flow ([`crate::runtime::injection::handle_page_load`]) merges the
//! [`InjectionRequest`]s this store produces with the manifest-declared ones,
//! so a dynamically-registered MAIN-world script reaches the page exactly like
//! a statically-declared one.
//!
//! ## Why `source_dir` is captured at registration time
//!
//! Registration resolves the calling extension's on-disk root once (from the
//! [`crate::registry::ExtensionRegistry`]) and stores it on the
//! [`RegisteredScript`]. Injection then reads each `js` file from
//! `source_dir.join(file)` without consulting the registry again — keeping the
//! hot on_page_load path free of a registry round-trip and making the store
//! unit-testable with only a temp dir.

use std::path::PathBuf;

use dashmap::DashMap;

use crate::{
    matcher::MatchPatternSet,
    registry::ExtensionId,
    runtime::{InjectionRequest, RunAt, World},
    Error, Result,
};

/// One dynamically-registered content script, mirroring the subset of Chrome's
/// `RegisteredContentScript` the runtime honors.
#[derive(Debug, Clone)]
pub struct RegisteredScript {
    /// Caller-chosen registration id, unique per extension (Chrome contract).
    pub id: String,
    /// Absolute on-disk root of the registering extension, resolved once at
    /// registration time. `js` paths are relative to this.
    pub source_dir: PathBuf,
    /// Compiled match patterns. The script injects when any pattern matches.
    pub matches: MatchPatternSet,
    /// Raw match-pattern strings, retained so `getRegisteredContentScripts`
    /// round-trips the exact values the caller passed.
    pub match_strings: Vec<String>,
    /// JS files (relative to `source_dir`) to inject, in order.
    pub js_files: Vec<PathBuf>,
    /// Injection phase.
    pub run_at: RunAt,
    /// Target world. Wallet EVM providers use [`World::Main`].
    pub world: World,
    /// Inject into child frames too (parsed; the single-frame eval path does
    /// not yet honor it, matching the static content-script limitation).
    pub all_frames: bool,
    /// Whether Chrome would persist this across browser sessions. Retained for
    /// `getRegisteredContentScripts` fidelity; the runtime re-registers from
    /// the BG worker on every boot, so persistence is informational here.
    pub persist_across_sessions: bool,
}

/// Per-extension set of dynamically-registered content scripts.
#[derive(Debug, Default)]
pub struct DynamicScriptStore {
    inner: DashMap<ExtensionId, Vec<RegisteredScript>>,
}

impl DynamicScriptStore {
    /// Fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one or more scripts for an extension.
    ///
    /// Mirrors Chrome's contract: every registration id must be unique within
    /// the extension. If any incoming id collides with an existing one — or
    /// with another id in the same batch — the whole call is rejected and no
    /// scripts are added (all-or-nothing).
    pub fn register(&self, ext: &ExtensionId, scripts: Vec<RegisteredScript>) -> Result<()> {
        // Reject duplicate ids within the incoming batch first.
        for (i, s) in scripts.iter().enumerate() {
            if scripts[..i].iter().any(|o| o.id == s.id) {
                return Err(Error::Runtime(format!(
                    "duplicate content script id in request: {}",
                    s.id
                )));
            }
        }
        let mut entry = self.inner.entry(ext.clone()).or_default();
        for s in &scripts {
            if entry.iter().any(|existing| existing.id == s.id) {
                return Err(Error::Runtime(format!(
                    "content script id '{}' is already registered",
                    s.id
                )));
            }
        }
        entry.extend(scripts);
        Ok(())
    }

    /// Unregister scripts. `Some(ids)` removes exactly those; `None` clears
    /// every dynamic script for the extension (Chrome's
    /// `unregisterContentScripts()` with no filter).
    pub fn unregister(&self, ext: &ExtensionId, ids: Option<&[String]>) {
        if let Some(mut entry) = self.inner.get_mut(ext) {
            match ids {
                Some(ids) => entry.retain(|s| !ids.iter().any(|want| want == &s.id)),
                None => entry.clear(),
            }
        }
    }

    /// Snapshot the registered scripts for an extension, optionally filtered to
    /// a set of ids (Chrome's `getRegisteredContentScripts({ ids })`).
    pub fn get_registered(&self, ext: &ExtensionId, ids: Option<&[String]>) -> Vec<RegisteredScript> {
        self.inner
            .get(ext)
            .map(|entry| {
                entry
                    .iter()
                    .filter(|s| match ids {
                        Some(ids) => ids.iter().any(|want| want == &s.id),
                        None => true,
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drop every dynamic registration for an extension — called on unload /
    /// reload so a reinstalled extension starts from a clean slate (its BG
    /// worker re-registers on boot).
    pub fn drop_extension(&self, ext: &ExtensionId) {
        self.inner.remove(ext);
    }

    /// Produce one [`InjectionRequest`] per matching dynamic script file for a
    /// URL, across every extension. Mirrors
    /// [`crate::registry::ExtensionRegistry::content_scripts_for_url`] but
    /// sourced from dynamic registrations. Unreadable files are logged and
    /// skipped so one bad path doesn't strand the rest.
    pub fn requests_for_url(&self, url: &url::Url) -> Vec<InjectionRequest> {
        let mut out = Vec::new();
        for entry in self.inner.iter() {
            let ext = entry.key();
            for script in entry.value() {
                if !script.matches.matches(url) {
                    continue;
                }
                for js_rel in &script.js_files {
                    let full = script.source_dir.join(js_rel);
                    match std::fs::read_to_string(&full) {
                        Ok(source) => out.push(InjectionRequest {
                            extension: ext.clone(),
                            source,
                            run_at: script.run_at,
                            world: script.world,
                        }),
                        Err(e) => tracing::warn!(
                            extension = %ext,
                            file = %full.display(),
                            error = %e,
                            "skipping unreadable dynamic content-script file",
                        ),
                    }
                }
            }
        }
        out
    }
}
