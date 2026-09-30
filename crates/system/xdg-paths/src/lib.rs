//! Deprecated: this crate is now [`tilo_paths`], re-exported unchanged.
//!
//! Every item, including the [`xdg`] module, is the `tilo-paths` item of the
//! same name, so existing `xdg_paths::…` paths resolve to identical values.
//! New code depends on `tilo-paths` directly; once no dependent is left this
//! shim is deleted.

pub use tilo_paths::*;
