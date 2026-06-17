//! Persistent store for installed-extension metadata.
//!
//! The store is the plugin's durable record of *which* extensions are
//! installed and *where* their source directories live. It does **not**
//! persist runtime state (Running/Stopped/…) — on boot, every installed
//! extension that was last-known-enabled starts fresh.
//!
//! The default implementation ([`FsStateStore`]) writes JSON atomically
//! (`.tmp` + rename) to `<app_data_dir>/extensions/state.json`. The trait
//! is exposed so tests can swap in an in-memory fake and so future consumers
//! can redirect persistence elsewhere (keychain-backed, network, etc.).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{registry::ExtensionId, Error, Result};

/// Current on-disk schema version for the persisted state file.
///
/// The persisted document is an object shaped like:
///
/// ```json
/// { "schema_version": 1, "entries": [ ... ] }
/// ```
///
/// Legacy state files written before schema versioning are bare JSON arrays
/// (`[ ... ]`); [`FsStateStore::load`] accepts either shape and treats a bare
/// array as schema version 0 (pre-versioning). A persisted document whose
/// `schema_version` exceeds [`STATE_SCHEMA_VERSION`] is rejected with
/// [`Error::Storage`] so a newer-build state file cannot silently corrupt an
/// older binary.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// On-disk wrapper: `{ schema_version, entries }`. Used for serialization
/// only — the public trait surface keeps the plain `Vec<PersistedEntry>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateDocument {
    /// Monotonic schema version — see [`STATE_SCHEMA_VERSION`].
    schema_version: u32,
    /// Installed-extension inventory.
    entries: Vec<PersistedEntry>,
}

/// One row in the persistent extension inventory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedEntry {
    /// Stable extension id. Derived from the manifest `key` or from
    /// [`ExtensionId::from_source_dir`] — never random for persisted rows.
    pub id: ExtensionId,
    /// Canonical absolute path of the unpacked extension directory.
    pub source_dir: PathBuf,
    /// Whether the extension should auto-start on next boot.
    pub enabled: bool,
    /// When the extension was first installed (UNIX seconds).
    pub installed_at_unix: u64,
    /// Manifest version string at last successful load, for drift diagnostics.
    pub last_loaded_version: Option<String>,
}

impl PersistedEntry {
    /// Build with `installed_at` set to the current wall-clock time.
    pub fn new(
        id: ExtensionId,
        source_dir: PathBuf,
        enabled: bool,
        last_loaded_version: Option<String>,
    ) -> Self {
        Self {
            id,
            source_dir,
            enabled,
            installed_at_unix: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            last_loaded_version,
        }
    }
}

/// Persistence backend for the lifecycle manager.
#[async_trait]
pub trait StateStore: Send + Sync + std::fmt::Debug {
    /// Load every persisted row. Returns an empty vec if the store is
    /// fresh (file missing is not an error).
    async fn load(&self) -> Result<Vec<PersistedEntry>>;

    /// Overwrite the store with the given vec. Must be atomic — partial
    /// writes that leave a corrupt file are not acceptable.
    async fn save(&self, entries: &[PersistedEntry]) -> Result<()>;
}

/// Filesystem-backed state store. Writes atomically via the
/// `write-to-.tmp + fsync + rename` pattern.
///
/// Path defaults to `<app_data_dir>/extensions/state.json`; construct with
/// [`FsStateStore::at`] for an explicit location.
#[derive(Debug)]
pub struct FsStateStore {
    path: PathBuf,
}

impl FsStateStore {
    /// Construct bound to an explicit path. The parent directory is created
    /// on first save if missing; the file is not required to exist on load.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Convenience: rooted at `<base>/extensions/state.json`.
    pub fn under_app_data(base: &Path) -> Self {
        Self::at(base.join("extensions").join("state.json"))
    }
}

/// Parse the on-disk bytes. Accepts either:
/// - `{ "schema_version": N, "entries": [...] }` — versioned shape; rejects
///   `N > STATE_SCHEMA_VERSION`
/// - `[...]` — bare-array legacy shape; treated as schema version 0
fn decode_state_bytes(bytes: &[u8]) -> Result<Vec<PersistedEntry>> {
    // Parse once as untyped JSON so we can branch on the shape without
    // double-deserializing in the hot path. Empty or whitespace-only files
    // surface as Storage errors, not silent empties.
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    match value {
        serde_json::Value::Array(_) => {
            // Pre-versioning bare array — migration path from schema 0.
            let entries: Vec<PersistedEntry> = serde_json::from_value(value)?;
            Ok(entries)
        }
        serde_json::Value::Object(_) => {
            let doc: StateDocument = serde_json::from_value(value).map_err(|e| {
                Error::Storage(format!("state.json is not a recognized schema: {e}"))
            })?;
            if doc.schema_version > STATE_SCHEMA_VERSION {
                return Err(Error::Storage(format!(
                    "state.json schema_version {} is newer than this build supports ({})",
                    doc.schema_version, STATE_SCHEMA_VERSION
                )));
            }
            Ok(doc.entries)
        }
        other => Err(Error::Storage(format!(
            "state.json top-level must be object or array, got {}",
            match other {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::String(_) => "string",
                _ => "unexpected",
            }
        ))),
    }
}

#[async_trait]
impl StateStore for FsStateStore {
    async fn load(&self) -> Result<Vec<PersistedEntry>> {
        let path = self.path.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<PersistedEntry>> {
            match std::fs::read(&path) {
                Ok(bytes) => decode_state_bytes(&bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(Error::Io(e)),
            }
        })
        .await
        .map_err(|e| Error::Storage(format!("state-store load join: {e}")))?;
        result
    }

    async fn save(&self, entries: &[PersistedEntry]) -> Result<()> {
        let path = self.path.clone();
        let doc = StateDocument {
            schema_version: STATE_SCHEMA_VERSION,
            entries: entries.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&doc)?;
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(Error::Io)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, &bytes).map_err(Error::Io)?;
            // Best-effort fsync on the tmp file — not all filesystems
            // honour it, but when they do it's the difference between
            // "clean restart" and "corrupt json on power loss."
            if let Ok(f) = std::fs::File::open(&tmp) {
                let _ = f.sync_all();
            }
            std::fs::rename(&tmp, &path).map_err(Error::Io)?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Storage(format!("state-store save join: {e}")))??;
        Ok(())
    }
}

/// In-memory test double. Keeps the entries in a `Mutex<Vec<_>>`.
#[derive(Debug, Default, Clone)]
pub struct MemoryStateStore {
    inner: Arc<Mutex<Vec<PersistedEntry>>>,
}

impl MemoryStateStore {
    /// Fresh empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StateStore for MemoryStateStore {
    async fn load(&self) -> Result<Vec<PersistedEntry>> {
        Ok(self.inner.lock().clone())
    }

    async fn save(&self, entries: &[PersistedEntry]) -> Result<()> {
        *self.inner.lock() = entries.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn fs_store_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FsStateStore::under_app_data(dir.path());
        assert!(store.load().await.unwrap().is_empty());

        let entries = vec![PersistedEntry::new(
            ExtensionId::new("unpacked-abc"),
            PathBuf::from("/tmp/ext"),
            true,
            Some("0.1.0".to_string()),
        )];
        store.save(&entries).await.unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded, entries);

        // Atomic rewrite: overwriting with a shorter set must not leave the
        // larger set on disk.
        store.save(&[]).await.unwrap();
        assert!(store.load().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_store_roundtrip() {
        let store = MemoryStateStore::new();
        assert!(store.load().await.unwrap().is_empty());
        let entries = vec![PersistedEntry::new(
            ExtensionId::new("unpacked-xyz"),
            PathBuf::from("/tmp/ext2"),
            false,
            None,
        )];
        store.save(&entries).await.unwrap();
        assert_eq!(store.load().await.unwrap(), entries);
    }

    /// Pre-v1 on-disk format was a bare JSON array. A binary shipping
    /// schema v1 must still load those files cleanly.
    #[tokio::test]
    async fn fs_store_reads_legacy_bare_array() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("extensions").join("state.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Hand-crafted legacy bytes — whatever `serde_json::to_vec_pretty`
        // would have produced for a `Vec<PersistedEntry>`.
        let legacy_entry = PersistedEntry::new(
            ExtensionId::new("unpacked-legacy"),
            PathBuf::from("/tmp/legacy"),
            true,
            Some("0.0.9".to_string()),
        );
        let raw = serde_json::to_vec_pretty(&vec![legacy_entry.clone()]).unwrap();
        std::fs::write(&path, raw).unwrap();

        let store = FsStateStore::at(path.clone());
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded, vec![legacy_entry]);

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
    async fn fs_store_reads_current_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let entries = vec![PersistedEntry::new(
            ExtensionId::new("unpacked-current"),
            PathBuf::from("/tmp/current"),
            true,
            None,
        )];
        let doc = serde_json::json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "entries": entries.clone()
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

        let store = FsStateStore::at(path);
        assert_eq!(store.load().await.unwrap(), entries);
    }
}
