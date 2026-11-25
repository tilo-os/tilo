//! Wire types for the Tilo app registry: signed package metadata, catalogues, moderation and the
//! change requests that edit them.
//!
//! Entries are signed by the publisher, the registry by its owner: clients pin owner keys, then
//! verify each entry against the publishers the owner allows. JSON rides the wire, but signatures
//! cover postcard encodings of a fixed field list — JSON is not canonical, so signing its bytes
//! would break as soon as one side reformats.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod application;
pub mod block;
pub mod catalogue;
pub mod change;
pub mod compact;
pub mod entry;
mod external_moderation;
pub mod handshake;
pub mod interfaces;
pub mod key_file;
pub mod moderation;
pub use external_moderation::{
    ExternalModerationOperation, ModerationCredential, ModerationDestination,
    sign_external_moderation, sign_external_moderation_as,
};
pub mod namespace;
pub mod pending;
mod report_evidence;
pub mod retention;
pub use report_evidence::report_evidence_digest;
pub mod snapshot;
pub mod target;
pub mod upload;
pub mod verify;
pub mod version;

pub use block::{Block, BlockList, Blocker, UnblockError};
pub use change::{EntryAction, EntryChangeRequest, sign_change_request};
pub use compact::{
    CatalogueEntry, CatalogueSnapshot, CompactRegistryEntry, CompactRegistrySnapshot,
    MAX_VERSION_INDEX_BYTES, RegistryVersionIndex, VersionIndexError, VersionIndexFile,
    VersionIndexRef, compact_signing_message, compact_snapshot, compact_snapshot_hash,
    newest_versions, sign_compact_snapshot, verify_compact_snapshot, verify_version_index,
    version_index_bytes,
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
    Behind, InterfaceError, InterfaceRequirement, ProvidedInterfaces, RUNTIME_INTERFACE,
    RuntimeServices, SYSTEM_INTERFACE, Unmet, describe_unmet, valid_interface_name,
};
pub use moderation::{
    MODERATION_PROTOCOL, ModerationActorRole, ModerationCase, ModerationCaseStatus,
    ModerationDecision, ModerationEvent, ModerationEventKind, ModerationMutation,
    ModerationWireError, SignedModerationMutation, TransparencyPage, derive_verified_case,
    moderation_case_id, moderation_signing_message, transparency_event_hash,
    verify_moderation_authorization, verify_moderation_mutation, verify_transparency_chain,
};
pub use snapshot::{
    ChannelAuthorization, MAX_SNAPSHOT_ENCODED_BYTES, MAX_SNAPSHOT_ENTRIES,
    MAX_SNAPSHOT_PUBLISHERS, MAX_SNAPSHOT_VALIDITY_SECONDS, PublisherAuthorization,
    SignedRegistrySnapshot, SnapshotError, sign_channel_authorization, sign_snapshot,
    sign_snapshot_delegated, snapshot_hash, snapshot_signing_message, verify_channel_authorization,
    verify_snapshot, within_validity_window,
};
pub use target::{
    ANDROID_PACKAGE_EXTENSIONS, ANDROID_PLATFORM, ANY_ARCHITECTURE, DESKTOP_PLATFORMS,
    LANGUAGE_PACK_PACKAGE_PREFIX, LINUX_PLATFORM, MACOS_PLATFORM, NATIVE_PLATFORMS,
    NATIVE_RUNTIME_ID_HEX_LEN, NATIVE_RUNTIME_MARKER, NATIVE_RUNTIME_PACKAGE_ID, NativeRuntime,
    TILO_OS_PLATFORM, TargetError, WINDOWS_PLATFORM, is_language_pack_package_id,
    is_runtime_package_id, parse_native_runtime,
};
pub use verify::{VerifyError, verify_change_request, verify_entry_signature};

/// The package format this build writes and the last of [`READABLE_CATALOGUE_FORMATS`]: a package's
/// archive, its signed manifest, root and chunk list, and the registry's catalogue, each carrying
/// the number in its `format` or `protocol` field.
/// `crates/core/tilo-app-package/interface.toml` holds the schema.
pub const PACKAGE_FORMAT_VERSION: u32 = 4;

/// Every catalogue protocol this build verifies, oldest first, the newest being
/// [`PACKAGE_FORMAT_VERSION`]. A reader keeps every format it can decode and verify; a format
/// that changes the catalogue's signed form leaves the earlier ones unreadable, and they go.
///
/// Format 4 adds each version's `tapp_surface_hex` to the signed form. The form is positional,
/// so a catalogue written before it neither decodes nor verifies here, and one written by this
/// build does not by an earlier one: a reader refuses the other format in words
/// ([`unreadable_catalogue_format`]), and a registry publishes its catalogue again at format 4.
pub const READABLE_CATALOGUE_FORMATS: [u32; 1] = [PACKAGE_FORMAT_VERSION];

/// What a catalogue at a protocol this build does not read means, and what to do.
#[must_use]
pub fn unreadable_catalogue_format(protocol: u32) -> String {
    let newest = PACKAGE_FORMAT_VERSION;
    if protocol > newest {
        format!(
            "this catalogue is published in package format {protocol}, which is newer than this \
             Tilo reads (up to {newest}): update Tilo to read this catalogue"
        )
    } else {
        format!(
            "this catalogue is published in package format {protocol}, which no Tilo reads any \
             more (this one reads {newest}): the registry has to publish its catalogue at format \
             {newest}"
        )
    }
}
