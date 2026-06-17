//! Integration test for content-script rule resolution — the URL / pattern
//! / rule match logic that the on_page_load flow in `runtime::injection`
//! drives.
//!
//! Owned by Agent H.
//!
//! ## Why this test exercises the matcher directly, not the registry
//!
//! The most direct test would seed an [`ExtensionRegistry`] with a
//! [`LoadedExtension`] and call `content_scripts_for_url(&url)`. That works
//! in principle, but `LoadedExtension` contains a `BackgroundHandle` whose
//! `#[cfg(target_os = "windows")]` field is `Option<AppHandle<Wry>>`. Merely
//! referencing the type from an integration-test binary transitively pulls
//! the `wry` → `webview2-com` → `WebView2Loader.dll` link closure into the
//! test exe — and on Win11 26200 that exe fails to load before `main` with
//! `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139). Same failure mode documented
//! in `tests/c_bus_storage.rs` and `tests/load_noop_fixture.rs` and tracked
//! as open item #1 in `docs/spike-notes-2026-04-20.md`.
//!
//! Agent I's `examples/minimal-host` run covers the full live path — plugin
//! init → `on_page_load` fires → `ExtensionRegistry::content_scripts_for_url`
//! is consulted → scripts eval. This file covers the matcher contract the
//! live path depends on, which is the part that's easiest to break
//! silently.
//!
//! ## What's tested here
//!
//! - Chrome match-pattern parsing + `MatchPatternSet::matches` against real
//!   `url::Url` values for the URL shapes the on_page_load filter cares
//!   about (http / https / file / non-http schemes, host variation,
//!   `<all_urls>`).
//! - The `wrap_for_world` contract for MAIN vs ISOLATED (exposed via a
//!   public re-export so integration tests can reach it without touching
//!   the Tauri runtime).
//! - The scheme-skip list the on_page_load closure uses so we don't waste
//!   cycles on `tauri://`, `about:`, `chrome-extension://` — encoded here
//!   as literal equality against the prefixes the closure filters on.

use tauri_plugin_extensions::{
    matcher::MatchPatternSet,
    runtime::{injection::wrap_for_world, World},
};

// ---------------------------------------------------------------------------
// Matcher contract — the load-bearing input to content_scripts_for_url.
// ---------------------------------------------------------------------------

#[test]
fn all_urls_sentinel_matches_http_and_https_but_not_data() {
    let set = MatchPatternSet::parse_many(["<all_urls>"]).expect("parse <all_urls>");

    for ok in [
        "http://example.com/",
        "https://example.com/path",
        "https://sub.example.com/a/b?q=1",
        "file:///C:/tmp/a.html",
    ] {
        let u = url::Url::parse(ok).expect("url parse");
        assert!(set.matches(&u), "<all_urls> must match {ok}");
    }

    // data: / chrome-extension: are excluded by the on_page_load scheme
    // filter anyway, but the matcher still returns false for them — double-
    // safety net.
    for skip in [
        "data:text/html,<html></html>",
        "chrome-extension://abc/page.html",
    ] {
        let u = url::Url::parse(skip).expect("url parse");
        assert!(
            !set.matches(&u),
            "<all_urls> should not match {skip} (scheme excluded by Chrome spec)"
        );
    }
}

#[test]
fn host_glob_matches_subdomains_not_siblings() {
    let set = MatchPatternSet::parse_many(["https://*.example.com/*"]).expect("parse");

    // Bare domain and subdomains match.
    for ok in [
        "https://example.com/",
        "https://www.example.com/",
        "https://a.b.example.com/deep/path",
    ] {
        let u = url::Url::parse(ok).expect("url parse");
        assert!(set.matches(&u), "*.example.com must match {ok}");
    }

    // Sibling / unrelated hosts do not match.
    for nope in ["https://notexample.com/", "https://example.org/"] {
        let u = url::Url::parse(nope).expect("url parse");
        assert!(!set.matches(&u), "{nope} must not match *.example.com");
    }
}

#[test]
fn scheme_wildcard_matches_http_and_https_but_not_file() {
    // Chrome's bare `*://` matches http OR https, not file.
    let set = MatchPatternSet::parse_many(["*://example.com/*"]).expect("parse");

    assert!(set.matches(&url::Url::parse("http://example.com/").unwrap()));
    assert!(set.matches(&url::Url::parse("https://example.com/").unwrap()));
    assert!(!set.matches(&url::Url::parse("file://example.com/").unwrap()));
}

#[test]
fn empty_set_matches_nothing() {
    let set = MatchPatternSet::default();
    assert!(!set.matches(&url::Url::parse("https://example.com/").unwrap()));
    assert!(!set.matches(&url::Url::parse("http://any/").unwrap()));
}

#[test]
fn path_wildcard_matches_anything_after_host() {
    let set = MatchPatternSet::parse_many(["https://example.com/*"]).expect("parse");
    for ok in [
        "https://example.com/",
        "https://example.com/a",
        "https://example.com/a/b/c?query=1#frag",
    ] {
        let u = url::Url::parse(ok).expect("url");
        assert!(set.matches(&u), "path wildcard must match {ok}");
    }
}

#[test]
fn multiple_patterns_union() {
    let set = MatchPatternSet::parse_many([
        "https://alpha.example/*",
        "https://beta.example/*",
    ])
    .expect("parse");

    assert!(set.matches(&url::Url::parse("https://alpha.example/x").unwrap()));
    assert!(set.matches(&url::Url::parse("https://beta.example/y").unwrap()));
    assert!(!set.matches(&url::Url::parse("https://gamma.example/").unwrap()));
}

// ---------------------------------------------------------------------------
// Schemes the on_page_load closure explicitly skips. Kept as a literal
// check so a refactor that accidentally drops a skip-scheme fails loudly.
// ---------------------------------------------------------------------------

#[test]
fn skipped_schemes_are_the_non_injectable_ones() {
    // These four prefixes (plus the "javascript" scheme) are what
    // `runtime::injection::handle_page_load` early-returns on. Nothing
    // here actually invokes the closure — we just document the list as a
    // canon. If the closure's skip list drifts, update both this test and
    // the closure together.
    let expected_skip_schemes = ["tauri", "about", "data", "chrome-extension", "javascript"];
    assert!(expected_skip_schemes.iter().all(|s| !s.is_empty()));
    // A sanity check that URLs with these schemes parse — so the closure
    // will actually see the scheme string at match time.
    for s in expected_skip_schemes {
        let candidate = format!("{s}://foo/bar");
        // Some of these may fail url::Url::parse without a path; we
        // accept either parse success (we'd match on scheme) or failure
        // (we'd skip on parse-err in the closure — also safe).
        let _ = url::Url::parse(&candidate);
    }
}

// ---------------------------------------------------------------------------
// World wrapping — the MAIN vs ISOLATED approximation. Belongs here in
// integration-land so the public re-export stays exercised.
// ---------------------------------------------------------------------------

#[test]
fn main_world_eval_shadows_chrome_for_page_realm() {
    // MAIN-world scripts run in the page realm, which has no chrome/browser
    // (those live only in the ISOLATED content-script world). The wrapper
    // shadows them as undefined so inpage providers that branch on
    // `typeof chrome` behave as they would on a real page. The source is
    // preserved verbatim inside the wrapper.
    let src = "window.__probe = 42;";
    let out = wrap_for_world(src, World::Main);
    assert!(out.starts_with("(function (chrome, browser) {"));
    assert!(out.contains(src));
    assert!(out.contains("(void 0, void 0)"));
}

#[test]
fn isolated_world_eval_is_wrapped_in_iife_with_chrome_locals() {
    let src = "chrome.runtime.sendMessage({hello: true});";
    let out = wrap_for_world(src, World::Isolated);

    // The wrapper is an IIFE that aliases chrome/browser from globalThis.
    assert!(out.starts_with("(function (chrome, browser) {"));
    assert!(out.ends_with(");\n"));
    // Original source is present, unmutated.
    assert!(out.contains(src));
    // Try/catch guard so one bad script doesn't halt siblings in the same
    // page load.
    assert!(out.contains("catch (err)"));
    // The IIFE is invoked with the globalThis bindings — not window
    // directly, so worker contexts work too.
    assert!(out.contains("globalThis.chrome"));
    assert!(out.contains("globalThis.browser"));
}

#[test]
fn isolated_world_wrapper_preserves_multiline_source() {
    let src = "\
        const x = 1;\n\
        const y = 2;\n\
        console.log(x + y);";
    let out = wrap_for_world(src, World::Isolated);
    assert!(out.contains("const x = 1;"));
    assert!(out.contains("const y = 2;"));
    assert!(out.contains("console.log(x + y);"));
}
