//! `runtime-dylib` — the qOS runtime SDK, packaged as a single shared
//! object (`libruntime_dylib.so`).
//!
//! This crate carries **no logic of its own**. It re-exports the two
//! SDK surfaces a qOS app links against — the [`runtime`] umbrella and
//! the [`cli`] helpers — so that building this crate as a `dylib` pulls
//! their whole transitive graph (portal / collab / fs / identity, plus
//! their deps) into one shared library.
//!
//! ## How a thin app uses it
//!
//! A thin app depends on `runtime-dylib` and compiles with
//! `-C prefer-dynamic --profile thin-app`. Because this dylib was built
//! from the *same* `runtime` / `cli` sources in the *same* build unit,
//! rustc satisfies the app's `runtime` / `cli` references out of the
//! dylib rather than statically linking the rlibs — and links libstd
//! dynamically too. The app binary then holds only its own code; the
//! SDK + std ship once, in the content-addressed runtime bundle.
//!
//! Apps that don't want the shared build keep depending on `runtime` /
//! `cli` directly and are entirely unaffected — this crate is invisible
//! to them.

// Re-export the SDK crates so the dylib links (and therefore *contains*)
// their full graphs. `pub use` — rather than a bare `extern crate` — is
// what guarantees the symbols are kept and the metadata records these
// crates as provided-by-this-dylib, which is exactly what the
// downstream `-C prefer-dynamic` link step consumes.
pub use cli;
pub use runtime;
