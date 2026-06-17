//! Crate-wide error type. Every fallible path in the plugin surfaces one of
//! these variants; higher layers either propagate or serialize to JSON for
//! the JS side.

use std::path::PathBuf;

/// Crate-wide Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error for the extension plugin. One variant per failure
/// category; add specificity inline rather than introducing new variants for
/// each individual call site.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Manifest was malformed or referenced MV2.
    #[error("manifest error: {0}")]
    Manifest(String),

    /// The extension directory did not contain the expected files (most
    /// commonly a missing `manifest.json`).
    #[error("extension directory invalid at {path}: {reason}")]
    ExtensionDirectory {
        /// Path we attempted to load.
        path: PathBuf,
        /// Human-readable reason.
        reason: String,
    },

    /// URL / glob pattern compilation failed.
    #[error("invalid match pattern '{pattern}': {reason}")]
    MatchPattern {
        /// Offending pattern string.
        pattern: String,
        /// Compiler message.
        reason: String,
    },

    /// The runtime backend refused the operation — usually because the
    /// current platform is unsupported (WKWebView / WebKitGTK in v1).
    #[error("runtime backend: {0}")]
    Runtime(String),

    /// The target platform does not yet have a backend implementation.
    #[error("platform unsupported: the MV3 runtime is Windows-only in v1")]
    PlatformUnsupported,

    /// IPC bus failure — most commonly a message addressed to a port that
    /// no longer exists.
    #[error("ipc: {0}")]
    Ipc(String),

    /// Storage backend error — disk I/O or JSON encode/decode.
    #[error("storage: {0}")]
    Storage(String),

    /// The extension referenced by id is not loaded.
    #[error("extension not found: {0}")]
    ExtensionNotFound(String),

    /// Tauri returned an error (window management, eval, IPC).
    #[error(transparent)]
    Tauri(#[from] tauri::Error),

    /// JSON encode / decode error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// Filesystem I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}
