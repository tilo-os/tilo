//! Tilo's grammar set for syntax highlighting, as an engine library beside
//! the shared runtime (`libtilo_highlight_engine.so`).
//!
//! syntect, its regex engine (fancy-regex) and two-face's packed grammars are
//! a few megabytes of code and data, with tables the loader relocates. Only
//! apps that show code (Code Editor, Diffs, Git, Snippets, a code block in a
//! note) ever use them. Inside `libruntime_dylib.so` every app process mapped
//! and relocated them; here the toolkit opens them the first time a line of
//! code is highlighted (tilo-engine-loader), and no app names them. Typst's
//! engine library chains on this one for the same crates.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides. Outside
//! the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_highlight_grammars;

/// The entry the toolkit opens (`tilo_highlight::ENGINE_ENTRY`): the
/// grammar set. Its type is checked here against
/// `tilo_highlight::GrammarsEntry`.
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
///
/// # Errors
/// Names what the grammars classified wrongly.
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
