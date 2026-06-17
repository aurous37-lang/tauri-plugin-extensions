//! Chrome URL match-pattern and glob matchers.
//!
//! Owned by Agent B per `docs/spike-plan.md`. Two pattern languages, both
//! Chrome-faithful:
//!
//! - [`MatchPattern`] / [`MatchPatternSet`] — Chrome's `<scheme>://<host>/<path>`
//!   match-pattern grammar, plus the `<all_urls>` sentinel.
//! - [`Glob`] — Chrome's simpler glob used by `content_scripts.include_globs`
//!   and `exclude_globs`: `*` = any-sequence, `?` = any-single-char, anchored.
//!
//! Reference: <https://developer.chrome.com/docs/extensions/develop/concepts/match-patterns>.

mod glob;
mod url_pattern;

#[cfg(test)]
mod tests;

pub use glob::Glob;
pub use url_pattern::{MatchPattern, MatchPatternSet};
