//! Thin shim over [`crate::lifecycle::LifecycleManager`].
//!
//! The original loader owned manifest parsing, id derivation, and
//! `Backend::spawn_background` directly. That design had no concept of
//! identity across time: every call minted a fresh `ExtensionId`, spawned
//! a fresh hidden webview, and left the previous one running. The
//! result was a memory-hog leak (92 zombie `ext-bg-*` windows observed in
//! the spike-acceptance run on 2026-04-20).
//!
//! The lifecycle manager subsumes all of that. Loader is kept as a
//! narrow public entry point — `pub async fn load_unpacked` — so callers
//! that already depend on its signature don't break.

use std::path::Path;

use tauri::Manager;

use crate::{lifecycle::LifecycleManager, registry::ExtensionId, Result};

/// Install-or-reload an unpacked MV3 extension directory. Idempotent by
/// canonical source path — calling against the same directory twice
/// reloads the existing entry instead of duplicating it.
///
/// Delegates to [`LifecycleManager::install_or_reload`].
pub async fn load_unpacked<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &Path,
) -> Result<ExtensionId> {
    let manager = app.state::<std::sync::Arc<LifecycleManager<R>>>();
    manager.install_or_reload(path).await
}
