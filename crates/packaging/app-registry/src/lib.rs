//! `app-registry` — the client edges of the Tilo app registry.
//!
//! Registries are HTTP registry servers (`registry-server`): signed entries
//! at `/entries`, packages and payload blobs beside them. This crate holds
//! the two pieces every party that reads a registry's bytes shares — the
//! server reviewing an upload, the installer, the room host:
//!
//! - [`tapp`] — read + fail-closed verify of `.tapp` bytes against the
//!   publisher and package id a signed entry names.
//! - [`http_blobs`] — payload blobs from a registry server, each checked
//!   against its hash.
//!
//! Which registries a machine reads, and what they offer it, is the store
//! client's (`app-cli`): its `registries.toml` and catalogue are the one
//! resolve path `app update` and `tilo-install update` both take.

pub mod http_blobs;
pub mod tapp;

pub use http_blobs::HttpBlobs;
pub use tapp::{TappMeta, validate_payload_shape, validate_payload_shape_with, verify_bytes};
pub use tilo_app_package::{MAX_APP_TAPP_BYTES, tapp_size_allowed};
