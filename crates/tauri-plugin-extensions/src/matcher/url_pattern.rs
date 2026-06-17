//! Chrome-style URL match patterns.
//!
//! Grammar (simplified from the Chrome docs):
//!
//! ```text
//! <url-pattern>  := <scheme>://<host><path>
//! <scheme>       := '*' | 'http' | 'https' | 'file' | 'ftp' | 'urn'
//!                 | 'chrome-extension' | 'data'
//! <host>         := '*' | '*.' <any-char>+ | <any-char>+    (empty for file://)
//! <path>         := '/' <any-char>*
//! ```
//!
//! Plus the sentinel `<all_urls>` matching any URL whose scheme is `http`,
//! `https`, `file`, `ftp`, or `urn`.
//!
//! Scheme wildcard `*` is restricted: it matches `http` **or** `https` only,
//! not `file` / `ftp` / other schemes. This matches Chromium's
//! `URLPattern::SCHEME_HTTP | SCHEME_HTTPS` default for bare `*://`.
//!
//! Host wildcard `*` matches any host (when bare), or `*.<domain>` matches the
//! domain and any subdomain (including the bare domain itself — e.g.
//! `*.google.com` matches `google.com`, `www.google.com`, `a.b.google.com`).
//!
//! Path wildcard `*` matches any sequence (including empty). No other
//! metacharacters are honored in the path.
//!
//! Ports are not part of the Chrome match-pattern grammar — a pattern matches
//! regardless of the URL's port. A pattern string containing a `:` in the
//! host position is rejected.

use crate::{Error, Result};

/// Special sentinel pattern string. When parsed, matches any URL whose scheme
/// is one of `http`, `https`, `file`, `ftp`, `urn`.
pub const ALL_URLS: &str = "<all_urls>";

/// Schemes `<all_urls>` matches, in Chromium's canonical order.
const ALL_URLS_SCHEMES: &[&str] = &["http", "https", "file", "ftp", "urn"];

/// Schemes bare `*://` matches.
const STAR_SCHEMES: &[&str] = &["http", "https"];

/// Schemes accepted as literal in a match pattern.
const VALID_SCHEMES: &[&str] = &[
    "http",
    "https",
    "file",
    "ftp",
    "urn",
    "chrome-extension",
    "data",
];

/// A compiled Chrome match pattern.
#[derive(Debug, Clone)]
pub struct MatchPattern {
    /// Original pattern string, retained for diagnostics and round-tripping.
    raw: String,
    inner: Kind,
}

#[derive(Debug, Clone)]
enum Kind {
    /// The `<all_urls>` sentinel.
    AllUrls,
    /// A parsed `<scheme>://<host>/<path>` pattern.
    Standard {
        scheme: SchemeMatch,
        host: HostMatch,
        path: PathMatch,
    },
}

#[derive(Debug, Clone)]
enum SchemeMatch {
    /// `*` — matches http or https only.
    AnyHttp,
    /// Literal scheme (lowercased).
    Literal(String),
}

#[derive(Debug, Clone)]
enum HostMatch {
    /// Host is irrelevant to matching (e.g. `file://` has no host).
    None,
    /// Bare `*`.
    Any,
    /// `*.<suffix>` — matches `suffix` and any subdomain of it.
    Subdomains(String),
    /// Literal hostname (lowercased).
    Literal(String),
}

#[derive(Debug, Clone)]
struct PathMatch {
    /// Path segments split on `*`. `parts.len() == stars + 1`. Matching is
    /// anchored: the URL path must start with `parts[0]`, end with
    /// `parts[parts.len()-1]`, and contain each middle segment in order.
    parts: Vec<String>,
}

impl MatchPattern {
    /// Parse a pattern string into a compiled matcher.
    pub fn parse(pattern: &str) -> Result<Self> {
        if pattern.is_empty() {
            return Err(err(pattern, "empty pattern"));
        }

        if pattern == ALL_URLS {
            return Ok(Self {
                raw: pattern.to_string(),
                inner: Kind::AllUrls,
            });
        }

        // Split scheme off at the first `://`.
        let sep = pattern
            .find("://")
            .ok_or_else(|| err(pattern, "missing '://' separator"))?;
        let scheme_str = &pattern[..sep];
        let rest = &pattern[sep + 3..];

        let scheme = parse_scheme(pattern, scheme_str)?;

        // Split host / path at the first `/`.
        let (host_str, path_str) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => return Err(err(pattern, "missing path (pattern must include at least '/')")),
        };

        // `file://`, `data:`, and `urn:` patterns have an empty host component.
        // (url::Url normalizes `file://localhost/...` to host=None, so the two
        // forms are indistinguishable at match time.)
        let expects_empty_host = matches!(
            &scheme,
            SchemeMatch::Literal(s) if s == "file" || s == "data" || s == "urn"
        );
        let host = parse_host(pattern, host_str, expects_empty_host)?;
        let path = parse_path(path_str);

        Ok(Self {
            raw: pattern.to_string(),
            inner: Kind::Standard {
                scheme,
                host,
                path,
            },
        })
    }

    /// Is this the `<all_urls>` sentinel?
    pub fn is_all_urls(&self) -> bool {
        matches!(self.inner, Kind::AllUrls)
    }

    /// Returns the original pattern string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Test whether this pattern matches the given URL.
    pub fn matches(&self, url: &url::Url) -> bool {
        match &self.inner {
            Kind::AllUrls => ALL_URLS_SCHEMES.contains(&url.scheme()),
            Kind::Standard {
                scheme,
                host,
                path,
            } => scheme_matches(scheme, url.scheme())
                && host_matches(host, url)
                && path_matches(path, url),
        }
    }

    /// Convenience: parse the URL string and match.
    ///
    /// Returns `false` if the URL does not parse, rather than propagating the
    /// error — matcher callers uniformly treat "unparseable URL" as "no match".
    pub fn matches_str(&self, url: &str) -> bool {
        match url::Url::parse(url) {
            Ok(u) => self.matches(&u),
            Err(_) => false,
        }
    }
}

/// A set of match patterns. Matches if **any** contained pattern matches.
#[derive(Debug, Clone, Default)]
pub struct MatchPatternSet {
    patterns: Vec<MatchPattern>,
}

impl MatchPatternSet {
    /// Wrap an iterator of already-parsed patterns.
    pub fn new(patterns: impl IntoIterator<Item = MatchPattern>) -> Self {
        Self {
            patterns: patterns.into_iter().collect(),
        }
    }

    /// Parse many pattern strings at once; fails on the first malformed one.
    pub fn parse_many<S: AsRef<str>>(patterns: impl IntoIterator<Item = S>) -> Result<Self> {
        let mut v = Vec::new();
        for p in patterns {
            v.push(MatchPattern::parse(p.as_ref())?);
        }
        Ok(Self { patterns: v })
    }

    /// Match the URL against every contained pattern; returns true on first hit.
    pub fn matches(&self, url: &url::Url) -> bool {
        self.patterns.iter().any(|p| p.matches(url))
    }

    /// Number of patterns in the set.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Is the set empty?
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Borrow the underlying patterns.
    pub fn patterns(&self) -> &[MatchPattern] {
        &self.patterns
    }
}

fn err(pattern: &str, reason: impl Into<String>) -> Error {
    Error::MatchPattern {
        pattern: pattern.to_string(),
        reason: reason.into(),
    }
}

fn parse_scheme(full: &str, raw: &str) -> Result<SchemeMatch> {
    if raw.is_empty() {
        return Err(err(full, "empty scheme"));
    }
    if raw == "*" {
        return Ok(SchemeMatch::AnyHttp);
    }
    let lower = raw.to_ascii_lowercase();
    if VALID_SCHEMES.contains(&lower.as_str()) {
        Ok(SchemeMatch::Literal(lower))
    } else {
        Err(err(full, format!("unsupported scheme '{}'", raw)))
    }
}

fn parse_host(full: &str, raw: &str, expects_empty: bool) -> Result<HostMatch> {
    if expects_empty {
        // Canonical: `file:///path` parses to host="".
        if raw.is_empty() {
            return Ok(HostMatch::None);
        }
        // Chrome real-world accepts `file://*/*` as shorthand for "any file
        // path" — matches the empty host on every file URL. Phantom's
        // content_scripts use this form. Accept `*` for file/data/urn the
        // same way the canonical empty host would match.
        if raw == "*" {
            return Ok(HostMatch::None);
        }
        return Err(err(
            full,
            "this scheme requires an empty host or '*' (file / data / urn)",
        ));
    }

    if raw.is_empty() {
        return Err(err(full, "empty host"));
    }

    // Ports are not part of the Chrome match-pattern grammar.
    if raw.contains(':') {
        return Err(err(full, "ports are not allowed in match patterns"));
    }

    // Paths shouldn't appear here; splitter handled that already.
    if raw.contains('/') {
        return Err(err(full, "malformed host"));
    }

    if raw == "*" {
        return Ok(HostMatch::Any);
    }

    if let Some(suffix) = raw.strip_prefix("*.") {
        if suffix.is_empty() || suffix.contains('*') {
            return Err(err(full, "invalid host wildcard"));
        }
        return Ok(HostMatch::Subdomains(suffix.to_ascii_lowercase()));
    }

    // Any `*` appearing elsewhere in the host is illegal.
    if raw.contains('*') {
        return Err(err(
            full,
            "host wildcard '*' may only appear as '*' or a leading '*.'",
        ));
    }

    Ok(HostMatch::Literal(raw.to_ascii_lowercase()))
}

fn parse_path(raw: &str) -> PathMatch {
    // Split on `*` — each `*` in the pattern becomes a `.*` in matching logic.
    // We don't escape anything else: path matching is done as a literal-string
    // walk against the URL's path, so regex metachars are never interpreted.
    let parts: Vec<String> = raw.split('*').map(|s| s.to_string()).collect();
    PathMatch { parts }
}

fn scheme_matches(m: &SchemeMatch, actual: &str) -> bool {
    match m {
        SchemeMatch::AnyHttp => STAR_SCHEMES.contains(&actual),
        SchemeMatch::Literal(s) => s == actual,
    }
}

fn host_matches(m: &HostMatch, url: &url::Url) -> bool {
    match m {
        HostMatch::None => {
            // file:// may parse to host == Some("") on some inputs; treat any
            // empty / None host as a match and a non-empty host as non-match.
            match url.host_str() {
                None => true,
                Some(h) => h.is_empty(),
            }
        }
        HostMatch::Any => url.host_str().is_some(),
        HostMatch::Subdomains(suffix) => {
            let Some(host) = url.host_str() else {
                return false;
            };
            let host = host.to_ascii_lowercase();
            // `*.google.com` matches `google.com`, `www.google.com`,
            // `a.b.google.com`, but not `googleblog.com` or `notgoogle.com`.
            if host == *suffix {
                return true;
            }
            host.ends_with(&format!(".{}", suffix))
        }
        HostMatch::Literal(h) => match url.host_str() {
            Some(actual) => actual.eq_ignore_ascii_case(h),
            None => false,
        },
    }
}

fn path_matches(m: &PathMatch, url: &url::Url) -> bool {
    // Include the query string so patterns like `/foo?*` can match it; this
    // matches Chrome's behavior: the match pattern's path section is checked
    // against the URL's path + (if present) `?` + query.
    let actual = match url.query() {
        Some(q) => format!("{}?{}", url.path(), q),
        None => url.path().to_string(),
    };

    let parts = &m.parts;
    // Single part means no `*`: must equal exactly.
    if parts.len() == 1 {
        return parts[0] == actual;
    }

    // First part must be a prefix; last part must be a suffix; middle parts
    // must appear in order (non-overlapping) between them.
    let mut remaining: &str = actual.as_str();
    let first = parts.first().unwrap();
    if !remaining.starts_with(first) {
        return false;
    }
    remaining = &remaining[first.len()..];

    let last = parts.last().unwrap();

    // Middle parts (strictly interior): each must be findable in order.
    // Edge case: parts.len() == 2 → no middle parts, just first + last.
    for mid in &parts[1..parts.len() - 1] {
        if mid.is_empty() {
            // Empty middle from `**` in the pattern — effectively a no-op
            // since any position satisfies "match empty".
            continue;
        }
        match remaining.find(mid.as_str()) {
            Some(idx) => {
                remaining = &remaining[idx + mid.len()..];
            }
            None => return false,
        }
    }

    // Finally, the last part must be a suffix of what's left.
    remaining.ends_with(last)
}
