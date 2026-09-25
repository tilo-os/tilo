//! `tilo-app-package` — the foundation library for Tilo packages
//! (`.tapp`).
//!
//! Tilo distributes everything (apps, services, format providers,
//! plugins, runtimes) as content-addressed signed bundles. The
//! same packaging machinery handles all kinds; the `Kind`
//! discriminates *what* the package is, not *how* it is packed.
//!
//! ## What this crate provides
//!
//! - [`Manifest`] + supporting types (TOML schema) — see
//!   [`manifest`].
//! - [`PackageId`] — BLAKE3 hash that identifies a bundle. See
//!   [`id`].
//! - [`validate_manifest`] / [`validate::ManifestError`] — schema
//!   checks beyond TOML decode.
//! - [`BlobStore`] trait + [`MemoryBlobStore`] in-memory
//!   implementation. The loader supplies an `iroh-blobs`-backed
//!   implementation. See [`blobs`].
//! - [`pack`], [`unpack`], [`verify`] — the three operations
//!   that consumers compose.
//! - [`SigningIdentity`] — wraps either a plain Ed25519
//!   `SigningKey` or a 1-of-1 `ThresholdIdentity`. The true
//!   threshold (`t-of-n`, `t≥2`) signing flow is driven outside
//!   this crate; pass the aggregated signature in via
//!   [`pack::pack_with_signature`].
//!
//! ## Scope boundaries
//!
//! - No tokio. Everything in this crate is synchronous.
//! - No on-disk file format. That layering is owned by the CLI
//!   crate (`tilo-app-package-cli`): it dumps a `MemoryBlobStore` into a
//!   tar archive at pack time and the reverse at unpack time.
//! - No live iroh-blobs serving. The loader crate provides a
//!   `BlobStore` adapter over an iroh `Store`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod blobs;
pub mod id;
pub mod manifest;
pub mod pack;
pub mod signed;
pub mod trust;
pub mod unpack;
pub mod validate;
pub mod verify;
pub mod version;

pub use blobs::{BlobHash, BlobStore, BlobStoreError, Collection, MemoryBlobStore};
pub use id::{Kind, PackageId, ParsePackageIdError};
pub use manifest::{
    Cli, Entrypoint, EntrypointType, Manifest, ManifestParseError, NetMode, Package, PathPattern,
    Providers, Publisher, Requirement, Sandbox, SandboxFs, SandboxNet, ServiceReq,
    ServiceReqParseError, SplashSegment, SplashSegmentError, Trust, TrustAttestation,
};
pub use pack::{PackError, SigningIdentity, pack, pack_with_signature};
pub use signed::{SIGNED_ROOT_LEN, SIGNING_PREFIX_LEN, SignedRoot, SignedRootError};
pub use trust::{
    ATTESTATION_DOMAIN, AttestorScope, RevocationList, RevokedKey, TrustLoadError, TrustedAttestor,
    TrustedAttestors, attestation_preimage, default_revoked_keys_path,
    default_trusted_attestors_path,
};
pub use unpack::{UnpackError, unpack, validate_materialized_payload};
pub use validate::{
    FORMAT_PROVIDER_MODULE, ManifestError, validate_manifest, validate_manifest_with_payload,
};
pub use verify::{Verification, verify, verify_with_trust};

/// Maximum wire size of an application `.tapp`. Large shared dependencies
/// belong in a versioned runtime bundle; user documents and downloadable
/// content are resources, not application payload.
pub const MAX_APP_TAPP_BYTES: u64 = 5 * 1024 * 1024;

/// Materialized application payload budget. Collection entries may legally
/// share one content-addressed blob; count every reference so deduplication
/// cannot turn a tiny tapp into an arbitrarily large install.
pub const MAX_APP_UNPACKED_BYTES: u64 = 5 * 1024 * 1024;

/// Bound filesystem work/inodes independently of bytes. Empty files are not
/// allowed to bypass the materialized-byte limit indefinitely.
pub const MAX_APP_PAYLOAD_ENTRIES: usize = 4096;

/// Whether a package of `kind` and wire `size` satisfies the thin-app limit.
/// Non-app bundles are shared infrastructure and use their own distribution
/// policy.
pub fn tapp_size_allowed(kind: Kind, size: u64) -> bool {
    kind != Kind::App || size <= MAX_APP_TAPP_BYTES
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn app_limit_is_five_mib_and_runtime_bundles_are_separate() {
        assert_eq!(MAX_APP_TAPP_BYTES, 5_242_880);
        assert!(tapp_size_allowed(Kind::App, MAX_APP_TAPP_BYTES));
        assert!(!tapp_size_allowed(Kind::App, MAX_APP_TAPP_BYTES + 1));
        assert!(tapp_size_allowed(Kind::Runtime, u64::MAX));
    }
}
