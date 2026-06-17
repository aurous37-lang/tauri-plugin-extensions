//! In-memory registry of loaded extensions.
//!
//! Owned by Agent C. Keyed by [`ExtensionId`]; each entry holds the parsed
//! manifest, compiled content-script match rules, the hidden background-webview
//! handle (when one was spawned), and any open chrome.runtime ports.

use base64::Engine as _;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    matcher::MatchPatternSet,
    runtime::{background::BackgroundHandle, InjectionRequest, RunAt, World},
};

/// Stable identifier for a loaded extension. Derived from the manifest's
/// public `key` when present (Chrome-style 32-char `a..p` string), otherwise
/// a random UUID (matches how Chrome handles unpacked extensions without a
/// key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtensionId(String);

impl ExtensionId {
    /// Construct from a raw string. No validation — use [`ExtensionId::from_key`]
    /// or [`ExtensionId::random`] for vetted paths.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Derive the 32-char Chrome-style id from a manifest `key` (base64 DER of
    /// the extension's public key). Returns `None` if the key fails to decode.
    ///
    /// Algorithm (mirrors Chromium's `crx_file::id_util`):
    ///
    /// 1. base64-decode the key into its DER bytes.
    /// 2. SHA-256 the DER bytes.
    /// 3. Take the first 16 bytes of the digest.
    /// 4. For each of the 32 nibbles, map `0..=15` to `a..=p`.
    pub fn from_key(key: &str) -> Option<Self> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(key.trim())
            .ok()?;
        let digest = Sha256::digest(&der);
        let mut out = String::with_capacity(32);
        for byte in &digest[..16] {
            out.push(nibble_to_letter(byte >> 4));
            out.push(nibble_to_letter(byte & 0x0f));
        }
        Some(Self(out))
    }

    /// Fresh random id — used only by legacy paths that predate the
    /// lifecycle manager. New code should prefer [`ExtensionId::from_key`]
    /// or [`ExtensionId::from_source_dir`] so the id is stable across
    /// process restarts.
    pub fn random() -> Self {
        Self(format!("local-{}", Uuid::new_v4().simple()))
    }

    /// Derive a stable id from the canonical absolute source-directory
    /// path. Same directory → same id, across restarts, across machines
    /// with the same mount structure.
    ///
    /// Algorithm: case-fold the path on Windows, strip trailing separators,
    /// SHA-256, take the first 16 bytes, map each nibble to `a..=p` —
    /// the same mapping Chromium uses for key-derived ids, so ids
    /// produced here are lexically distinguishable from web-store ids
    /// only by the `unpacked-` prefix.
    pub fn from_source_dir(path: &std::path::Path) -> Self {
        let mut s = path.to_string_lossy().into_owned();
        if cfg!(target_os = "windows") {
            s = s.to_lowercase();
        }
        s = s.trim_end_matches(['/', '\\']).to_string();
        let digest = Sha256::digest(s.as_bytes());
        let mut suffix = String::with_capacity(32);
        for byte in &digest[..16] {
            suffix.push(nibble_to_letter(byte >> 4));
            suffix.push(nibble_to_letter(byte & 0x0f));
        }
        Self(format!("unpacked-{}", suffix))
    }

    /// Borrow the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn nibble_to_letter(nibble: u8) -> char {
    debug_assert!(nibble < 16);
    (b'a' + nibble) as char
}

/// A compiled content-script rule — extracted from the manifest's
/// `content_scripts[]` entries and cached on the registry so matching on
/// navigation is a hot-path walk.
#[derive(Debug, Clone)]
pub struct ContentScriptRule {
    /// Set of match patterns. Script fires when any pattern matches.
    pub matches: MatchPatternSet,
    /// Paths (relative to the extension root) of JS files to inject.
    pub js_files: Vec<std::path::PathBuf>,
    /// Injection phase.
    pub run_at: RunAt,
    /// JS world to inject into.
    pub world: World,
}

/// A single loaded extension.
#[derive(Debug)]
pub struct LoadedExtension {
    /// Stable extension identity.
    pub id: ExtensionId,
    /// On-disk path of the unpacked extension directory.
    pub source_dir: std::path::PathBuf,
    /// Parsed manifest (shape owned by Agent A).
    pub manifest: Arc<crate::manifest::Manifest>,
    /// Compiled content-script rules from `content_scripts[]`.
    pub content_scripts: Vec<ContentScriptRule>,
    /// Hidden background webview handle — `None` when no background worker is
    /// declared or when the platform backend refused to spawn one (e.g. the
    /// default stub backend on every platform during the spike).
    pub background_handle: Mutex<Option<BackgroundHandle>>,
}

/// Compact descriptor returned by `extensions_list` — the JS side doesn't need
/// the full `Manifest` every time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionSummary {
    /// Stable extension id.
    pub id: ExtensionId,
    /// Human-readable name from the manifest.
    pub name: String,
    /// Extension version string.
    pub version: String,
    /// Optional description. `None` when the manifest does not declare one.
    pub description: Option<String>,
}

/// In-memory registry of loaded extensions. Stored in Tauri state via
/// `tauri::Manager::manage(..)` at plugin init.
#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    inner: DashMap<ExtensionId, Arc<LoadedExtension>>,
}

impl ExtensionRegistry {
    /// Fresh, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a newly loaded extension. Returns the previously registered
    /// entry if the id collided.
    pub fn insert(&self, ext: LoadedExtension) -> Option<Arc<LoadedExtension>> {
        let id = ext.id.clone();
        self.inner.insert(id, Arc::new(ext))
    }

    /// Look up an extension by id.
    pub fn get(&self, id: &ExtensionId) -> Option<Arc<LoadedExtension>> {
        self.inner.get(id).map(|entry| entry.clone())
    }

    /// List every loaded extension's id.
    pub fn ids(&self) -> Vec<ExtensionId> {
        self.inner.iter().map(|entry| entry.key().clone()).collect()
    }

    /// List every loaded extension as a compact summary.
    pub fn list(&self) -> Vec<ExtensionSummary> {
        self.inner
            .iter()
            .map(|entry| {
                let ext = entry.value();
                ExtensionSummary {
                    id: ext.id.clone(),
                    name: ext.manifest.name.clone(),
                    // Agent A's schema has `version: Option<String>` and
                    // `description: Option<String>` — pass through directly.
                    version: ext.manifest.version.clone().unwrap_or_default(),
                    description: ext.manifest.description.clone(),
                }
            })
            .collect()
    }

    /// Remove an extension. Returns the entry if it existed.
    pub fn remove(&self, id: &ExtensionId) -> Option<Arc<LoadedExtension>> {
        self.inner.remove(id).map(|(_, v)| v)
    }

    /// Walk every loaded extension and produce one injection request per
    /// content-script JS file that matches the given URL. The backend
    /// consumes the returned vector to drive `evaluate_script` calls at the
    /// correct lifecycle phase.
    pub fn content_scripts_for_url(&self, url: &url::Url) -> Vec<InjectionRequest> {
        let mut requests = Vec::new();
        for entry in self.inner.iter() {
            let ext = entry.value();
            for rule in &ext.content_scripts {
                if !rule.matches.matches(url) {
                    continue;
                }
                for js_rel in &rule.js_files {
                    // Best-effort read: if a referenced file is missing we
                    // log at `warn` and skip — the rest of the extension
                    // still loads.
                    let full = ext.source_dir.join(js_rel);
                    let source = match std::fs::read_to_string(&full) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                extension = %ext.id,
                                file = %full.display(),
                                error = %e,
                                "skipping unreadable content-script file",
                            );
                            continue;
                        }
                    };
                    requests.push(InjectionRequest {
                        extension: ext.id.clone(),
                        source,
                        run_at: rule.run_at,
                        world: rule.world,
                    });
                }
            }
        }
        requests
    }
}

// Inline tests intentionally live in `tests/c_bus_storage.rs` instead of a
// `#[cfg(test)] mod tests` here. The lib-test binary on Windows 11 26200
// fails to load before `main` (STATUS_ENTRYPOINT_NOT_FOUND) because it links
// the full Tauri runtime; integration-test binaries link only what the test
// source references, so the ExtensionId / bus / storage tests run cleanly
// from there.
