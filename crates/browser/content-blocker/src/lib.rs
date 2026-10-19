//! Filter lists compiled into `WebKit` content-blocker rules.
//!
//! Takes Brave/EasyList-syntax filter lists (the same lists Brave ships) and
//! compiles them with adblock-rust's `content_blocking` support into the JSON
//! rule format `WebKit`'s `UserContentFilterStore` consumes. Blocking then
//! happens natively inside `WebKit`'s network process; there is no per-request
//! hook.
//!
//! The toolkit's `ui::webview::adblock` is the API apps call. This crate is
//! its implementation, kept free of the toolkit's types so that the toolkit
//! can either link it (a build without the shared runtime) or open it from
//! `libtilo_adblock_engine.so` the first time an app compiles lists, which
//! keeps adblock-rust out of every other app.

use std::sync::Arc;

use adblock::FilterSet;
use adblock::lists::ParseOptions;

/// What [`compile`] produced.
#[derive(Debug)]
pub struct Compiled {
    /// Each a JSON array of at most the requested number of rules.
    pub chunks: Vec<Arc<String>>,
    pub rule_count: usize,
    /// Rules with no `WebKit` equivalent.
    pub skipped: usize,
}

/// Why [`compile`] produced nothing.
#[derive(Debug)]
pub enum Failure {
    /// The lists hold no rule `WebKit`'s format can express.
    Empty,
    /// adblock-rust's conversion failed.
    Conversion,
    /// A chunk did not serialise.
    Serialise(serde_json::Error),
}

/// Compile `lists` into `WebKit` content-blocker JSON, split into chunks of
/// at most `max_rules_per_chunk` rules each (at least one).
///
/// # Errors
///
/// [`Failure::Conversion`] if adblock-rust's conversion fails,
/// [`Failure::Empty`] if `lists` produce no rule `WebKit` can express, and
/// [`Failure::Serialise`] if a chunk fails to serialise.
pub fn compile(lists: &[&str], max_rules_per_chunk: usize) -> Result<Compiled, Failure> {
    let mut set = FilterSet::new(true);
    for list in lists {
        set.add_filter_list(list, ParseOptions::default());
    }
    let (rules, skipped) = set
        .into_content_blocking()
        .map_err(|()| Failure::Conversion)?;
    if rules.is_empty() {
        return Err(Failure::Empty);
    }
    let rule_count = rules.len();
    let mut chunks = Vec::new();
    for slice in rules.chunks(max_rules_per_chunk.max(1)) {
        chunks.push(Arc::new(
            serde_json::to_string(slice).map_err(Failure::Serialise)?,
        ));
    }
    Ok(Compiled {
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
    fn adblock_rules_compile_to_chunked_json_filters() {
        // phase: compiles_network_and_cosmetic_rules_to_json
        {
            let compiled = compile(&[FIXTURE], 10_000).expect("fixture compiles");
            assert!(compiled.rule_count >= 3, "got {}", compiled.rule_count);
            assert_eq!(compiled.chunks.len(), 1, "under the cap is one filter");
            let json = &compiled.chunks[0];

            let parsed: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
            let rules = parsed.as_array().expect("JSON array of rules");
            assert_eq!(rules.len(), compiled.rule_count);

            assert!(
                rules.iter().any(|r| {
                    r["action"]["type"] == "block"
                        && r["trigger"]["url-filter"]
                            .as_str()
                            .is_some_and(|f| f.contains("ads\\.example\\.com"))
                }),
                "expected a block rule for ads.example.com in {json}"
            );
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
        // phase: empty_input_is_an_error
        {
            assert!(matches!(compile(&[""], 10_000), Err(Failure::Empty)));
        }
        // phase: chunking_splits_rules_across_multiple_filters
        {
            let compiled = compile(&[FIXTURE], 2).expect("fixture compiles");
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
    }
}
