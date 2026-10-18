//! The `WebView`'s filter-list compiler, as an engine library beside the
//! shared runtime (`libtilo_adblock_engine.so`).
//!
//! adblock-rust (with its regex and domain-resolution tables) compiles
//! Brave/EasyList filter lists into `WebKit` content-blocker rules. Only an
//! app that blocks ads in a `WebView` (the browser) ever does that, and only
//! when it refreshes its lists. Inside `libruntime_dylib.so` every app
//! process mapped and relocated it; here the toolkit opens it the first time
//! `ui::webview::adblock::compile_filter_lists_chunked` runs
//! (tilo-engine-loader), and no app names it.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides. Outside
//! the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_content_blocker;

#[cfg(feature = "thin")]
use tilo_ui::webview::adblock::{AdblockError, ChunkedFilter, CompileFn, EngineEntry};

/// The entry the toolkit opens (`ui::webview::adblock::ENGINE_ENTRY`): the
/// compiler, as the toolkit's [`CompileFn`]. Its type is checked here
/// against the toolkit's definition.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub fn tilo_adblock_engine_entry() -> CompileFn {
    compile
}

#[cfg(feature = "thin")]
const _: EngineEntry = tilo_adblock_engine_entry;

#[cfg(feature = "thin")]
fn compile(lists: &[&str], max_rules_per_chunk: usize) -> Result<ChunkedFilter, AdblockError> {
    use tilo_content_blocker::Failure;
    match tilo_content_blocker::compile(lists, max_rules_per_chunk) {
        Ok(compiled) => Ok(tilo_ui::webview::adblock::chunked_filter(
            compiled.chunks,
            compiled.rule_count,
            compiled.skipped,
        )),
        Err(Failure::Empty) => Err(AdblockError::Empty),
        Err(Failure::Conversion) => Err(AdblockError::Conversion),
        Err(Failure::Serialise(error)) => Err(AdblockError::Serialise(error)),
    }
}

/// Run the engine's own code: compile a two-rule list through the entry the
/// toolkit opens, and check what came out.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
///
/// # Errors
/// Names what the compiler produced wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let list = "||ads.example.com^\n###ad-banner\n";
    let compiled = tilo_adblock_engine_entry()(&[list], 1).map_err(|error| error.to_string())?;
    if compiled.rule_count < 2 || compiled.chunks.len() != compiled.rule_count {
        return Err(format!(
            "{} rules in {} chunks of one",
            compiled.rule_count,
            compiled.chunks.len()
        ));
    }
    if !compiled
        .chunks
        .iter()
        .any(|chunk| chunk.contains("ads\\\\.example\\\\.com"))
    {
        return Err("no rule blocks ads.example.com".to_string());
    }
    Ok(format!("{} rules compiled", compiled.rule_count))
}
