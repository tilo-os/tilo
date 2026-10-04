//! `tilo-app-package` — the format of Tilo packages (`.tapp`).
//!
//! Tilo distributes everything (apps, services, format providers,
//! plugins, runtimes) as content-addressed signed packages. The
//! same packaging machinery handles all kinds; the `Kind`
//! discriminates *what* the package is, not *how* it is packed.
//!
//! ## What this crate provides
//!
//! - [`Manifest`] + supporting types (TOML schema) — see
//!   [`manifest`].
//! - [`PackageId`] — BLAKE3 hash that identifies a package. See
//!   [`id`].
//! - [`validate_manifest`] / [`validate::ManifestError`] — schema
//!   checks beyond TOML decode.
//! - [`BlobHash`], [`Collection`] and the streaming [`BlobSource`] /
//!   [`BlobSink`] edges — see [`blobs`].
//! - [`PackageMeta`] — the signed root, manifest and collection with every
//!   hash link checked. See [`package`].
//! - [`pack()`], [`unpack()`], [`verify()`] and the [`tapp`] archive codec
//!   (thin or full) — the operations consumers compose.
//! - [`SigningIdentity`] — wraps either a plain Ed25519
//!   `SigningKey` or a 1-of-1 `ThresholdIdentity`. The true
//!   threshold (`t-of-n`, `t≥2`) signing flow is driven outside
//!   this crate; pass the aggregated signature in via
//!   [`pack::pack_with_signature`].
//!
//! ## Scope boundaries
//!
//! - No tokio. Everything in this crate is synchronous.
//! - No storage. Payload bytes flow through [`BlobSource`] and
//!   [`BlobSink`]; keeping them is the device content store's job.
//! - No networking. Fetching the payload of a thin `.tapp` belongs to
//!   whoever knows where bytes are.

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
pub mod desktop;
pub mod id;
pub mod image;
pub mod manifest;
pub mod pack;
pub mod package;
pub mod signed;
pub mod tapp;
pub mod trust;
pub mod unpack;
pub mod validate;
pub mod verify;
pub mod version;
pub mod web;

pub use blobs::{
    BlobFetcher, BlobHash, BlobSink, BlobSource, Collection, CollectionDecodeError, DiscardBlobs,
    HashingReader, OpenBlob, PackageFetcher,
};
pub use id::{Kind, PackageId, ParsePackageIdError};
pub use image::{
    ImageError, ImageSpec, TREE_FILE, TreeSpec, image_manifest, seal_image, seal_image_for,
};
pub use manifest::{
    Capabilities, Cli, DEFAULT_PLATFORMS, Entrypoint, EntrypointType, Image, Manifest,
    ManifestParseError, NetMode, Package, PathPattern, Platform, Providers, Publisher, Requirement,
    Sandbox, SandboxFs, SandboxNet, ServiceReq, ServiceReqParseError, SplashSegment,
    SplashSegmentError, Target, Trust, TrustAttestation,
};
pub use pack::{
    PackError, Packed, SigningIdentity, pack, pack_with_signature, sign_placeholder_root,
};
pub use package::{MetaError, PackageMeta, is_safe_path};
pub use signed::{SIGNED_ROOT_LEN, SIGNING_PREFIX_LEN, SignedRoot, SignedRootError};
pub use tapp::{TAPP_FORMAT_VERSION, Tapp, TappError};
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

/// The application size ceiling, in bytes. It lives in a plain file beside
/// this crate so the packaging scripts (`os/scripts/pack-apps.sh`,
/// `dist/apps/package-apps.sh`) read the very same number instead of carrying
/// a copy that can drift.
const APP_SIZE_LIMIT_BYTES: u64 = parse_size_limit(include_str!("../max-app-bytes"));

const fn parse_size_limit(text: &str) -> u64 {
    let mut rest = text.as_bytes();
    let mut value: u64 = 0;
    while let [digit @ b'0'..=b'9', tail @ ..] = rest {
        value = value
            .saturating_mul(10)
            .saturating_add(digit.wrapping_sub(b'0') as u64);
        rest = tail;
    }
    assert!(
        matches!(rest, [] | [b'\n']),
        "max-app-bytes must be a decimal byte count"
    );
    assert!(value > 0, "max-app-bytes must be positive");
    value
}

/// Maximum wire size of an application `.tapp`. Large shared dependencies
/// belong in a versioned runtime bundle; user documents and downloadable
/// content are resources, not application payload.
pub const MAX_APP_TAPP_BYTES: u64 = APP_SIZE_LIMIT_BYTES;

/// Materialized application payload budget. Collection entries may legally
/// share one content-addressed blob; count every reference so deduplication
/// cannot turn a tiny tapp into an arbitrarily large install.
pub const MAX_APP_UNPACKED_BYTES: u64 = APP_SIZE_LIMIT_BYTES;

/// Wire size limit of a Linux, Windows, macOS or Android build. Those systems ship no Tilo
/// toolkit, so the executable links it in; the limit is a distribution
/// policy, not a design target.
pub const MAX_DESKTOP_APP_TAPP_BYTES: u64 = 64 * 1024 * 1024;

/// Bound filesystem work/inodes independently of bytes. Empty files are not
/// allowed to bypass the materialized-byte limit indefinitely.
pub const MAX_APP_PAYLOAD_ENTRIES: usize = 4096;

/// Whether a package of `kind` and wire `size` satisfies the thin-app limit.
/// Non-app bundles are shared infrastructure and use their own distribution
/// policy.
pub fn tapp_size_allowed(kind: Kind, size: u64) -> bool {
    kind != Kind::App || size <= MAX_APP_TAPP_BYTES
}

/// The wire and materialized size limit of an application built for the
/// registry `platform` (`registry_types::TILO_OS_PLATFORM`, `"linux"`,
/// `"android"`, …): [`MAX_DESKTOP_APP_TAPP_BYTES`] for a build that links the
/// toolkit in, [`MAX_APP_TAPP_BYTES`] otherwise. The registry enforces it and
/// `tilo-app package` reports against it.
#[must_use]
pub fn platform_size_limit(platform: &str) -> u64 {
    if registry_types::DESKTOP_PLATFORMS.contains(&platform) {
        MAX_DESKTOP_APP_TAPP_BYTES
    } else {
        MAX_APP_TAPP_BYTES
    }
}

/// The wire and materialized size limit of the application `manifest`
/// describes: its `[target]` platform's [`platform_size_limit`], or the
/// thin-app limit without one. `None` for non-app bundles.
pub fn app_size_limit(manifest: &Manifest) -> Option<u64> {
    if manifest.package.kind != Kind::App {
        return None;
    }
    Some(
        manifest
            .target
            .as_ref()
            .map_or(MAX_APP_TAPP_BYTES, |target| {
                platform_size_limit(&target.platform)
            }),
    )
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn builds_that_link_the_toolkit_in_get_the_desktop_limit() {
        for platform in [
            "linux",
            "windows",
            "macos",
            registry_types::ANDROID_PLATFORM,
        ] {
            assert_eq!(
                platform_size_limit(platform),
                MAX_DESKTOP_APP_TAPP_BYTES,
                "{platform}"
            );
        }
        for platform in [registry_types::TILO_OS_PLATFORM, "web", "unknown"] {
            assert_eq!(
                platform_size_limit(platform),
                MAX_APP_TAPP_BYTES,
                "{platform}"
            );
        }
    }

    #[test]
    fn app_limit_is_eight_mib_and_runtime_bundles_are_separate() {
        assert_eq!(MAX_APP_TAPP_BYTES, 8 * 1024 * 1024);
        assert_eq!(MAX_APP_UNPACKED_BYTES, MAX_APP_TAPP_BYTES);
        assert!(tapp_size_allowed(Kind::App, MAX_APP_TAPP_BYTES));
        assert!(!tapp_size_allowed(Kind::App, MAX_APP_TAPP_BYTES + 1));
        assert!(tapp_size_allowed(Kind::Runtime, u64::MAX));
    }
}
