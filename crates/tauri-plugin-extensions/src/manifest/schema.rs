//! Typed structs for the subset of Chrome MV3 `manifest.json` that real wallet
//! extensions declare.
//!
//! Shape conventions:
//!
//! - Every struct derives `Debug, Clone, Serialize, Deserialize`.
//! - Optional top-level keys are `Option<T>` so their absence is meaningful.
//! - Nested structs use `#[serde(default)]` on fields with sane defaults
//!   (empty vec, false, enum default) so partial objects parse.
//! - Unknown top-level keys are preserved in [`Manifest::extra`] via
//!   `#[serde(flatten)]` — forward-compat is a hard requirement. Chrome adds
//!   new fields every release and we refuse to fail cold on them.
//!
//! Fields were chosen by inspecting Phantom, MetaMask, and Rabby's current
//! MV3 manifests. New fields get added here as they appear in target
//! extensions; see `crates/tauri-plugin-extensions/tests/fixtures/manifests/`
//! for the reference corpus.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

// ---------------------------------------------------------------------------
// Top-level
// ---------------------------------------------------------------------------

/// Parsed Chrome MV3 `manifest.json`.
///
/// `manifest_version` is validated in [`crate::manifest::parse`] — MV2 is
/// rejected with [`crate::Error::Manifest`] before this struct is returned to
/// a caller, so downstream code can assume `manifest_version == 3`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Always 3 in v1. MV2 is rejected in [`crate::manifest::parse`].
    pub manifest_version: u8,

    /// Human-readable name (may be a `__MSG_*__` locale placeholder).
    pub name: String,

    /// Semver-ish extension version string (e.g. `"26.13.0"`).
    ///
    /// Chrome's MV3 spec marks this required, and packaged extensions
    /// always have it. But several in-tree manifest *templates* (MetaMask's
    /// `app/manifest/v3/_base.json`, for example) omit `version` and inject
    /// it at build time from platform-override JSON. We model the field as
    /// optional so those templates parse cleanly; the loader tier is the
    /// right place to enforce presence on actually-unpacked extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Short name, shown in places with limited width (Chrome app launcher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,

    /// Human-readable description (often `__MSG_*__`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Author. Chrome accepts either a bare string or `{ "email": "..." }`
    /// object form — both parse here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<Author>,

    /// Public homepage URL shown in the store listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage_url: Option<String>,

    /// Locale subdirectory that `__MSG_*__` placeholders resolve against
    /// (e.g. `"en"` → `_locales/en/messages.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_locale: Option<String>,

    /// Icon set, keyed by pixel size as a string (`"16"`, `"128"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub icons: BTreeMap<String, String>,

    /// Static permissions granted at install time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,

    /// Permissions the user may grant at runtime via `chrome.permissions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_permissions: Vec<String>,

    /// Origin patterns the extension needs host-level access to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_permissions: Vec<String>,

    /// Content scripts injected into matching pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_scripts: Vec<ContentScript>,

    /// Background service worker (MV3 form). MV2-style `scripts` arrays are
    /// tolerated but ignored — see [`Background`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Background>,

    /// Toolbar button definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,

    /// Embedded options page (modern form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_ui: Option<OptionsUi>,

    /// Legacy options page (MV2 → MV3 carryover — path to an HTML file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options_page: Option<String>,

    /// Resources the extension wants to expose to web pages. MV3 requires the
    /// object form — see [`WebAccessibleResource`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_accessible_resources: Vec<WebAccessibleResource>,

    /// Content Security Policy. MV3 requires the object form; MV2's string
    /// form is rejected by [`crate::manifest::parse`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_security_policy: Option<Csp>,

    /// Other extensions / web pages that may `chrome.runtime.connect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub externally_connectable: Option<ExternallyConnectable>,

    /// Minimum Chrome version the extension will install on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_chrome_version: Option<String>,

    /// Base64 DER-encoded public key. Chrome derives the extension ID from
    /// this. Optional — only present in packaged builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Every top-level key we don't explicitly model lands here so forward
    /// compat never crashes the parser. Useful when debugging: a target
    /// extension may declare `sandbox`, `side_panel`, `commands`,
    /// `update_url`, etc. that we haven't lifted into typed form yet.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Author (string or { email } object)
// ---------------------------------------------------------------------------

/// `author` field — accepts either a bare string ("https://metamask.io") or
/// the legacy Chrome object form `{ "email": "foo@bar" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Author {
    /// Bare string — most common. URL, name, or free-form text.
    String(String),
    /// Object form `{ "email": "..." }`.
    Object {
        /// Author email.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        email: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Content scripts
// ---------------------------------------------------------------------------

/// One entry in the `content_scripts` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentScript {
    /// URL patterns the script should inject into.
    #[serde(default)]
    pub matches: Vec<String>,

    /// URL patterns to exclude from the above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_matches: Vec<String>,

    /// Shell-style globs to restrict injection further.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_globs: Vec<String>,

    /// Shell-style globs to exclude injection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_globs: Vec<String>,

    /// JS files to inject, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub js: Vec<String>,

    /// CSS files to inject.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub css: Vec<String>,

    /// When in the page lifecycle to inject.
    #[serde(default)]
    pub run_at: RunAt,

    /// Isolated world (default, Chrome-recommended) or main world (shares
    /// page globals — used by Phantom / MetaMask for `window.ethereum`-style
    /// provider injection).
    #[serde(default, deserialize_with = "deserialize_world")]
    pub world: World,

    /// Inject into child frames too. Defaults to false.
    #[serde(default)]
    pub all_frames: bool,

    /// Inject into `about:blank` frames that inherit a matching origin.
    #[serde(default)]
    pub match_about_blank: bool,

    /// Extend the matcher to consider the initiator origin when the frame's
    /// own URL is opaque (e.g. `data:`, `about:blank`).
    #[serde(default)]
    pub match_origin_as_fallback: bool,
}

/// `run_at` enum. Chrome default when omitted is `document_idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAt {
    /// Inject as early as possible — before any page script runs.
    DocumentStart,
    /// Inject when the DOM is ready (after `DOMContentLoaded`).
    DocumentEnd,
    /// Chrome default. Inject when the page is idle (after `load`).
    #[default]
    DocumentIdle,
}

/// Execution world for content scripts. Isolated is Chrome's default.
///
/// This enum is case-sensitive in the normal serde path, but real manifests
/// (Phantom, MetaMask) use UPPERCASE `"MAIN"` / `"ISOLATED"`. We install a
/// custom deserializer on [`ContentScript::world`] to lowercase the value
/// before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum World {
    /// Private JS context; does not share globals with the page.
    #[default]
    Isolated,
    /// Main page world — globals like `window.ethereum` can be set here.
    Main,
}

fn deserialize_world<'de, D: Deserializer<'de>>(d: D) -> Result<World, D::Error> {
    // Accept "ISOLATED" / "isolated" / "MAIN" / "main" — real manifests in
    // the wild use both conventions, so we normalize to lowercase here.
    let raw = Option::<String>::deserialize(d)?;
    Ok(match raw {
        None => World::default(),
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "isolated" => World::Isolated,
            "main" => World::Main,
            other => {
                return Err(serde::de::Error::unknown_variant(
                    other,
                    &["isolated", "main"],
                ));
            }
        },
    })
}

// ---------------------------------------------------------------------------
// Background
// ---------------------------------------------------------------------------

/// MV3 `background` block.
///
/// MV3 uses `service_worker`. If a manifest still carries the MV2 `scripts`
/// array, the parser logs a warning and ignores it rather than failing —
/// several in-the-wild MV3 builds haven't scrubbed the legacy field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Background {
    /// Path to the service worker JS. Required on MV3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_worker: Option<String>,

    /// `"classic"` or `"module"`. Chrome's default when omitted is classic.
    #[serde(default)]
    pub r#type: BackgroundType,

    /// Legacy MV2 field; retained for diagnostics. Should be empty on MV3.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<String>,
}

/// `background.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundType {
    /// Classic script loader. Chrome's default.
    #[default]
    Classic,
    /// ES module loader (`import`/`export` supported at the top level).
    Module,
}

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// `action` — toolbar button.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    /// Popup HTML to open when clicked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_popup: Option<String>,

    /// Tooltip title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_title: Option<String>,

    /// Either a single icon path, or an icons-map keyed by size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_icon: Option<ActionIcon>,
}

/// `action.default_icon` accepts either a bare string (one icon for all
/// sizes) or a `{ "16": "...", "32": "..." }` map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionIcon {
    /// Single path used at all sizes.
    Single(String),
    /// Map keyed by pixel size (stringified).
    Map(BTreeMap<String, String>),
}

// ---------------------------------------------------------------------------
// Options UI
// ---------------------------------------------------------------------------

/// `options_ui`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionsUi {
    /// Path to the options HTML page.
    pub page: String,
    /// Open in a full tab instead of an embedded iframe.
    #[serde(default)]
    pub open_in_tab: bool,
}

// ---------------------------------------------------------------------------
// Web accessible resources
// ---------------------------------------------------------------------------

/// One entry in the MV3 `web_accessible_resources` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAccessibleResource {
    /// Resource paths (inside the extension) to expose.
    #[serde(default)]
    pub resources: Vec<String>,

    /// URL match patterns that may load the resources. MV3 requires this
    /// field — bare string arrays (MV2 form) fail in [`crate::manifest::parse`].
    #[serde(default)]
    pub matches: Vec<String>,

    /// Extension IDs permitted to access (optional).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_ids: Vec<String>,

    /// If true, Chrome assigns a randomized URL per session.
    #[serde(default)]
    pub use_dynamic_url: bool,
}

// ---------------------------------------------------------------------------
// CSP
// ---------------------------------------------------------------------------

/// Content Security Policy — MV3 object form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Csp {
    /// Policy applied to extension-owned pages (popup, options, BG).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_pages: Option<String>,

    /// Policy applied to sandboxed pages (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
}

// ---------------------------------------------------------------------------
// Externally connectable
// ---------------------------------------------------------------------------

/// `externally_connectable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternallyConnectable {
    /// URL match patterns allowed to connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<String>,

    /// Other extension IDs allowed to connect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,

    /// If true, web page `connect` traffic is permitted.
    #[serde(default)]
    pub accepts_tls_channel_id: bool,
}
