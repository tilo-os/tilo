//! `app-registry` — the Tilo app registry protocol, both sides.
//!
//! Registries are HTTP registry servers (`registry-server`): signed entries
//! at `/entries`, packages and payload blobs beside them. This crate holds
//! every piece of that protocol a client of one shares, split by which side
//! of a publication it runs on:
//!
//! - the **reader** side — what every party that reads a registry's bytes
//!   uses: [`tapp`] reads + fail-closed verifies `.tapp` bytes against the
//!   publisher and package id a signed entry names, and [`http_blobs`]
//!   serves the payload blobs of a thin package, each checked against its
//!   hash;
//! - the **publisher** side — [`publisher`]: keys, signed entries, and the
//!   registry's HTTP API for uploads, entries and publisher applications.
//!   `tilo-app publish`, `tilo-app apply` and the App Publisher app use it;
//!   so can any CI that would rather not shell out.
//!
//! Which registries a machine reads, and what they offer it, is the store
//! client's (`app-cli`): its `registries.toml` and catalogue are the one
//! resolve path `app update` and `tilo-install update` both take.

pub mod http_blobs;
pub mod publisher;
pub mod tapp;

pub use http_blobs::HttpBlobs;
pub use tapp::{TappMeta, validate_payload_shape, validate_payload_shape_with, verify_bytes};
pub use tilo_app_package::{MAX_APP_TAPP_BYTES, tapp_size_allowed};
