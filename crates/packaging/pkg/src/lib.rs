//! `qos-pkg` — the foundation library for qOS packages
//! (`.qpkg`).
//!
//! qOS distributes everything (apps, services, format providers,
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
//! ## What this crate deliberately does NOT do
//!
//! - No tokio. Everything in this crate is synchronous.
//! - No on-disk file format. That layering is owned by the CLI
//!   crate (`pkg-cli`): it dumps a `MemoryBlobStore` into a
//!   tar archive at pack time and the reverse at unpack time.
//! - No live iroh-blobs serving. The loader crate provides a
//!   `BlobStore` adapter over a real iroh `Store`.

pub mod blobs;
pub mod id;
pub mod manifest;
pub mod pack;
pub mod signed;
pub mod trust;
pub mod unpack;
pub mod validate;
pub mod verify;

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
pub use unpack::{UnpackError, unpack};
pub use validate::{ManifestError, validate_manifest, validate_manifest_with_payload};
pub use verify::{Verification, verify, verify_with_trust};
