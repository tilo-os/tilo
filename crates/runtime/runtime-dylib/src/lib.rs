//! `runtime-dylib` — the Tilo runtime SDK and UI toolkit, packaged as a
//! single shared object (`libruntime_dylib.so`).
//!
//! This crate carries **no logic of its own** — it re-exports the SDK
//! surfaces a Tilo app links against ([`cli`], [`ui`], [`tilo_app`]) so that
//! building it as a `dylib` pulls their whole transitive graph (portal /
//! collab / fs client / identity, and for `ui` the wgpu + vello + cosmic-text +
//! automerge + taffy + fluent + winit stack) into one shared library.
//!
//! `ui` is why this earns its keep: a statically linked graphical app is
//! ~24 MB, ~18 MB of it toolkit `.text` carried again by every single app.
//! Linked out of here it exists once on disk, in the content-addressed
//! runtime bundle.
//!
//! A thin app depends on `runtime-dylib` and compiles with
//! `-C prefer-dynamic --profile thin-app`; because this dylib is built
//! from the *same* sources in the *same* build unit, rustc satisfies those
//! references out of the dylib instead of statically linking the rlibs, and
//! links libstd dynamically too — the app binary holds only its own code.
//! Apps that skip the shared build keep depending on the same crates
//! directly and are unaffected.
//!
//! Every app process maps this library, so it carries what apps share and
//! never an OS daemon or other OS-internal crate: no FUSE filesystem
//! (`tilo-fs`), sandbox launcher, app host or install CLI. An app that calls
//! into one of those links it statically and gets only the code it reaches;
//! `tools/policy/check_runtime_dylib_surface.py` keeps them out of this graph.
//! Nor does it carry an engine only a few apps use once that engine has a
//! library of its own beside this one (`crates/runtime/engines/`).

// `pub use`, not a bare `extern crate`: that's what keeps the symbols and
// records these crates as provided-by-this-dylib in the metadata, which
// the downstream `-C prefer-dynamic` link step consumes.
pub use accounts;
pub use agent_provider_config;
pub use cli;
pub use collab_types;
pub use desktop_entry;
pub use desktop_policy;
pub use media;
pub use os_actions;
pub use registry_types;
pub use roxmltree;
pub use runtime_fs;
pub use runtime_identity;
pub use settings_store;
pub use thumbnail;
pub use tilo_actions;
pub use tilo_app;
pub use tilo_bluez;
pub use tilo_doc_codecs;
pub use tilo_document;
pub use tilo_files;
pub use tilo_mime;
pub use tilo_model_client;
pub use tilo_model_client as tilo_model_gateway;
pub use tilo_relay_protocol;
pub use tilo_rich_text;
pub use tilo_user_identity;
pub use ui;
pub use vte;
pub use xattr;
