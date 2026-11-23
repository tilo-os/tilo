//! `app-registry`: the Tilo app registry protocol, both sides.
//!
//! Registries are HTTP servers: signed entries at `/entries`, packages and payload blobs beside
//! them. The reader side, which every party reading a registry's bytes uses, is [`tapp`] (fail-closed
//! verification against the publisher and package id a signed entry names) and [`http_blobs`] (the
//! payload blobs of a thin package, each checked against its hash). The publisher side is
//! [`publisher`]: keys, signed entries, and the registry's HTTP API for uploads, entries and
//! publisher applications.
//!
//! Which registries a machine reads, and what they offer it, is the store client's (`app-cli`): its
//! `registries.toml` and catalogue are the one resolve path `app update` and `tilo-install update`
//! both take.

pub mod http_blobs;
pub mod publisher;
pub mod tapp;

pub use http_blobs::HttpBlobs;
pub use tapp::{TappMeta, validate_payload_shape, validate_payload_shape_with, verify_bytes};
pub use tilo_app_package::{MAX_APP_TAPP_BYTES, tapp_size_allowed};
