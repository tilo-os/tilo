//! Filter-list → `WebKit` content-blocker compiler for the `WebView`'s
//! built-in ad blocking (feature `webview-adblock`).
//!
//! Takes Brave/EasyList-syntax filter lists (the same lists Brave ships)
//! and compiles them via adblock-rust's `content_blocking` support into
//! the JSON rule format `WebKit`'s `UserContentFilterStore` consumes.
//! Blocking then happens natively inside `WebKit`'s network process —
//! there is no per-request Rust hook (`WebKit` exposes none to embedders),
//! which is also why this is faster than extension-based blockers.
//!
//! Fetching and caching the list text is the caller's job (the browser
//! app fetches with its own HTTP stack and disk cache); this module is
//! pure text → JSON.
//!
//! The compiler itself (`tilo-content-blocker`, adblock-rust) is not part of
//! the shared runtime every app maps. With `lazy-adblock` (the runtime's
//! build, turned on by the engine library's own build) it comes from
//! `libtilo_adblock_engine.so`, opened the first time an app compiles lists;
//! otherwise it is linked.

use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum AdblockError {
    #[error("filter lists produced no convertible rules")]
    Empty,
    #[error("content-blocker conversion failed")]
    Conversion,
    #[error("serialising content-blocker rules failed: {0}")]
    Serialise(#[from] serde_json::Error),
    /// The compiler's engine library could not be opened.
    #[error("the content-blocker compiler is unavailable: {0}")]
    Unavailable(String),
}

/// `WebKit` rejects a single content-rule list over this many rules with
/// "Too many rules in JSON array". Its documented cap is higher, but a
/// conservative split keeps every chunk comfortably valid.
pub const MAX_RULES_PER_FILTER: usize = 25_000;

/// The full `EasyList` + uBlock + `AdGuard` set is far larger than `WebKit`'s
/// per-list rule cap, so it must be installed as several content filters
/// rather than one giant array (which `WebKit` refuses to compile).
pub struct ChunkedFilter {
    /// Each element a JSON array of at most [`MAX_RULES_PER_FILTER`]
    /// rules — install each as its own `WebKit` content filter.
    pub chunks: Vec<std::sync::Arc<String>>,
    pub rule_count: usize,
    pub skipped: usize,
}

/// The compiler `libtilo_adblock_engine.so` hands over: what
/// [`compile_filter_lists_chunked`] does, over a slice of lists.
pub type CompileFn = fn(&[&str], usize) -> Result<ChunkedFilter, AdblockError>;

/// The entry `libtilo_adblock_engine.so` exports, returning its [`CompileFn`].
pub type EngineEntry = fn() -> CompileFn;

/// The engine library and its entry.
pub const ENGINE_LIBRARY: &str = "libtilo_adblock_engine.so";
pub const ENGINE_ENTRY: &str = "tilo_adblock_engine_entry";

/// Compile `lists` into `WebKit` content-blocker JSON, split into chunks of
/// at most `max_rules_per_chunk` rules each.
///
/// # Errors
///
/// Returns [`AdblockError::Conversion`] if adblock-rust's content-blocking
/// conversion fails, [`AdblockError::Empty`] if `lists` produces no rule
/// convertible to `WebKit`'s declarative format,
/// [`AdblockError::Serialise`] if a chunk fails to serialise to JSON, and
/// [`AdblockError::Unavailable`] if the compiler's engine library does not
/// open.
pub fn compile_filter_lists_chunked<'a>(
    lists: impl IntoIterator<Item = &'a str>,
    max_rules_per_chunk: usize,
) -> Result<ChunkedFilter, AdblockError> {
    let lists: Vec<&str> = lists.into_iter().collect();
    compiler()?(&lists, max_rules_per_chunk)
}

/// The compiler, linked.
#[cfg(not(feature = "lazy-adblock"))]
fn compiler() -> Result<CompileFn, AdblockError> {
    Ok(linked)
}

/// The compiler, from its engine library: opened the first time, kept.
#[cfg(feature = "lazy-adblock")]
fn compiler() -> Result<CompileFn, AdblockError> {
    static COMPILER: std::sync::OnceLock<CompileFn> = std::sync::OnceLock::new();
    if let Some(compile) = COMPILER.get() {
        return Ok(*compile);
    }
    // SAFETY: the engine's entry is checked against `EngineEntry` where it
    // is defined (crates/runtime/engines/adblock), in the same build.
    let entry = unsafe { tilo_engine_loader::entry::<EngineEntry>(ENGINE_LIBRARY, ENGINE_ENTRY) }
        .map_err(|error| AdblockError::Unavailable(error.to_string()))?;
    Ok(*COMPILER.get_or_init(entry))
}

/// [`CompileFn`] over the linked compiler.
#[cfg(not(feature = "lazy-adblock"))]
fn linked(lists: &[&str], max_rules_per_chunk: usize) -> Result<ChunkedFilter, AdblockError> {
    use tilo_content_blocker::Failure;
    match tilo_content_blocker::compile(lists, max_rules_per_chunk) {
        Ok(compiled) => Ok(chunked_filter(
            compiled.chunks,
            compiled.rule_count,
            compiled.skipped,
        )),
        Err(Failure::Empty) => Err(AdblockError::Empty),
        Err(Failure::Conversion) => Err(AdblockError::Conversion),
        Err(Failure::Serialise(error)) => Err(AdblockError::Serialise(error)),
    }
}

/// [`ChunkedFilter`] from the compiler's parts. The engine library maps the
/// compiler's result through this (the toolkit's runtime build names no
/// compiler type).
#[must_use]
pub fn chunked_filter(
    chunks: Vec<Arc<String>>,
    rule_count: usize,
    skipped: usize,
) -> ChunkedFilter {
    ChunkedFilter {
        chunks,
        rule_count,
        skipped,
    }
}

#[cfg(all(test, not(feature = "lazy-adblock")))]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
! Title: fixture list
||ads.example.com^
###ad-banner
";

    #[test]
    fn the_toolkit_api_compiles_through_the_linked_compiler() {
        let compiled = compile_filter_lists_chunked([FIXTURE], 10_000).expect("fixture compiles");
        assert!(compiled.rule_count >= 2, "got {}", compiled.rule_count);
        assert_eq!(compiled.chunks.len(), 1);
        assert!(matches!(
            compile_filter_lists_chunked([""], 10_000),
            Err(AdblockError::Empty)
        ));
    }
}
