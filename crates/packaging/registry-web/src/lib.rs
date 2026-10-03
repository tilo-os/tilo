//! Registry verification for browsers.
//!
//! A page that runs apps from a registry trusts exactly one thing: the
//! registry owner's Ed25519 key, pinned in the page. Everything it downloads
//! is checked against that key before it is compiled:
//!
//! 1. [`verify_snapshot`] checks the owner-signed catalogue snapshot
//!    (signatures, freshness, publisher authorization; see
//!    [`registry_types::verify_snapshot`]) and the anti-rollback
//!    [`Checkpoint`] the page kept from its last visit, and returns the next
//!    checkpoint to keep.
//! 2. [`VerifiedSnapshot::select`] picks the newest `web`/`wasm32` version of
//!    a package that the [`Host`] can run: its wasm features, and for apps the
//!    exact runtime ABI id of the loaded toolkit core.
//! 3. [`verify_package`] checks the thin (or full) tapp of that version: its
//!    signed root hashes to the version's package root, is signed by the
//!    entry's publisher, and commits to a manifest whose `[target]` is the
//!    version's signed target. It returns the payload files by BLAKE3.
//! 4. [`verify_blob`] checks each payload blob against its hash.
//!
//! The crate has no I/O and no clock: bytes and the current time come from
//! the caller, so the same code runs natively in tests and in the page.

mod package;
mod snapshot;

pub use package::{
    PackageKind, PayloadFile, VerifiedPackage, blake3_hex, verify_blob, verify_package,
};
pub use snapshot::{
    Checkpoint, DEFAULT_WASM_FEATURES, Host, MAX_SNAPSHOT_BYTES, Selection, VerifiedSnapshot,
    verify_snapshot,
};

/// Why a download was refused. Every variant fails closed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("registry snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes")]
    SnapshotTooLarge,
    #[error("registry snapshot is not JSON of the expected shape: {0}")]
    SnapshotDecode(String),
    #[error("registry snapshot: {0}")]
    Snapshot(#[from] registry_types::SnapshotError),
    #[error("stored checkpoint is unreadable: {0}")]
    Checkpoint(String),
    #[error(
        "registry snapshot {sequence} would roll back or fork the checkpoint at sequence \
         {checkpoint}"
    )]
    Rollback { checkpoint: u64, sequence: u64 },
    #[error("{0} is not in the registry")]
    NotListed(String),
    #[error("{package_id} has no web version for {wanted}")]
    NoCompatibleVersion { package_id: String, wanted: String },
    #[error("tapp archive: {0}")]
    Archive(String),
    #[error("tapp {0} is missing")]
    MissingEntry(&'static str),
    #[error("tapp entry {0} appears twice")]
    DuplicateEntry(String),
    #[error("tapp entry {name} exceeds {limit} bytes")]
    EntryTooLarge { name: String, limit: usize },
    #[error("signed root: {0}")]
    SignedRoot(&'static str),
    #[error("the tapp's package root is {found}, the registry lists {expected}")]
    RootMismatch { expected: String, found: String },
    #[error("the tapp is signed by {found}, the registry entry's publisher is {expected}")]
    PublisherMismatch { expected: String, found: String },
    #[error("the publisher signature does not verify")]
    Signature,
    #[error("{0} does not hash to the value the signed root commits to")]
    HashLink(&'static str),
    #[error("payload collection: {0}")]
    Collection(&'static str),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("the signed manifest does not match the registry version: {0}")]
    ManifestMismatch(&'static str),
    #[error("blob does not match BLAKE3 {0}")]
    BlobMismatch(String),
}
