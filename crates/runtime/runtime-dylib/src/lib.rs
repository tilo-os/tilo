//! The Tilo runtime SDK and UI toolkit as one shared object
//! (`libruntime_dylib.so`): apps built with `-C prefer-dynamic --profile
//! thin-app` link the toolkit once instead of carrying it each.
//!
//! Every app process maps this library, so it takes what apps share and no
//! OS-internal crate: no FUSE filesystem, sandbox launcher, app host or
//! install CLI. `tools/policy/check_runtime_dylib_surface.py` keeps them out.

// `pub use`, not a bare `extern crate`: this is what records the crates as
// provided by this dylib, which the `-C prefer-dynamic` link step reads.
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
