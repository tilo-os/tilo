//! `qos-pkg` — the foundation library for qOS packages
//! (`.qpkg`).
//!
//! qOS distributes everything (apps, services, format providers,
//! plugins, runtimes) as content-addressed signed bundles. The
//! same packaging machinery handles all kinds; the `Kind`
//! discriminates *what* the package is, not *how* it is packed.
//!
//! This module provides the manifest schema, the BLAKE3-rooted
//! package id, and the synchronous blob-store abstraction. The
//! pack/unpack/verify operations are layered on top in sibling
//! modules.
//!
//! ## What this crate deliberately does NOT do
//!
//! - No tokio. Everything in this crate is synchronous.
//! - No on-disk file format. That layering is owned by the CLI
//!   crate (`qos-pkg-cli`).
//! - No live iroh-blobs serving. The loader crate provides a
//!   `BlobStore` adapter over a real iroh `Store`.

pub mod blobs;
pub mod id;
pub mod manifest;
pub mod signed;
pub mod validate;

pub use blobs::{BlobHash, BlobStore, BlobStoreError, Collection, MemoryBlobStore};
pub use id::{Kind, PackageId, ParsePackageIdError};
pub use manifest::{
    Entrypoint, Manifest, ManifestParseError, NetMode, Package, PathPattern, Providers, Publisher,
    Requirement, Sandbox, SandboxFs, SandboxNet, ServiceReq, ServiceReqParseError, SplashSegment,
    SplashSegmentError, Trust, TrustAttestation,
};
pub use signed::{SIGNED_ROOT_LEN, SIGNING_PREFIX_LEN, SignedRoot, SignedRootError};
pub use validate::{ManifestError, validate_manifest, validate_manifest_with_payload};
