//! Test suite for the matcher subsystem.
//!
//! Organized as:
//!
//! - `match_pattern_accept` — patterns that must parse and then match /
//!   not-match the URLs in the Chrome reference.
//! - `match_pattern_reject` — malformed patterns that must return
//!   `Error::MatchPattern`.
//! - `glob_tests` — Chrome's `include_globs` / `exclude_globs` grammar.
//! - `chromium_reference_vectors` — cases cherry-picked from
//!   `chromium/src/extensions/common/url_pattern_unittest.cc`, encoded here so
//!   any divergence from the reference shows up as a red test rather than as
//!   a subtle runtime mismatch.

use super::{Glob, MatchPattern, MatchPatternSet};
use crate::Error;

fn parse(p: &str) -> MatchPattern {
    MatchPattern::parse(p).unwrap_or_else(|e| panic!("expected '{p}' to parse, got {e:?}"))
}

fn parse_err(p: &str) -> Error {
    match MatchPattern::parse(p) {
        Ok(_) => panic!("expected '{p}' to fail to parse, but it succeeded"),
        Err(e) => e,
    }
}

#[test]
fn all_urls_sentinel() {
    let m = parse("<all_urls>");
    assert!(m.is_all_urls());
    assert!(m.matches_str("https://example.com/"));
    assert!(m.matches_str("http://foo/bar"));
    assert!(m.matches_str("file:///etc/passwd"));
    assert!(m.matches_str("ftp://a.b/"));
    assert!(m.matches_str("urn:isbn:0451450523"));
    // chrome-extension / data / other schemes are NOT in the <all_urls> set.
    assert!(!m.matches_str("chrome-extension://abc/x"));
    assert!(!m.matches_str("data:text/plain,hello"));
}

#[test]
fn star_scheme_star_host_star_path() {
    let m = parse("*://*/*");
    assert!(m.matches_str("https://example.com/"));
    assert!(m.matches_str("http://a.b/"));
    assert!(m.matches_str("https://example.com/some/deep/path?q=1"));
    // `*` scheme matches http or https ONLY.
    assert!(!m.matches_str("file:///tmp/x"));
    assert!(!m.matches_str("ftp://host/path"));
    assert!(!m.matches_str("chrome-extension://abc/x"));
}

#[test]
fn https_star_host_star_path() {
    let m = parse("https://*/*");
    assert!(m.matches_str("https://example.com/"));
    assert!(m.matches_str("https://a.b/c"));
    assert!(!m.matches_str("http://example.com/"));
    assert!(!m.matches_str("ftp://example.com/"));
}

#[test]
fn subdomain_wildcard_matches_bare_domain() {
    let m = parse("https://*.google.com/*");
    assert!(m.matches_str("https://google.com/"));
    assert!(m.matches_str("https://www.google.com/"));
    assert!(m.matches_str("https://docs.www.google.com/"));
    assert!(m.matches_str("https://a.b.c.google.com/deep/path"));
    // Sibling / suffix-tricks must NOT match.
    assert!(!m.matches_str("https://googleblog.com/"));
    assert!(!m.matches_str("https://goo.gle/"));
    assert!(!m.matches_str("https://notgoogle.com/"));
    assert!(!m.matches_str("https://evilgoogle.com/"));
}

#[test]
fn exact_host_exact_path_prefix() {
    let m = parse("https://mail.google.com/mail/*");
    assert!(m.matches_str("https://mail.google.com/mail/"));
    assert!(m.matches_str("https://mail.google.com/mail/u/0/"));
    assert!(m.matches_str("https://mail.google.com/mail/u/0/#inbox"));
    assert!(!m.matches_str("https://mail.google.com/other"));
    assert!(!m.matches_str("https://docs.google.com/mail/"));
    assert!(!m.matches_str("http://mail.google.com/mail/"));
}

#[test]
fn file_scheme_requires_empty_host() {
    let m = parse("file:///etc/*");
    assert!(m.matches_str("file:///etc/passwd"));
    assert!(m.matches_str("file:///etc/ssh/sshd_config"));
    assert!(!m.matches_str("file:///var/log/syslog"));
}

#[test]
fn file_scheme_star_host_is_shorthand_for_empty() {
    // Chrome real-world behavior: `file://*/*` is shorthand for any
    // file URL (host wildcard folds into the empty-host match). Phantom's
    // content_scripts use this form; the strict-per-spec rejection broke
    // Phantom load on 2026-04-20.
    let m = parse("file://*/*");
    assert!(m.matches_str("file:///etc/passwd"));
    assert!(m.matches_str("file:///C:/Users/Me/index.html"));
    assert!(m.matches_str("file://localhost/tmp/x"));
    assert!(!m.matches_str("https://example.com/"));
}

#[test]
fn exact_pattern_matches_only_exact_url() {
    let m = parse("https://example.com/path/to/specific");
    assert!(m.matches_str("https://example.com/path/to/specific"));
    assert!(!m.matches_str("https://example.com/path/to/specific/more"));
    assert!(!m.matches_str("https://example.com/path/to/"));
    assert!(!m.matches_str("https://example.com/"));
}

#[test]
fn path_wildcard_middle() {
    let m = parse("https://example.com/foo/*/bar");
    assert!(m.matches_str("https://example.com/foo//bar"));
    assert!(m.matches_str("https://example.com/foo/x/bar"));
    assert!(m.matches_str("https://example.com/foo/a/b/c/bar"));
    assert!(!m.matches_str("https://example.com/foo/x/bar/extra"));
    assert!(!m.matches_str("https://example.com/foo/bar")); // missing separator
}

#[test]
fn multiple_path_wildcards() {
    let m = parse("https://example.com/*/mail/*");
    assert!(m.matches_str("https://example.com/u/mail/inbox"));
    assert!(m.matches_str("https://example.com//mail/"));
    assert!(!m.matches_str("https://example.com/mail/inbox"));
}

#[test]
fn case_insensitive_scheme_and_host() {
    let m = parse("HTTPS://Example.COM/*");
    assert!(m.matches_str("https://example.com/"));
    assert!(m.matches_str("https://EXAMPLE.com/path"));
    // Path is case-sensitive; Chrome keeps path case.
    let m2 = parse("https://example.com/Foo");
    assert!(m2.matches_str("https://example.com/Foo"));
    assert!(!m2.matches_str("https://example.com/foo"));
}

#[test]
fn empty_pattern_rejected() {
    let Error::MatchPattern { reason, .. } = parse_err("") else {
        panic!("expected MatchPattern error")
    };
    assert!(reason.contains("empty"));
}

#[test]
fn missing_scheme_separator_rejected() {
    let Error::MatchPattern { reason, .. } = parse_err("no-scheme-here") else {
        panic!("expected MatchPattern error")
    };
    assert!(reason.contains("'://'"));
}

#[test]
fn unknown_scheme_rejected() {
    let Error::MatchPattern { reason, .. } = parse_err("gopher://host/path") else {
        panic!("expected MatchPattern error")
    };
    assert!(reason.contains("scheme"));
    assert!(matches!(
        parse_err("foo://bar/baz"),
        Error::MatchPattern { .. }
    ));
}

#[test]
fn invalid_host_wildcard_rejected() {
    assert!(matches!(
        parse_err("http://*foo/*"),
        Error::MatchPattern { .. }
    ));
    assert!(matches!(
        parse_err("http://foo.*.com/*"),
        Error::MatchPattern { .. }
    ));
    assert!(matches!(
        parse_err("http://*.*/*"),
        Error::MatchPattern { .. }
    ));
}

#[test]
fn missing_path_rejected() {
    // No `/` after host → reject.
    let Error::MatchPattern { reason, .. } = parse_err("http://example.com") else {
        panic!("expected MatchPattern error")
    };
    assert!(reason.contains("path"));
}

#[test]
fn port_in_pattern_rejected() {
    // Chrome's match-pattern grammar does not accept ports. A match pattern
    // matches a URL regardless of the URL's port.
    assert!(matches!(
        parse_err("http://example.com:8080/*"),
        Error::MatchPattern { .. }
    ));
}

#[test]
fn pattern_matches_url_with_any_port() {
    let m = parse("http://example.com/*");
    assert!(m.matches_str("http://example.com/"));
    assert!(m.matches_str("http://example.com:8080/"));
    assert!(m.matches_str("http://example.com:9999/deep/path"));
}

#[test]
fn file_pattern_with_nonempty_host_rejected() {
    assert!(matches!(
        parse_err("file://host/etc/*"),
        Error::MatchPattern { .. }
    ));
}

#[test]
fn match_pattern_set_parse_and_match() {
    let set = MatchPatternSet::parse_many([
        "https://*.google.com/*",
        "file:///etc/*",
        "http://example.com/specific",
    ])
    .unwrap();
    assert_eq!(set.len(), 3);
    assert!(!set.is_empty());

    let u1 = url::Url::parse("https://docs.google.com/d").unwrap();
    let u2 = url::Url::parse("file:///etc/hosts").unwrap();
    let u3 = url::Url::parse("http://example.com/specific").unwrap();
    let u4 = url::Url::parse("http://example.com/other").unwrap();
    assert!(set.matches(&u1));
    assert!(set.matches(&u2));
    assert!(set.matches(&u3));
    assert!(!set.matches(&u4));
}

#[test]
fn match_pattern_set_empty() {
    let set = MatchPatternSet::default();
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
    let u = url::Url::parse("https://example.com/").unwrap();
    assert!(!set.matches(&u));
}

#[test]
fn match_pattern_set_propagates_parse_error() {
    let r = MatchPatternSet::parse_many(["https://*/*", "bad-pattern"]);
    assert!(r.is_err());
}

#[test]
fn matches_str_with_unparseable_url_is_false() {
    let m = parse("<all_urls>");
    assert!(!m.matches_str("not a url"));
    assert!(!m.matches_str(""));
}

#[test]
fn chrome_extension_scheme_requires_literal() {
    let m = parse("chrome-extension://abcdefghij/*");
    assert!(m.matches_str("chrome-extension://abcdefghij/popup.html"));
    // `*` scheme does not match chrome-extension.
    let star = parse("*://*/*");
    assert!(!star.matches_str("chrome-extension://abcdefghij/popup.html"));
}

#[test]
fn urn_scheme_parses() {
    // `urn` is a recognized literal scheme with an empty host. The `url`
    // crate treats `urn:` as non-special; we don't assert matching here
    // because parser behavior for `urn:...` varies by version. The
    // invariant is that the *pattern* parses.
    let _ = parse("urn:///*");
}

#[test]
fn data_scheme_parses() {
    // `data` is a recognized literal scheme with an empty host. Matching
    // data: URLs in practice is unusual (no extension would need to) but we
    // don't want to reject the pattern shape.
    let _ = parse("data:///*");
}

//
// ----------- Glob tests -----------
//

#[test]
fn glob_simple_star() {
    let g = Glob::parse("https://*.example.com/*").unwrap();
    assert!(g.matches("https://www.example.com/"));
    assert!(g.matches("https://a.example.com/path"));
    // Globs are char-level, so this specific pattern does NOT match a URL
    // whose host is bare `example.com` (no leading dot).
    assert!(!g.matches("https://example.com/"));
}

#[test]
fn glob_star_matches_slashes() {
    // `*` matches ANY sequence including `/` — unlike match patterns, the
    // glob is char-level.
    let g = Glob::parse("*://*/*").unwrap();
    assert!(g.matches("https://example.com/"));
    assert!(g.matches("http://a/b/c"));
    assert!(g.matches("file:///etc/passwd"));
}

#[test]
fn glob_question_mark_single_char() {
    let g = Glob::parse("?://?.?.?/").unwrap();
    // scheme=1 char, host parts each 1 char, path=/
    assert!(g.matches("a://b.c.d/"));
    assert!(!g.matches("ab://b.c.d/"));
    assert!(!g.matches("a://bb.c.d/"));
    assert!(!g.matches("a://b.c.d/e"));
    assert!(!g.matches("a://b.c.d"));
}

#[test]
fn glob_mixed_literal_star_literal() {
    let g = Glob::parse("https://*/foo*bar").unwrap();
    assert!(g.matches("https://example.com/foobar"));
    assert!(g.matches("https://example.com/foo-x-bar"));
    assert!(g.matches("https://example.com/foo/lots/of/stuff/bar"));
    assert!(!g.matches("https://example.com/fooBAR")); // case-sensitive
    assert!(!g.matches("https://example.com/foo"));
    assert!(!g.matches("http://example.com/foobar"));
}

#[test]
fn glob_anchored() {
    let g = Glob::parse("https://example.com/foo").unwrap();
    assert!(g.matches("https://example.com/foo"));
    assert!(!g.matches("https://example.com/foobar"));
    assert!(!g.matches("prefix-https://example.com/foo"));
}

#[test]
fn glob_escapes_regex_metachars() {
    // Parens, dots, and braces are literal in globs, not regex metachars.
    let g = Glob::parse("https://example.com/(a+b).html").unwrap();
    assert!(g.matches("https://example.com/(a+b).html"));
    assert!(!g.matches("https://example.com/aaab.html"));
    assert!(!g.matches("https://example.com/a+b.html"));
}

#[test]
fn glob_empty_pattern_matches_only_empty() {
    let g = Glob::parse("").unwrap();
    assert!(g.matches(""));
    assert!(!g.matches("x"));
}

#[test]
fn glob_only_star_matches_anything() {
    let g = Glob::parse("*").unwrap();
    assert!(g.matches(""));
    assert!(g.matches("anything goes\nincluding newlines"));
    assert!(g.matches("https://example.com/"));
}

//
// ----------- Reference vectors -----------
//
// Cherry-picked from Chromium's url_pattern_unittest.cc. Each assertion is
// labelled with the test name it came from so we can audit drift against the
// upstream file.

#[test]
fn chromium_reference_vectors() {
    // ParseInvalid cases — malformed patterns must fail.
    for bad in [
        "",
        "http",
        "http:",
        "http:/",
        "about://",
        "http://*foo/bar",
        "http://foo.*.bar/baz",
        "http:/bar",
        "http://",
        "chrome://",
    ] {
        assert!(
            MatchPattern::parse(bad).is_err(),
            "expected '{bad}' to fail to parse"
        );
    }

    // Scheme `*` matches http + https only. (Match_SchemeStar)
    let p = parse("*://google.com/foo");
    assert!(p.matches_str("http://google.com/foo"));
    assert!(p.matches_str("https://google.com/foo"));
    assert!(!p.matches_str("file:///google.com/foo"));
    assert!(!p.matches_str("ftp://google.com/foo"));

    // Match_SubdomainMatching.
    let p = parse("http://*.google.com/foo");
    assert!(p.matches_str("http://google.com/foo"));
    assert!(p.matches_str("http://www.google.com/foo"));
    assert!(p.matches_str("http://monkey.images.google.com/foo"));
    assert!(!p.matches_str("http://www.google.com/foobar"));

    // Match_SpecificPort — port is ignored, pattern still matches.
    let p = parse("http://www.example.com/foo");
    assert!(p.matches_str("http://www.example.com:80/foo"));
    assert!(p.matches_str("http://www.example.com/foo"));
    assert!(p.matches_str("http://www.example.com:1234/foo"));

    // Match_ExplicitPortWildcard — ports are not allowed in the pattern.
    assert!(MatchPattern::parse("http://www.example.com:*/foo").is_err());

    // Match_IgnorePorts — path wildcards still work regardless of port.
    let p = parse("http://www.example.com/foo/*");
    assert!(p.matches_str("http://www.example.com:8080/foo/bar"));

    // Match_FileScheme — host must be empty for file patterns.
    // Note: `url::Url` normalizes `file://localhost/...` → host=None, so the
    // two spellings are indistinguishable at match time. Chrome behaves the
    // same way (WHATWG URL compat), so both forms match.
    let p = parse("file:///foo/bar");
    assert!(p.matches_str("file:///foo/bar"));
    assert!(p.matches_str("file://localhost/foo/bar"));

    // Match_FileSchemeWithAnyHost — hostless path wildcard.
    let p = parse("file:///*");
    assert!(p.matches_str("file:///"));
    assert!(p.matches_str("file:///foo/bar"));
    assert!(p.matches_str("file:///etc/passwd"));

    // Match_Path — path must match exactly (no wildcard).
    let p = parse("http://www.example.com/foo");
    assert!(p.matches_str("http://www.example.com/foo"));
    assert!(!p.matches_str("http://www.example.com/foo/"));
    assert!(!p.matches_str("http://www.example.com/foobar"));
    assert!(!p.matches_str("http://www.example.com/"));

    // Match_PathWildcard — `*` inside path.
    let p = parse("http://www.example.com/foo*");
    assert!(p.matches_str("http://www.example.com/foo"));
    assert!(p.matches_str("http://www.example.com/foobar"));
    assert!(p.matches_str("http://www.example.com/foo/bar"));
    assert!(!p.matches_str("http://www.example.com/fo"));

    // Match_ChromeUrls — chrome:// scheme is NOT in our allow list; reject.
    assert!(MatchPattern::parse("chrome://favicon/*").is_err());

    // Match_AllUrls sentinel sanity.
    let p = parse("<all_urls>");
    assert!(p.matches_str("http://a/b"));
    assert!(p.matches_str("https://a/b"));
    assert!(p.matches_str("file:///foo"));
    assert!(p.matches_str("ftp://a/b"));
    assert!(!p.matches_str("chrome-extension://xyz/popup.html"));
}
