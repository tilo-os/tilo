//! Registry payload transport and `.tapp` verification.
//!
//! A registry advertises signed package metadata; the bytes themselves are
//! content addressed. This crate holds the pieces every registry client and
//! the self-hostable server share that are about those bytes, not about the
//! catalogue:
//!
//! - [`http_blobs`] — payload blobs from a registry server over HTTPS,
//!   including its compressed twins and chunked objects.
//! - [`local_blobs`] — payload blobs from a registry directory on this
//!   machine.
//! - [`blobs`] — either of the two, chosen by a registry's base URL.
//! - [`tapp`] — read and fail-closed verify `.tapp` bytes on the way in or
//!   out of a registry.
//!
//! The catalogue protocol itself — signed snapshots, version indexes,
//! publisher authorization, owner pins, resolution and publishing — lives in
//! `registry-types`, `registry-client`, `registry-server` and the `app`
//! command. This crate deliberately defines no second index.

pub mod blobs;
pub mod http_blobs;
pub mod local_blobs;
pub mod tapp;

pub use blobs::RegistryBlobs;
pub use http_blobs::HttpBlobs;
pub use local_blobs::LocalBlobs;
pub use tapp::{TappMeta, validate_payload_shape, validate_payload_shape_with, verify_bytes};
pub use tilo_app_package::{MAX_APP_TAPP_BYTES, tapp_size_allowed};
