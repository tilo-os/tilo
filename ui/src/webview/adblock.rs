//! Filter-list → WebKit content-blocker compiler for the WebView's
//! built-in ad blocking (feature `webview-adblock`).
//!
//! Takes Brave/EasyList-syntax filter lists (the same lists Brave ships)
//! and compiles them via adblock-rust's `content_blocking` support into
//! the JSON rule format WebKit's `UserContentFilterStore` consumes.
//! Blocking then happens natively inside WebKit's network process —
//! there is no per-request Rust hook (WebKit exposes none to embedders),
//! which is also why this is faster than extension-based blockers.
//!
//! Fetching and caching the list text is the caller's job (the browser
//! app fetches with its own HTTP stack and disk cache); this module is
//! pure text → JSON.

use adblock::FilterSet;
use adblock::lists::ParseOptions;

/// Result of compiling one or more filter lists.
pub struct CompiledFilter {
    /// JSON array of WebKit content-blocker rules, ready for
    /// `WebViewCommand::SetContentFilter` /
    /// `webkit_user_content_filter_store_save()`.
    pub json: std::sync::Arc<String>,
    /// Number of content-blocker rules emitted.
    pub rule_count: usize,
    /// Filters that had no content-blocker equivalent and were skipped
    /// (WebKit's declarative format cannot express every EasyList
    /// construct — scriptlets, some regex filters, `$redirect`, …).
    pub skipped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AdblockError {
    #[error("filter lists produced no convertible rules")]
    Empty,
    #[error("content-blocker conversion failed")]
    Conversion,
    #[error("serialising content-blocker rules failed: {0}")]
    Serialise(#[from] serde_json::Error),
}

/// Compile filter-list text(s) into WebKit content-blocker JSON.
pub fn compile_filter_lists<'a>(
    lists: impl IntoIterator<Item = &'a str>,
) -> Result<CompiledFilter, AdblockError> {
    // `true` = keep the parsed filters' debug info — the content-blocking
    // conversion refuses to run without it (it needs the raw filter text).
    let mut set = FilterSet::new(true);
    for list in lists {
        set.add_filter_list(list, ParseOptions::default());
    }
    let (rules, skipped) = set
        .into_content_blocking()
        .map_err(|_| AdblockError::Conversion)?;
    if rules.is_empty() {
        return Err(AdblockError::Empty);
    }
    let rule_count = rules.len();
    let json = serde_json::to_string(&rules)?;
    Ok(CompiledFilter {
        json: std::sync::Arc::new(json),
        rule_count,
        skipped: skipped.len(),
    })
}

/// WebKit rejects a single content-rule list over this many rules with
/// "Too many rules in JSON array". Its documented cap is higher, but a
/// conservative split keeps every chunk comfortably valid and still lets
/// us ship the full multi-list rule set across several filters.
pub const MAX_RULES_PER_FILTER: usize = 25_000;

/// A rule set compiled into one *or more* content-blocker JSON blobs.
/// The full EasyList + uBlock + AdGuard set is far larger than WebKit's
/// per-list rule cap, so it must be installed as several content filters
/// rather than one giant array (which WebKit refuses to compile).
pub struct ChunkedFilter {
    /// Each element is a JSON array of at most [`MAX_RULES_PER_FILTER`]
    /// rules — install each as its own WebKit content filter.
    pub chunks: Vec<std::sync::Arc<String>>,
    /// Total rules across all chunks.
    pub rule_count: usize,
    /// Filters with no content-blocker equivalent (see [`CompiledFilter`]).
    pub skipped: usize,
}

/// Compile filter-list text(s) into content-blocker JSON split into
/// chunks of at most `max_rules_per_chunk` rules, so each chunk stays
/// within WebKit's per-list limit.
pub fn compile_filter_lists_chunked<'a>(
    lists: impl IntoIterator<Item = &'a str>,
    max_rules_per_chunk: usize,
) -> Result<ChunkedFilter, AdblockError> {
    let mut set = FilterSet::new(true);
    for list in lists {
        set.add_filter_list(list, ParseOptions::default());
    }
    let (rules, skipped) = set
        .into_content_blocking()
        .map_err(|_| AdblockError::Conversion)?;
    if rules.is_empty() {
        return Err(AdblockError::Empty);
    }
    let rule_count = rules.len();
    let max = max_rules_per_chunk.max(1);
    let mut chunks = Vec::new();
    for slice in rules.chunks(max) {
        chunks.push(std::sync::Arc::new(serde_json::to_string(slice)?));
    }
    Ok(ChunkedFilter {
        chunks,
        rule_count,
        skipped: skipped.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
! Title: fixture list
||ads.example.com^
||tracker.test^$third-party
###ad-banner
example.org##.sponsored
";

    #[test]
    fn compiles_network_and_cosmetic_rules_to_json() {
        let compiled = compile_filter_lists([FIXTURE]).expect("fixture compiles");
        assert!(compiled.rule_count >= 3, "got {}", compiled.rule_count);

        let parsed: serde_json::Value =
            serde_json::from_str(&compiled.json).expect("valid JSON");
        let rules = parsed.as_array().expect("JSON array of rules");
        assert_eq!(rules.len(), compiled.rule_count);

        // A blocking rule for ads.example.com exists…
        assert!(
            rules.iter().any(|r| {
                r["action"]["type"] == "block"
                    && r["trigger"]["url-filter"]
                        .as_str()
                        .is_some_and(|f| f.contains("ads\\.example\\.com"))
            }),
            "expected a block rule for ads.example.com in {}",
            compiled.json
        );
        // …and the cosmetic filter becomes a css-display-none rule.
        assert!(
            rules.iter().any(|r| {
                r["action"]["type"] == "css-display-none"
                    && r["action"]["selector"]
                        .as_str()
                        .is_some_and(|s| s.contains("#ad-banner"))
            }),
            "expected a css-display-none rule for #ad-banner",
        );
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(matches!(
            compile_filter_lists([""]),
            Err(AdblockError::Empty)
        ));
    }

    #[test]
    fn chunking_splits_rules_across_multiple_filters() {
        // The fixture yields several rules; a tiny cap must split them into
        // more than one chunk, each a valid JSON array, together covering
        // exactly the full rule count.
        let compiled =
            compile_filter_lists_chunked([FIXTURE], 2).expect("fixture compiles");
        assert!(compiled.rule_count >= 3);
        assert!(
            compiled.chunks.len() >= 2,
            "expected multiple chunks, got {}",
            compiled.chunks.len()
        );
        let mut total = 0;
        for chunk in &compiled.chunks {
            let arr: serde_json::Value =
                serde_json::from_str(chunk).expect("each chunk is valid JSON");
            let n = arr.as_array().expect("array").len();
            assert!(n <= 2, "chunk exceeds the cap: {n}");
            total += n;
        }
        assert_eq!(total, compiled.rule_count, "chunks must cover every rule");
    }

    #[test]
    fn single_chunk_when_under_cap() {
        let compiled =
            compile_filter_lists_chunked([FIXTURE], 10_000).expect("fixture compiles");
        assert_eq!(compiled.chunks.len(), 1);
    }
}
