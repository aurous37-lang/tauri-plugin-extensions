//! Disk-backed `chrome.storage.local` backend.
//!
//! One JSON file per extension at
//! `<root>/<ext_id>/storage-local.json`. Writes are serialized behind a
//! `tokio::sync::Mutex`; each write goes to a sibling `.tmp` file and is then
//! `rename`d over the real file so readers never see a torn JSON blob.
//!
//! The entire blob is loaded into an in-memory [`HashMap`] on first access
//! and mutated there; the mutex guard wraps both the in-memory copy and the
//! disk file so the two stay consistent.

use dashmap::DashMap;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

use crate::{registry::ExtensionId, Error, Result};

/// Per-extension disk-backed kv.
#[derive(Debug)]
pub struct LocalStorage {
    path: PathBuf,
    inner: Mutex<Option<HashMap<String, Value>>>,
}

impl LocalStorage {
    /// Build a store anchored at `path`. The file does not need to exist yet
    /// — it's created on the first write.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            inner: Mutex::new(None),
        }
    }

    async fn load(&self) -> Result<HashMap<String, Value>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let bytes = fs::read(&self.path)
            .await
            .map_err(|e| Error::Storage(format!("read {}: {e}", self.path.display())))?;
        if bytes.is_empty() {
            return Ok(HashMap::new());
        }
        let map: HashMap<String, Value> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("parse {}: {e}", self.path.display())))?;
        Ok(map)
    }

    async fn ensure_loaded<'a>(
        &'a self,
        guard: &mut tokio::sync::MutexGuard<'a, Option<HashMap<String, Value>>>,
    ) -> Result<()> {
        if guard.is_none() {
            **guard = Some(self.load().await?);
        }
        Ok(())
    }

    async fn persist(&self, map: &HashMap<String, Value>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::Storage(format!("mkdir {}: {e}", parent.display())))?;
        }
        let bytes = serde_json::to_vec(map)
            .map_err(|e| Error::Storage(format!("encode: {e}")))?;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)
                .await
                .map_err(|e| Error::Storage(format!("create {}: {e}", tmp.display())))?;
            f.write_all(&bytes)
                .await
                .map_err(|e| Error::Storage(format!("write {}: {e}", tmp.display())))?;
            f.sync_all()
                .await
                .map_err(|e| Error::Storage(format!("fsync {}: {e}", tmp.display())))?;
        }
        fs::rename(&tmp, &self.path).await.map_err(|e| {
            Error::Storage(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })?;
        Ok(())
    }

    /// Get a batch. `None` returns the entire store; `Some(keys)` returns only
    /// the keys that exist.
    pub async fn get_many(&self, keys: Option<&[String]>) -> Result<HashMap<String, Value>> {
        let mut guard = self.inner.lock().await;
        self.ensure_loaded(&mut guard).await?;
        let map = guard.as_ref().expect("just loaded");
        let out = match keys {
            Some(keys) => keys
                .iter()
                .filter_map(|k| map.get(k).map(|v| (k.clone(), v.clone())))
                .collect(),
            None => map.clone(),
        };
        Ok(out)
    }

    /// Set a batch. Overwrites existing keys.
    pub async fn set_many(&self, entries: HashMap<String, Value>) -> Result<()> {
        let mut guard = self.inner.lock().await;
        self.ensure_loaded(&mut guard).await?;
        {
            let map = guard.as_mut().expect("just loaded");
            for (k, v) in entries {
                map.insert(k, v);
            }
        }
        let snapshot = guard.as_ref().expect("still loaded").clone();
        self.persist(&snapshot).await
    }

    /// Remove a batch of keys.
    pub async fn remove_many(&self, keys: &[String]) -> Result<()> {
        let mut guard = self.inner.lock().await;
        self.ensure_loaded(&mut guard).await?;
        {
            let map = guard.as_mut().expect("just loaded");
            for k in keys {
                map.remove(k);
            }
        }
        let snapshot = guard.as_ref().expect("still loaded").clone();
        self.persist(&snapshot).await
    }

    /// Drop every key.
    pub async fn clear(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        *guard = Some(HashMap::new());
        let snapshot = guard.as_ref().unwrap().clone();
        self.persist(&snapshot).await
    }
}

/// Lazily-created per-extension [`LocalStorage`] map. One instance held in
/// Tauri state; anchors every extension's JSON file under a shared root (the
/// app data dir's `extensions/` subfolder).
#[derive(Debug)]
pub struct LocalStorageManager {
    root: PathBuf,
    stores: DashMap<ExtensionId, Arc<LocalStorage>>,
}

impl LocalStorageManager {
    /// Build a manager rooted at `<root>/extensions/`. The path does not need
    /// to exist yet.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().join("extensions"),
            stores: DashMap::new(),
        }
    }

    fn path_for(&self, id: &ExtensionId) -> PathBuf {
        self.root.join(id.as_str()).join("storage-local.json")
    }

    /// Get or create the on-disk store for the given extension.
    pub fn for_extension(&self, id: &ExtensionId) -> Arc<LocalStorage> {
        if let Some(s) = self.stores.get(id) {
            return s.clone();
        }
        let store = Arc::new(LocalStorage::new(self.path_for(id)));
        self.stores.insert(id.clone(), store.clone());
        store
    }

    /// Remove the in-memory handle for the given extension. Does not delete
    /// the on-disk JSON — that's preserved across reloads deliberately.
    pub fn drop_extension(&self, id: &ExtensionId) {
        self.stores.remove(id);
    }
}

// Tests live in `tests/c_bus_storage.rs` — see note in `registry.rs`.
