//! In-memory `chrome.storage.session` backend.
//!
//! One [`SessionStorage`] instance per extension, lazily created by
//! [`SessionStorageManager`] on first access. Dropping the manager (on plugin
//! teardown) clears everything.

use dashmap::DashMap;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

use crate::registry::ExtensionId;

/// Per-extension in-memory kv. `chrome.storage.session` semantics: cleared on
/// extension unload, no disk backing.
#[derive(Debug, Default)]
pub struct SessionStorage {
    inner: DashMap<String, Value>,
}

impl SessionStorage {
    /// Fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a single key.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.inner.get(key).map(|v| v.clone())
    }

    /// Get a batch of keys. Unknown keys are omitted from the result map —
    /// this mirrors `chrome.storage.local.get(["a", "b"])`, which returns only
    /// the keys actually present.
    pub fn get_many(&self, keys: Option<&[String]>) -> HashMap<String, Value> {
        match keys {
            Some(keys) => keys
                .iter()
                .filter_map(|k| self.get(k).map(|v| (k.clone(), v)))
                .collect(),
            None => self
                .inner
                .iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect(),
        }
    }

    /// Set a single key.
    pub fn set(&self, key: String, value: Value) {
        self.inner.insert(key, value);
    }

    /// Set a batch of keys.
    pub fn set_many(&self, entries: HashMap<String, Value>) {
        for (k, v) in entries {
            self.inner.insert(k, v);
        }
    }

    /// Remove a key, returning the previous value if any.
    pub fn remove(&self, key: &str) -> Option<Value> {
        self.inner.remove(key).map(|(_, v)| v)
    }

    /// Remove a batch of keys.
    pub fn remove_many(&self, keys: &[String]) {
        for k in keys {
            self.inner.remove(k);
        }
    }

    /// Drop every key.
    pub fn clear(&self) {
        self.inner.clear();
    }
}

/// Lazily-created per-extension [`SessionStorage`] map. One instance held in
/// Tauri state.
#[derive(Debug, Default)]
pub struct SessionStorageManager {
    stores: DashMap<ExtensionId, Arc<SessionStorage>>,
}

impl SessionStorageManager {
    /// Fresh manager with no stores.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the session store for an extension.
    pub fn for_extension(&self, id: &ExtensionId) -> Arc<SessionStorage> {
        if let Some(s) = self.stores.get(id) {
            return s.clone();
        }
        let store = Arc::new(SessionStorage::new());
        self.stores.insert(id.clone(), store.clone());
        store
    }

    /// Drop the session store for an extension (called on unload).
    pub fn drop_extension(&self, id: &ExtensionId) {
        self.stores.remove(id);
    }
}

// Tests live in `tests/c_bus_storage.rs` — see note in `registry.rs`.
