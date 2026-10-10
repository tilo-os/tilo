//! `runtime-dylib` — the Tilo runtime SDK and UI toolkit, packaged as a
//! single shared object (`libruntime_dylib.so`).
//!
//! This crate carries **no logic of its own** — it re-exports the SDK
//! surfaces a Tilo app links against ([`runtime`], [`cli`], [`ui`], [`tilo_app`]) so that
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub use account_custody;
pub use accounts;
pub use agent_provider_config;
pub use app_cli;
pub use app_host;
pub use async_imap;
pub use chrono_tz;
pub use cli;
pub use collab_types;
pub use comemo;
pub use desktop_entry;
pub use desktop_policy;
pub use fs;
pub use iroh;
pub use launcher;
pub use media;
pub use mime_guess;
pub use os_actions;
pub use registry_types;
pub use roxmltree;
pub use runtime;
pub use runtime_fs;
pub use runtime_identity;
pub use runtime_portal_xdg;
pub use rustypipe;
pub use settings_store;
pub use thumbnail;
pub use tilo_actions;
pub use tilo_app;
pub use tilo_bluez;
pub use tilo_callstream;
pub use tilo_files;
pub use tilo_model_gateway;
pub use tilo_oauth;
pub use tilo_pdf;
pub use tilo_relay_protocol;
pub use tilo_spotify;
pub use tilo_subject;
pub use tilo_user_identity;
pub use tilo_wet_media;
pub use tiny_skia;
pub use typst;
pub use typst_assets;
pub use typst_layout;
pub use typst_pdf;
pub use typst_render;
pub use ui;
pub use vte;
pub use xattr;

/// Parse the runtime-bundled Typst fonts without making each thin app retain
/// `typst-assets`' multi-megabyte static font table in its own executable.
///
/// Returning the concrete vector keeps iteration over the embedded byte table
/// on this side of the dylib boundary; thin apps receive only parsed fonts.
pub fn typst_fonts() -> Vec<typst::text::Font> {
    typst_assets::fonts()
        .flat_map(|data| typst::text::Font::iter(typst::foundations::Bytes::new(data)))
        .collect()
}
