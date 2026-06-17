//! Chrome-style glob patterns.
//!
//! Used by `content_scripts.include_globs` / `exclude_globs`. Chrome's glob
//! language is deliberately small:
//!
//! - `*` matches any sequence of any characters (including `/`).
//! - `?` matches exactly one arbitrary character.
//! - Every other character is literal.
//! - Anchored: the pattern must match the entire URL.
//!
//! We deliberately do **not** use the `globset` crate — its dialect supports
//! `**`, character classes, `{a,b}` braces, and other shell-style extensions
//! that Chrome's matcher does not. Instead we translate the pattern to an
//! anchored regex by escaping literal runs and substituting `*` → `.*`,
//! `?` → `.` (with the `s` flag so `.` matches newlines — Chrome treats
//! the URL as opaque bytes).

use regex::Regex;

use crate::{Error, Result};

/// A compiled Chrome-style glob.
#[derive(Debug, Clone)]
pub struct Glob {
    /// Original pattern, retained for diagnostics.
    raw: String,
    re: Regex,
}

impl Glob {
    /// Compile a glob pattern.
    pub fn parse(pattern: &str) -> Result<Self> {
        let mut regex_src = String::with_capacity(pattern.len() + 8);
        regex_src.push_str("(?s)\\A");
        let mut literal = String::new();
        for c in pattern.chars() {
            match c {
                '*' => {
                    if !literal.is_empty() {
                        regex_src.push_str(&regex::escape(&literal));
                        literal.clear();
                    }
                    regex_src.push_str(".*");
                }
                '?' => {
                    if !literal.is_empty() {
                        regex_src.push_str(&regex::escape(&literal));
                        literal.clear();
                    }
                    regex_src.push('.');
                }
                other => literal.push(other),
            }
        }
        if !literal.is_empty() {
            regex_src.push_str(&regex::escape(&literal));
        }
        regex_src.push_str("\\z");

        let re = Regex::new(&regex_src).map_err(|e| Error::MatchPattern {
            pattern: pattern.to_string(),
            reason: format!("regex compile failed: {e}"),
        })?;

        Ok(Self {
            raw: pattern.to_string(),
            re,
        })
    }

    /// Match a candidate URL (or any string) against the compiled glob.
    pub fn matches(&self, url: &str) -> bool {
        self.re.is_match(url)
    }

    /// The original pattern string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}
