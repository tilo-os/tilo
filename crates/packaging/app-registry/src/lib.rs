//! `app-registry` — the Tilo app registry protocol, end to end.
//!
//! This crate provides the protocol that makes "open an app from the store"
//! fetch, verify, and cache a remote `.tapp`. It sits between the
//! packaging foundation (`pkg`: manifest, signing, verify) and the
//! two consumers that drive it:
//!
//! - the app-host daemon, whose remote-fetch calls
//!   [`resolve::fetch_tapp`] and hands the verified bytes to its
//!   installer;
//! - `tilo-app-package publish`, which uses [`publish`] to sign an index entry,
//!   upload the blob, and emit the entry to a registry.
//!
//! ## Shape of the protocol
//!
//! - [`index`] — the **signed registry index**: `app-id → versions`,
//!   each version carrying its tapp blake3 content-hash, publisher
//!   identity + signature, target arch/ISA, and runtime ABI ref. The
//!   whole document is signed by the registry operator; each entry is
//!   signed by the app publisher. Served as static JSON over HTTPS
//!   (self-hostable).
//! - [`config`] — `registries.toml`: the global + per-workspace list
//!   of registries, each with a `base-url` and a trust level.
//! - [`transport`] — content transport over **iroh-blobs**, keyed by
//!   blake3. The index carries hashes + tickets, never bytes, so a
//!   blob is downloaded once and deduped across every app.
//! - [`resolve`] — the fail-closed resolve → download → verify path.
//! - [`publish`] — the publish side of the same protocol.
//! - [`registry_dir`] — a `file://` registry directory (the
//!   self-hosted reference: `index.json` + an iroh-blobs `blobs/`
//!   store).
//! - [`tapp`] — read + fail-closed verify of `.tapp` tar bytes.
//!
//! ## Trust, in one line
//!
//! Bytes are **never** installed unverified: the registry index is
//! signed by the operator (pinned key), each entry is signed by the
//! publisher, the downloaded blob must match the advertised blake3,
//! AND the publisher signature embedded in the tapp must validate
//! against the entry's publisher. Any mismatch aborts the fetch.

pub mod config;
pub mod http_blobs;
pub mod index;
pub mod publish;
pub mod registry_dir;
pub mod resolve;
pub mod tapp;
pub mod transport;

pub use config::{RegistriesConfig, RegistryRef, RegistryTrust};
pub use http_blobs::HttpBlobs;
pub use index::{HostProfile, IndexEntry, IndexError, RegistryIndex, RuntimeRef};
pub use publish::{PublishReceipt, build_entry, publish_to_dir};
pub use resolve::{FetchOutcome, fetch_tapp, resolve_and_fetch};
pub use tapp::{TappMeta, validate_payload_shape, validate_payload_shape_with, verify_bytes};
pub use tilo_app_package::{MAX_APP_TAPP_BYTES, tapp_size_allowed};
pub use transport::{BlobNode, ProviderBlobs, client_endpoint, fetch_blob};
