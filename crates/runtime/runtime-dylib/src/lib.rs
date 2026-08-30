//! `runtime-dylib` — the qOS runtime SDK and UI toolkit, packaged as a
//! single shared object (`libruntime_dylib.so`).
//!
//! This crate carries **no logic of its own** — it re-exports the three
//! surfaces a qOS app links against ([`runtime`], [`cli`], [`ui`]) so that
//! building it as a `dylib` pulls their whole transitive graph (portal /
//! collab / fs / identity, and for `ui` the wgpu + vello + cosmic-text +
//! automerge + taffy + fluent + winit stack) into one shared library.
//!
//! `ui` is why this earns its keep: a statically linked graphical app is
//! ~24 MB, ~18 MB of it toolkit `.text` carried again by every single app.
//! Linked out of here it exists once on disk, in the content-addressed
//! runtime bundle.
//!
//! A thin app depends on `runtime-dylib` and compiles with
//! `-C prefer-dynamic --profile thin-app`; because this dylib is built
//! from the *same* `runtime`/`cli` sources in the *same* build unit, rustc
//! satisfies those references out of the dylib instead of statically
//! linking the rlibs, and links libstd dynamically too — the app binary
//! holds only its own code. Apps that skip the shared build keep depending
//! on `runtime`/`cli` directly and are unaffected.

// `pub use`, not a bare `extern crate`: that's what keeps the symbols and
// records these crates as provided-by-this-dylib in the metadata, which
// the downstream `-C prefer-dynamic` link step consumes.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub use cli;
pub use runtime;
pub use ui;
