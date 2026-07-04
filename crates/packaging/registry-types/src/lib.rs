//! Wire types for the qOS app registry.
//!
//! A registry advertises signed metadata for `.qpkg` packages —
//! `id`, `versions`, `publisher pubkey`, `name`, `description`,
//! icon / source / homepage URLs, and a BLAKE3 root hash so
//! downloaders can verify the bytes match what the registry
//! signed for. Multiple registries can coexist: qOS ships one
//! hosted instance whose source is public and any user or org
//! can host their own. The registry client is itself a `.qpkg`
//! app distributed via any registry.
//!
//! ## Trust model
//!
//! Entries in a registry are signed by the *publisher* using
//! their Ed25519 identity — the same key that appears on the
//! `.qpkg`'s `pkg::Manifest.publisher.identity` field. The
//! registry itself is signed by its *owner*: clients pin
//! registry-owner pubkeys, then verify each entry against the
//! set of publisher pubkeys the owner has allowlisted.
//! Publishers can update or delete only their own entries;
//! owners can add publishers and moderate entries.
//!
//! ## Canonical signing form
//!
//! The publisher signature covers
//! `blake3(b"qos-registry-entry-v1\0" || postcard(entry_signing_payload))`
//! where `entry_signing_payload` mirrors every field of
//! [`RegistryEntry`] except `signature_hex` (the field that
//! carries the sig would form a fixed-point otherwise).
//!
//! Registry-owner signatures for federation cover
//! `blake3(b"qos-registry-index-v1\0" || postcard(index_manifest))`.
//!
//! This mirrors the domain-separation pattern used by
//! `crates/packaging/pkg::trust::attestation_preimage`.
//!
//! ## Why postcard?
//!
//! JSON is what rides the wire — humans need to read `curl` output,
//! and the client can present entries with a browser. But JSON is
//! **not** canonical: whitespace, key order, escape choices, and
//! number formatting all vary between encoders, so signing over
//! the JSON bytes would break as soon as one side reformats.
//! Postcard gives a fixed byte order per struct field and a stable
//! length prefix on collections; two implementations that agree on
//! the field list produce identical bytes. That property is what
//! makes a detached publisher signature portable across the wire.

pub mod change;
pub mod entry;
pub mod handshake;
pub mod verify;

pub use change::{EntryAction, EntryChangeRequest, sign_change_request};
pub use entry::{
    ENTRY_DOMAIN, INDEX_DOMAIN, RegistryEntry, RegistryVersion, entry_signing_message,
    index_signing_message, sign_entry,
};
pub use handshake::{PROTOCOL_VERSION, RegistryHandshake};
pub use verify::{VerifyError, verify_change_request, verify_entry_signature};

/// Length in bytes of an Ed25519 signature, once decoded from hex.
pub const ED25519_SIGNATURE_BYTES: usize = 64;
/// Length in bytes of an Ed25519 public key, once decoded from hex.
pub const ED25519_PUBKEY_BYTES: usize = 32;
