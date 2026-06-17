//! Chrome MV3 `manifest.json` parser.
//!
//! Owned by Agent A per `docs/spike-plan.md`. v1 parses the subset of fields
//! declared by Phantom, MetaMask, and Rabby; unknown fields are preserved but
//! ignored so forward-compat doesn't break cold.
//!
//! Public API:
//!
//! - [`parse`] — byte-slice in, typed [`Manifest`] out.
//! - [`parse_from_path`] — convenience wrapper that reads from disk.
//!
//! Error contract: anything that is not a well-formed MV3 manifest yields
//! [`crate::Error::Manifest`]. IO errors from [`parse_from_path`] come back
//! as [`crate::Error::Io`].

mod schema;

pub use schema::{
    Action, ActionIcon, Author, Background, BackgroundType, ContentScript, Csp,
    ExternallyConnectable, Manifest, OptionsUi, RunAt, WebAccessibleResource, World,
};

use std::path::Path;

/// Parse a manifest.json byte slice.
///
/// Rejects MV2, rejects the MV2 string-form `content_security_policy`, and
/// rejects the MV2 string-array form of `web_accessible_resources` — each
/// with a [`crate::Error::Manifest`] carrying a human-readable reason.
///
/// Unknown top-level fields are preserved in [`Manifest::extra`] rather than
/// failing the parse; see the module docs for why.
pub fn parse(bytes: &[u8]) -> crate::Result<Manifest> {
    // First pass: peek at the raw JSON so we can surface MV2 / MV2-shape
    // errors with precise messages before serde attempts a typed parse that
    // would blow up with a less-useful default message.
    let raw: serde_json::Value = serde_json::from_slice(bytes)?;

    // MV3 is the only supported manifest version. Reject everything else up
    // front so downstream code can assume `manifest_version == 3`.
    let mv = raw
        .get("manifest_version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| crate::Error::Manifest("missing manifest_version".into()))?;
    if mv != 3 {
        return Err(crate::Error::Manifest(format!(
            "manifest_version must be 3; got {mv}"
        )));
    }

    // MV2 CSP was a bare string. MV3 requires the object form.
    if let Some(csp) = raw.get("content_security_policy") {
        if csp.is_string() {
            return Err(crate::Error::Manifest(
                "MV3 requires object-form content_security_policy \
                 (e.g. { \"extension_pages\": \"...\" }); got a string"
                    .into(),
            ));
        }
    }

    // MV2 web_accessible_resources was `["path/a.js", "path/b.js"]`. MV3
    // demands an array of objects — reject the legacy shape.
    if let Some(war) = raw.get("web_accessible_resources") {
        if let Some(arr) = war.as_array() {
            if arr.iter().any(|v| v.is_string()) {
                return Err(crate::Error::Manifest(
                    "MV3 requires object-form web_accessible_resources \
                     entries; got a string-array (MV2 form)"
                        .into(),
                ));
            }
        }
    }

    // MV2 background.scripts is tolerated but a warning signal — log it.
    if let Some(bg) = raw.get("background") {
        if bg.get("scripts").and_then(|s| s.as_array()).is_some() {
            tracing::warn!(
                "background.scripts is MV2-shaped; MV3 expects service_worker. \
                 Ignoring scripts[] and continuing."
            );
        }
    }

    // Second pass: typed deserialization. Any remaining structural error
    // bubbles up as Error::Json → we rewrap as Error::Manifest for a clearer
    // message at call sites.
    let manifest: Manifest = serde_json::from_value(raw).map_err(|e| {
        crate::Error::Manifest(format!("schema mismatch parsing manifest.json: {e}"))
    })?;
    Ok(manifest)
}

/// Read and parse a manifest.json from the given path.
pub fn parse_from_path(path: &Path) -> crate::Result<Manifest> {
    let bytes = std::fs::read(path)?;
    parse(&bytes)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Noop MV3 fixture lives at the repo root; resolve relative to this
    /// crate's manifest dir so tests don't depend on the cwd.
    const NOOP_MANIFEST: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/test-extensions/noop-mv3/manifest.json"
    ));

    #[test]
    fn parses_noop_manifest_with_every_field_asserted() {
        let m = parse(NOOP_MANIFEST.as_bytes()).expect("noop manifest should parse");

        assert_eq!(m.manifest_version, 3);
        assert_eq!(m.name, "Noop MV3 (spike fixture)");
        assert_eq!(m.version.as_deref(), Some("0.1.0"));
        assert!(m.description.as_ref().unwrap().contains("Minimal MV3"));

        // Background — MV3 shape, module type.
        let bg = m.background.as_ref().expect("background block present");
        assert_eq!(bg.service_worker.as_deref(), Some("background.js"));
        assert_eq!(bg.r#type, BackgroundType::Module);
        assert!(bg.scripts.is_empty());

        // Content scripts — exactly one entry with world=ISOLATED (uppercase
        // in the fixture, normalized here).
        assert_eq!(m.content_scripts.len(), 1);
        let cs = &m.content_scripts[0];
        assert_eq!(cs.matches, vec!["<all_urls>".to_string()]);
        assert_eq!(cs.js, vec!["content.js".to_string()]);
        assert_eq!(cs.run_at, RunAt::DocumentEnd);
        assert_eq!(cs.world, World::Isolated);
        assert!(!cs.all_frames);

        assert_eq!(m.permissions, vec!["storage".to_string()]);
        assert!(m.host_permissions.is_empty());

        assert_eq!(m.icons.get("16").map(String::as_str), Some("icon.png"));
        assert_eq!(m.icons.get("48").map(String::as_str), Some("icon.png"));
        assert_eq!(m.icons.get("128").map(String::as_str), Some("icon.png"));
    }

    #[test]
    fn rejects_mv2_manifest() {
        let mv2 = r#"{"manifest_version": 2, "name": "legacy", "version": "1"}"#;
        let err = parse(mv2.as_bytes()).expect_err("MV2 must be rejected");
        match err {
            crate::Error::Manifest(msg) => assert!(msg.contains("manifest_version must be 3")),
            other => panic!("expected Error::Manifest; got {other:?}"),
        }
    }

    #[test]
    fn missing_manifest_version_errors() {
        let bad = r#"{"name": "no version", "version": "1"}"#;
        let err = parse(bad.as_bytes()).expect_err("missing mv must fail");
        match err {
            crate::Error::Manifest(msg) => assert!(msg.contains("missing manifest_version")),
            other => panic!("expected Error::Manifest; got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_field_does_not_fail() {
        // `update_url`, `commands`, `side_panel`, etc. are real-world fields
        // we haven't lifted into typed form yet. Parsing must not blow up on
        // them — they round-trip through `extra`.
        let with_extras = r#"{
            "manifest_version": 3,
            "name": "Extras",
            "version": "0.1.0",
            "update_url": "https://example.com/updates",
            "something_new_in_chrome_200": {"foo": "bar"}
        }"#;
        let m = parse(with_extras.as_bytes()).expect("extras should not fail");
        assert!(m.extra.contains_key("update_url"));
        assert!(m.extra.contains_key("something_new_in_chrome_200"));
    }

    #[test]
    fn world_case_insensitive_matches_phantom_shape() {
        // This is the load-bearing Phantom case: their content_scripts use
        // uppercase "ISOLATED" / "MAIN". If this regresses, Phantom fails to
        // load and we flag-ship the wrong world.
        let upper = r#"{
            "manifest_version": 3,
            "name": "world-caps",
            "version": "0.1",
            "content_scripts": [
                {"matches": ["<all_urls>"], "js": ["a.js"], "world": "ISOLATED"},
                {"matches": ["<all_urls>"], "js": ["b.js"], "world": "MAIN"},
                {"matches": ["<all_urls>"], "js": ["c.js"], "world": "isolated"},
                {"matches": ["<all_urls>"], "js": ["d.js"], "world": "main"}
            ]
        }"#;
        let m = parse(upper.as_bytes()).expect("case-insensitive world should parse");
        assert_eq!(m.content_scripts[0].world, World::Isolated);
        assert_eq!(m.content_scripts[1].world, World::Main);
        assert_eq!(m.content_scripts[2].world, World::Isolated);
        assert_eq!(m.content_scripts[3].world, World::Main);
    }

    #[test]
    fn rejects_mv2_csp_string_form() {
        let s = r#"{
            "manifest_version": 3,
            "name": "bad-csp",
            "version": "0.1",
            "content_security_policy": "script-src 'self'; object-src 'self'"
        }"#;
        let err = parse(s.as_bytes()).expect_err("MV2 CSP must be rejected");
        match err {
            crate::Error::Manifest(msg) => assert!(msg.contains("object-form")),
            other => panic!("expected Error::Manifest; got {other:?}"),
        }
    }

    #[test]
    fn accepts_mv3_csp_object_form() {
        let s = r#"{
            "manifest_version": 3,
            "name": "ok-csp",
            "version": "0.1",
            "content_security_policy": {
                "extension_pages": "script-src 'self'; object-src 'self'"
            }
        }"#;
        let m = parse(s.as_bytes()).expect("MV3 CSP must parse");
        assert_eq!(
            m.content_security_policy
                .as_ref()
                .and_then(|c| c.extension_pages.as_deref()),
            Some("script-src 'self'; object-src 'self'")
        );
    }

    #[test]
    fn rejects_mv2_web_accessible_resources_string_array() {
        let s = r#"{
            "manifest_version": 3,
            "name": "bad-war",
            "version": "0.1",
            "web_accessible_resources": ["a.js", "b.js"]
        }"#;
        let err = parse(s.as_bytes()).expect_err("MV2 WAR must be rejected");
        match err {
            crate::Error::Manifest(msg) => assert!(msg.contains("web_accessible_resources")),
            other => panic!("expected Error::Manifest; got {other:?}"),
        }
    }

    #[test]
    fn author_accepts_string_or_object() {
        let as_string = r#"{
            "manifest_version": 3,
            "name": "a",
            "version": "0.1",
            "author": "https://metamask.io"
        }"#;
        let m = parse(as_string.as_bytes()).expect("string author parses");
        match m.author.as_ref().unwrap() {
            Author::String(s) => assert_eq!(s, "https://metamask.io"),
            Author::Object { .. } => panic!("expected string variant"),
        }

        let as_obj = r#"{
            "manifest_version": 3,
            "name": "b",
            "version": "0.1",
            "author": {"email": "dev@example.com"}
        }"#;
        let m = parse(as_obj.as_bytes()).expect("object author parses");
        match m.author.as_ref().unwrap() {
            Author::Object { email } => assert_eq!(email.as_deref(), Some("dev@example.com")),
            Author::String(_) => panic!("expected object variant"),
        }
    }

    #[test]
    fn default_run_at_is_document_idle() {
        let s = r#"{
            "manifest_version": 3,
            "name": "r",
            "version": "0.1",
            "content_scripts": [{"matches": ["<all_urls>"], "js": ["x.js"]}]
        }"#;
        let m = parse(s.as_bytes()).expect("parses");
        assert_eq!(m.content_scripts[0].run_at, RunAt::DocumentIdle);
        assert_eq!(m.content_scripts[0].world, World::Isolated);
    }

    #[test]
    fn mv2_background_scripts_tolerated_not_fatal() {
        // A manifest claims MV3 but still has MV2 `background.scripts`.
        // We log-and-drop rather than fail; real-world extensions occasionally
        // ship like this during migration. `service_worker` should be None
        // and the caller gets to decide whether that's acceptable.
        let s = r#"{
            "manifest_version": 3,
            "name": "hybrid-bg",
            "version": "0.1",
            "background": {"scripts": ["legacy-bg.js"]}
        }"#;
        let m = parse(s.as_bytes()).expect("tolerant parse");
        let bg = m.background.unwrap();
        assert_eq!(bg.service_worker, None);
        assert_eq!(bg.scripts, vec!["legacy-bg.js".to_string()]);
    }

    #[test]
    fn snapshot_noop_manifest_debug() {
        // Golden/snapshot coverage via insta. Debug output captures the full
        // parsed structure; changing the shape of `Manifest` will force a
        // snapshot review.
        let m = parse(NOOP_MANIFEST.as_bytes()).expect("parses");
        insta::assert_debug_snapshot!("noop_mv3_manifest", m);
    }
}
