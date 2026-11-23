//! Proves this engine loads and runs against the runtime it is built with.
//!
//! os/scripts/build-thin-apps.sh builds and runs it on every runtime build,
//! so the engine exists, and is known to resolve against this exact
//! `libruntime_dylib.so`, whichever apps the release selects. Parsing the
//! bundled fonts runs engine code (`typst-assets`, Typst's font parser) that
//! reaches into the runtime (`ttf-parser`, `std`).

use std::process::ExitCode;

fn main() -> ExitCode {
    let fonts = tilo_typst_engine::typst_fonts();
    if fonts.is_empty() {
        eprintln!("tilo-typst-engine: the engine parsed none of its bundled fonts");
        return ExitCode::FAILURE;
    }
    println!("tilo-typst-engine: {} bundled fonts", fonts.len());
    ExitCode::SUCCESS
}
