//! Wire types for the Tilo app registry.
//!
//! A registry advertises signed metadata for `.tapp` packages —
//! `id`, `versions`, `publisher pubkey`, `name`, `description`,
//! icon / source / homepage URLs, and a BLAKE3 root hash so
//! downloaders can verify the bytes match what the registry
//! signed for. Multiple registries can coexist: Tilo ships one
//! hosted instance whose source is public and any user or org
//! can host their own. The registry client is itself a `.tapp`
//! app distributed via any registry.
//!
//! ## Trust model
//!
//! Entries in a registry are signed by the *publisher* using
//! their Ed25519 identity — the same key that appears on the
//! `.tapp`'s `tilo_app_package::Manifest.publisher.identity` field. The
//! registry itself is signed by its *owner*: clients pin
//! registry-owner pubkeys, then verify each entry against the
//! set of publisher pubkeys the owner has allowlisted.
//! Publishers can update or delete only their own entries;
//! owners can add publishers and moderate entries.
//!
//! ## Canonical signing form
//!
//! The publisher signature covers
//! `blake3(b"tilo-registry-entry-v1\0" || postcard(entry_signing_payload))`
//! where `entry_signing_payload` mirrors every field of
//! [`RegistryEntry`] except `signature_hex` (the field that
//! carries the sig would form a fixed-point otherwise) and `kind`. A bot
//! entry signs the same payload under `tilo-registry-bot-entry-v1`, so the
//! kind is bound by the domain and app entries keep their original bytes.
//!
//! Registry-owner signatures for federation cover
//! `blake3(b"tilo-registry-index-v1\0" || postcard(index_manifest))`.
//!
//! This mirrors the domain-separation pattern used by
//! `crates/packaging/tilo-app-package::trust::attestation_preimage`.
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

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod application;
pub mod catalogue;
pub mod change;
pub mod compact;
pub mod entry;
pub mod handshake;
pub mod interfaces;
pub mod moderation;
pub mod namespace;
pub mod pending;
pub mod retention;
pub mod snapshot;
pub mod target;
pub mod upload;
pub mod verify;
pub mod version;

pub use change::{EntryAction, EntryChangeRequest, sign_change_request};
pub use compact::{
    COMPACT_SNAPSHOT_PROTOCOL, CatalogueEntry, CatalogueSnapshot, CompactRegistryEntry,
    CompactRegistrySnapshot, MAX_VERSION_INDEX_BYTES, RegistryVersionIndex, VERSION_INDEX_PROTOCOL,
    VersionIndexError, VersionIndexFile, VersionIndexRef, compact_signing_message,
    compact_snapshot, compact_snapshot_hash, newest_versions, sign_compact_snapshot,
    verify_compact_snapshot, verify_version_index, version_index_bytes,
};
pub use entry::{
    BOT_ENTRY_DOMAIN, BotArtifact, BotRequirements, DirectoryRef, ENTRY_DOMAIN, EntryKind,
    EntryValidationError, INDEX_DOMAIN, LocalizedImages, LocalizedText, MAX_BOT_PROGRAM_BYTES,
    PackageRequirement, RegistryEntry, RegistryImage, RegistryImageKind, RegistryTarget,
    RegistryVersion, TappArtifact, VersionArtifact, VersionGroup, bot_program_url,
    entry_signing_message, index_signing_message, sign_entry, valid_language_tag,
};
pub use handshake::{PROTOCOL_VERSION, RegistryHandshake};
pub use interfaces::{
    InterfaceError, InterfaceRequirement, ProvidedInterfaces, SYSTEM_INTERFACE, Unmet,
    describe_unmet, valid_interface_name,
};
pub use moderation::{
    MODERATION_DOMAIN, ModerationActorRole, ModerationCase, ModerationCaseStatus,
    ModerationDecision, ModerationEvent, ModerationEventKind, ModerationMutation,
    SignedModerationMutation, TransparencyPage, derive_verified_case, moderation_case_id,
    moderation_signing_message, sign_moderation_mutation, transparency_event_hash,
    verify_moderation_authorization, verify_moderation_mutation, verify_transparency_chain,
};
pub use snapshot::{
    CHANNEL_AUTHORIZATION_PROTOCOL, ChannelAuthorization, DELEGATED_SNAPSHOT_PROTOCOL,
    MAX_SNAPSHOT_ENCODED_BYTES, MAX_SNAPSHOT_ENTRIES, MAX_SNAPSHOT_PUBLISHERS,
    MAX_SNAPSHOT_VALIDITY_SECONDS, PublisherAuthorization, SNAPSHOT_PROTOCOL,
    SignedRegistrySnapshot, SnapshotError, sign_channel_authorization, sign_snapshot,
    sign_snapshot_delegated, snapshot_hash, snapshot_signing_message, verify_channel_authorization,
    verify_snapshot, within_validity_window,
};
pub use target::{
    ANDROID_PACKAGE_EXTENSIONS, ANDROID_PLATFORM, DESKTOP_PLATFORMS, LINUX_PLATFORM,
    MACOS_PLATFORM, NATIVE_PLATFORMS, NATIVE_RUNTIME_ID_HEX_LEN, NATIVE_RUNTIME_MARKER,
    NATIVE_RUNTIME_PACKAGE_ID, NativeRuntime, TILO_OS_PLATFORM, TargetError, WINDOWS_PLATFORM,
    is_runtime_package_id, parse_native_runtime,
};
pub use verify::{VerifyError, verify_change_request, verify_entry_signature};

/// Length in bytes of an Ed25519 signature, once decoded from hex.
pub const ED25519_SIGNATURE_BYTES: usize = 64;
/// Length in bytes of an Ed25519 public key, once decoded from hex.
pub const ED25519_PUBKEY_BYTES: usize = 32;
