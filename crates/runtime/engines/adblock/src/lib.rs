//! The `WebView`'s filter-list compiler, as its own library in the runtime bundle
//! (`libtilo_adblock_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

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
/// compiler, its type checked here against the toolkit's [`CompileFn`].
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "Rust" fn tilo_adblock_engine_entry() -> CompileFn {
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

/// The engine's self-check ([`self_check`]) for its check binary, by the
/// unmangled name the binary links, as the toolkit
/// opens [`tilo_adblock_engine_entry`]: nothing compiles against this library, so
/// it exports no Rust item. Prints the report; whether it passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_adblock_engine_check() -> bool {
    match self_check() {
        Ok(report) => {
            println!("tilo-adblock-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-adblock-engine: {error}");
            false
        }
    }
}

/// Run the engine's own code: compile a two-rule list through the entry the
/// toolkit opens, and check what came out.
///
/// The check binary runs it through its unmangled entry, so a release proves
/// the library loads and runs against the runtime it ships with.
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
