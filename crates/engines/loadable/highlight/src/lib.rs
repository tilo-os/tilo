//! Tilo's grammar set for syntax highlighting, as its own library in the runtime
//! bundle (`libtilo_highlight_engine.so`).
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
pub use tilo_highlight_grammars;

/// The entry the toolkit opens (`tilo_highlight::ENGINE_ENTRY`): the grammar
/// set, its type checked here against `tilo_highlight::GrammarsEntry`.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub fn tilo_highlight_engine_entry() -> &'static dyn tilo_highlight::Grammars {
    tilo_highlight_grammars::grammars()
}

#[cfg(feature = "thin")]
const _: tilo_highlight::GrammarsEntry = tilo_highlight_engine_entry;

/// Run the engine's own code: highlight a line of Rust through the entry the
/// toolkit opens, and a block comment across two lines.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use tilo_highlight::TokenKind;

    let grammars = tilo_highlight_engine_entry();
    let rust = grammars
        .find("rs")
        .ok_or_else(|| "no grammar for rs".to_string())?;
    let mut state = grammars.start(rust);
    let line = "fn main() { let answer = 42; } /* open";
    let spans = state.highlight_line(line);
    let kind_of = |text: &str| {
        spans
            .iter()
            .find(|span| line.get(span.range.clone()) == Some(text))
            .map(|span| span.kind)
    };
    if kind_of("fn") != Some(TokenKind::Keyword) || kind_of("42") != Some(TokenKind::Number) {
        return Err(format!("{} classified {spans:?}", grammars.name(rust)));
    }
    let next = state.highlight_line("still a comment */");
    if next.first().map(|span| span.kind) != Some(TokenKind::Comment) {
        return Err(format!("a block comment did not carry: {next:?}"));
    }
    Ok(format!("{} highlighted", grammars.name(rust)))
}
