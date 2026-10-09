//! Tilo runtime bundle producer + verifier.
//!
//! A bundle is a directory (`bundle.toml` + `lib/` + optional
//! `etc/ld.so.cache`) holding a host-copied loader/libc; it is
//! content-addressed — `bundle.toml.root_hash` is a BLAKE3 root over
//! the sorted `(path, blake3(file))` pairs, so any byte change flips it.
//!
//! The source paths to copy are architecture-specific
//! ([`host_minimal_paths_for`]); at build time each is probed against
//! the host and silently skipped if absent, since Debian and Fedora
//! layouts differ and only what exists is copied.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod component_pins;
pub mod verity;

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tilo_clock::unix_now;

use serde::{Deserialize, Serialize};

/// Standard bundle manifest filename inside a bundle directory.
pub const BUNDLE_MANIFEST_FILENAME: &str = "bundle.toml";

/// Domain-separation prefix for the bundle-signing preimage; bumped only on a wire-breaking change.
pub const BUNDLE_SIGNING_DOMAIN: &[u8] = b"tilo-runtime-bundle-v1\0";

/// Domain-separation prefix for the preimage of a bundle's fs-verity
/// signature ([`BundleManifest::verity_signature_hex`]).
pub const VERITY_SIGNING_DOMAIN: &[u8] = b"tilo-runtime-bundle-verity-v1\0";

/// Domain-separation prefix of a runtime build's ABI digest.
pub const ABI_DIGEST_DOMAIN: &[u8] = b"tilo-runtime-abi-v1\0";

/// Separates a runtime release from its ABI digest in a bundle version:
/// `2026.07+native.<32 hex>`. The same spelling `registry_types` parses in
/// an app's `tilo-runtime/<version>`.
pub const NATIVE_ABI_MARKER: &str = "+native.";

/// Hex digits of the ABI digest.
pub const ABI_DIGEST_HEX_LEN: usize = 32;

/// The ABI digest of the runtime build whose payload has `root_hash`.
///
/// Thin apps link the bundle's Rust dylibs (`libruntime_dylib.so`,
/// `libstd-<hash>.so`), which have no stable ABI: an app runs only on the
/// exact build it was linked against. The digest covers every payload byte,
/// so two bundles share it exactly when an app linked against one loads the
/// other.
#[must_use]
pub fn abi_digest(root_hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ABI_DIGEST_DOMAIN);
    hasher.update(root_hash.as_bytes());
    let mut hex = hasher.finalize().to_hex().to_string();
    hex.truncate(ABI_DIGEST_HEX_LEN);
    hex
}

/// The bundle version of the build of `release` whose payload has
/// `root_hash`: `<release>+native.<abi digest>`.
#[must_use]
pub fn abi_version(release: &str, root_hash: &str) -> String {
    format!("{release}{NATIVE_ABI_MARKER}{}", abi_digest(root_hash))
}

/// Split an ABI-bound bundle version into its release and digest; `None`
/// for a version that names a release only (an engine runtime, a
/// development bundle).
#[must_use]
pub fn split_abi_version(version: &str) -> Option<(&str, &str)> {
    version.split_once(NATIVE_ABI_MARKER)
}

/// Bundle metadata written into `bundle.toml`; fields are stable so `verify` roundtrips across tool versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Human-readable version tag — matches `tilo-runtime/<version>` in a package manifest.
    pub version: String,
    /// Target architecture the bundle was built for (matches
    /// `std::env::consts::ARCH`); a launcher on the wrong arch refuses
    /// the bundle rather than getting the loader wrong at exec time.
    pub arch: String,
    /// BLAKE3 root hash over the sorted (path, file-hash) list; hex, lowercase, 64 chars.
    pub root_hash: String,
    /// Sorted list of every bundle file (relative to root) with its BLAKE3, sorted for deterministic encoding.
    #[serde(default)]
    pub files: Vec<BundleFile>,
    /// Produced-at unix time; informational only — the hash is what matters for identity.
    #[serde(default)]
    pub produced_at: u64,
    /// Optional publisher Ed25519 public key, hex-encoded (64 chars); present iff signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_hex: Option<String>,
    /// Optional Ed25519 signature over `blake3(DOMAIN || version || root_hash)`, hex-encoded (128 chars); present iff signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hex: Option<String>,
    /// The same publisher's Ed25519 signature over [`verity_signing_preimage`]:
    /// the version, the root hash and every file's [`BundleFile::verity`]
    /// digest. A separate signature, so the root hash, the runtime's ABI
    /// digest and the signature older launchers check are unchanged by it.
    /// Absent on unsigned bundles and on bundles built before fs-verity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verity_signature_hex: Option<String>,
}

/// One `(payload-relative-path, hash)` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Path relative to the bundle root, always `/`-separated for portable encoding.
    pub path: String,
    /// BLAKE3 hash of the file bytes. Hex, lowercase.
    pub hash: String,
    /// Size in bytes.
    pub size: u64,
    /// The file's fs-verity digest (SHA-256, 4 KiB blocks, no salt; see
    /// [`verity`]), hex. Covered by [`BundleManifest::verity_signature_hex`];
    /// a launch that measures this digest on a verity-enabled file trusts
    /// the file without reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verity: Option<String>,
}

/// Errors produced by the builder + verifier.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("no bundle sources found — checked {checked:?}")]
    NoSources { checked: Vec<PathBuf> },
    #[error("bundle root {root} contains no lib/ payload")]
    EmptyBundle { root: PathBuf },
    #[error("bundle.toml not found at {path}")]
    ManifestMissing { path: PathBuf },
    #[error("bundle.toml parse error: {0}")]
    ManifestParse(String),
    #[error("hash mismatch: expected {expected} in bundle.toml, computed {computed}")]
    HashMismatch { expected: String, computed: String },
    #[error("file listed in bundle.toml is missing from the payload: {path}")]
    FileMissing { path: String },
    #[error("file present in payload but not listed in bundle.toml: {path}")]
    FileUnlisted { path: String },
    #[error("invalid payload path in bundle.toml: {path}")]
    InvalidPayloadPath { path: String },
    #[error("payload entry is not a regular file: {path}")]
    NonRegularFile { path: String },
    #[error("file size mismatch for {path}: expected {expected}, got {actual}")]
    SizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("home / data-dir not resolvable — set XDG_DATA_HOME or HOME")]
    NoDataDir,
    #[error("SOURCE_DATE_EPOCH must be an unsigned unix timestamp: {0}")]
    InvalidSourceDateEpoch(String),
    #[error("bundle is not signed — signature required (pass --allow-unsigned-runtime to opt out)")]
    NotSigned,
    #[error("bundle signature does not verify against embedded publisher")]
    BadSignature,
    #[error("bundle fs-verity signature does not verify against embedded publisher")]
    BadVeritySignature,
    #[error("bundle publisher {actual} does not match required {required}")]
    PublisherMismatch { required: String, actual: String },
    #[error("bundle arch {actual:?} does not match host arch {host:?}")]
    ArchMismatch { actual: String, host: String },
    // Named trailing argument: thiserror 2 rejects mixing `{0}` (tuple
    // field) with a bare `{}` in one format string.
    #[error("no host source table for arch {0} — supported: {supported}", supported = supported_archs().join(", "))]
    UnsupportedArch(String),
    #[error("identity file: {0}")]
    IdentityIo(String),
    #[error("identity file: {0}")]
    IdentityParse(String),
    #[error("bundle signature field {field} is malformed: {reason}")]
    SignatureFieldMalformed { field: &'static str, reason: String },
    #[error("extra lib to include is missing from the host: {path}")]
    IncludeMissing { path: PathBuf },
    #[error("extra lib to include has no file name: {path}")]
    IncludeUnnamed { path: PathBuf },
    #[error("included directory holds something other than files and directories: {path}")]
    IncludeSpecialFile { path: PathBuf },
    #[error(
        "bundle version {version} claims ABI digest {claimed}, but its payload's digest is \
         {computed}: it is not the runtime build it names"
    )]
    AbiDigestMismatch {
        version: String,
        claimed: String,
        computed: String,
    },
    #[error("runtime release {0:?} must not be empty or contain `/` or `+`")]
    InvalidRelease(String),
}

/// The set of host source paths for a given target architecture.
///
/// Absolute paths on the *host*, resolved against `--from` (default
/// `/`); missing ones are silently skipped, so the result is still
/// hash-stable since the manifest lists only what was actually
/// included. Two layouts are covered per arch: Fedora/RHEL-ish
/// (`/lib64`, `/usr/lib64`) and Debian/Ubuntu-ish
/// (`/lib/<arch>-linux-gnu`). Adding an architecture means adding a
/// row here and to `CANONICAL_LOADER_PATHS` on the sandbox side.
pub fn host_minimal_paths_for(arch: &str) -> Option<&'static [&'static str]> {
    match arch {
        "x86_64" => Some(HOST_MINIMAL_PATHS_X86_64),
        "aarch64" => Some(HOST_MINIMAL_PATHS_AARCH64),
        _ => None,
    }
}

/// List every architecture [`host_minimal_paths_for`] has a table for.
pub fn supported_archs() -> Vec<&'static str> {
    vec!["x86_64", "aarch64"]
}

/// `std::env::consts::ARCH` at build time; the default target for `build_bundle` when unset.
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// `x86_64` loader + libc set: Fedora (/lib64), Debian
/// (/lib/x86_64-linux-gnu via symlink), and legacy /lib/ld-linux.so.2.
pub const HOST_MINIMAL_PATHS_X86_64: &[&str] = &[
    // Dynamic loader itself.
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
    "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    // libc + friends that most Rust binaries need.
    "/lib64/libc.so.6",
    "/lib64/libgcc_s.so.1",
    "/lib64/libm.so.6",
    "/lib64/libdl.so.2",
    "/lib64/libpthread.so.0",
    "/lib64/librt.so.1",
    "/lib64/libutil.so.1",
    // Debian-ish alternatives — bundle-hash reflects whichever exist.
    "/lib/x86_64-linux-gnu/libc.so.6",
    "/lib/x86_64-linux-gnu/libgcc_s.so.1",
    "/lib/x86_64-linux-gnu/libm.so.6",
    "/lib/x86_64-linux-gnu/libdl.so.2",
    "/lib/x86_64-linux-gnu/libpthread.so.0",
    "/lib/x86_64-linux-gnu/librt.so.1",
    "/lib/x86_64-linux-gnu/libutil.so.1",
];

/// aarch64 loader + libc set: Debian (/lib/aarch64-linux-gnu) and Fedora (/lib64, same convention as `x86_64`).
pub const HOST_MINIMAL_PATHS_AARCH64: &[&str] = &[
    // Dynamic loader itself.
    "/lib/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
    "/usr/lib64/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    // Fedora aarch64's /lib64 is a real dir (not a symlink to
    // /usr/lib64, which also exists) — bundle both.
    "/lib64/libc.so.6",
    "/lib64/libgcc_s.so.1",
    "/lib64/libm.so.6",
    "/lib64/libdl.so.2",
    "/lib64/libpthread.so.0",
    "/lib64/librt.so.1",
    "/lib64/libutil.so.1",
    "/usr/lib64/libc.so.6",
    "/usr/lib64/libgcc_s.so.1",
    "/usr/lib64/libm.so.6",
    "/usr/lib64/libdl.so.2",
    "/usr/lib64/libpthread.so.0",
    "/usr/lib64/librt.so.1",
    "/usr/lib64/libutil.so.1",
    // Debian-ish alternatives.
    "/lib/aarch64-linux-gnu/libc.so.6",
    "/lib/aarch64-linux-gnu/libgcc_s.so.1",
    "/lib/aarch64-linux-gnu/libm.so.6",
    "/lib/aarch64-linux-gnu/libdl.so.2",
    "/lib/aarch64-linux-gnu/libpthread.so.0",
    "/lib/aarch64-linux-gnu/librt.so.1",
    "/lib/aarch64-linux-gnu/libutil.so.1",
];

/// Legacy alias for [`HOST_MINIMAL_PATHS_X86_64`]; new code should use [`host_minimal_paths_for`].
#[deprecated(note = "use host_minimal_paths_for(arch) — HOST_MINIMAL_PATHS is x86_64-only")]
pub const HOST_MINIMAL_PATHS: &[&str] = HOST_MINIMAL_PATHS_X86_64;

/// Optional `/etc/ld.so.cache` — loader falls back to a slower search without it; include when present.
pub const HOST_LD_SO_CACHE: &str = "/etc/ld.so.cache";

/// Environment override naming the root that holds `<version>/` bundle
/// directories (counterpart of `TILO_APPS_DATA_DIR`).
///
/// Repointing `XDG_DATA_HOME` for a session's own documents would
/// also isolate the runtime bundles living under it, breaking every
/// app's launch — this variable lets a caller keep the (shared)
/// runtime reachable without moving it into the session's isolated documents.
pub const RUNTIME_DIR_ENV: &str = "TILO_RUNTIME_BUNDLE_DIR";

/// Root holding the per-user bundles, before the version is appended:
/// [`RUNTIME_DIR_ENV`] when set and non-empty, else
/// `$XDG_DATA_HOME/tilo-runtime`.
///
/// # Errors
///
/// Returns [`BundleError::NoDataDir`] if [`RUNTIME_DIR_ENV`] is unset
/// and the platform data directory can't be resolved (`XDG_DATA_HOME`/`HOME` unset).
pub fn bundle_root() -> Result<PathBuf, BundleError> {
    bundle_root_from(std::env::var_os(RUNTIME_DIR_ENV))
}

/// [`bundle_root`] with the override supplied by the caller instead of read
/// from [`RUNTIME_DIR_ENV`]. The launcher passes only an override its own
/// build policy allows; `None` (or empty) means the platform data directory.
///
/// # Errors
///
/// Returns [`BundleError::NoDataDir`] if `root_override` is unset and the
/// platform data directory can't be resolved.
pub fn bundle_root_from(root_override: Option<std::ffi::OsString>) -> Result<PathBuf, BundleError> {
    if let Some(dir) = root_override
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    Ok(tilo_paths::data_dir()
        .ok_or(BundleError::NoDataDir)?
        .join("tilo-runtime"))
}

/// Default per-user bundle directory: `$XDG_DATA_HOME/tilo-runtime/<v>/`,
/// unless [`RUNTIME_DIR_ENV`] names another root.
///
/// # Errors
///
/// Returns [`BundleError::NoDataDir`] under the same condition as [`bundle_root`].
pub fn default_bundle_path(version: &str) -> Result<PathBuf, BundleError> {
    Ok(bundle_root()?.join(version))
}

/// [`default_bundle_path`] under [`bundle_root_from`]'s caller-supplied
/// override.
///
/// # Errors
///
/// Returns [`BundleError::NoDataDir`] under the same condition as
/// [`bundle_root_from`].
pub fn default_bundle_path_from(
    root_override: Option<std::ffi::OsString>,
    version: &str,
) -> Result<PathBuf, BundleError> {
    Ok(bundle_root_from(root_override)?.join(version))
}

/// System-wide fallback: `/usr/lib/tilo-runtime/<v>/`.
pub fn system_bundle_path(version: &str) -> PathBuf {
    PathBuf::from("/usr/lib/tilo-runtime").join(version)
}

/// The image's list of runtime publishers the launcher accepts: one hex
/// Ed25519 key per line, `#` comments.
pub const TRUSTED_PUBLISHERS_FILE: &str = "/etc/tilo/runtime-bundle-trusted-publishers.txt";

/// Read a trusted-publisher list, ignoring blank and `#` lines; `None` when
/// the file is absent or unreadable.
#[must_use]
pub fn load_trusted_publishers(path: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_ascii_lowercase)
            .collect(),
    )
}

/// Walk the search path and return the first existing bundle dir with a `bundle.toml` (per-user, then system).
pub fn resolve_bundle_path(version: &str) -> Option<PathBuf> {
    if let Ok(p) = default_bundle_path(version)
        && p.join(BUNDLE_MANIFEST_FILENAME).exists()
    {
        return Some(p);
    }
    let sys = system_bundle_path(version);
    if sys.join(BUNDLE_MANIFEST_FILENAME).exists() {
        return Some(sys);
    }
    None
}

/// Optional knobs for [`build_bundle_with`].
#[derive(Default, Debug, Clone)]
pub struct BuildOptions<'a> {
    /// Host root each source path is resolved against; `None` = `/`.
    pub from: Option<&'a Path>,
    /// Target architecture; defaults to [`host_arch`]. Explicit override lets tests fabricate a non-host-arch bundle.
    pub arch: Option<&'a str>,
    /// Path to an identity JSON file (see [`Identity::load`]); if set, the finished bundle is signed.
    pub identity: Option<&'a Path>,
    /// Extra shared objects placed directly under `lib/` by file name —
    /// e.g. `libstd-<hash>.so`, `libruntime_dylib.so` for a thin
    /// (`-C prefer-dynamic`) app. Unlike the loader/libc set (re-bound at
    /// canonical `/lib64` paths for `INTERP`), these sit at the top of
    /// `lib/` so `LD_LIBRARY_PATH=<mount>/lib` resolves them by soname.
    /// Hashed and signature-covered like any other bundle file; absolute
    /// host paths, must exist.
    pub extra_libs: &'a [PathBuf],
}

/// Signing identity for [`build_bundle_with`]: JSON `{ "secret_key_hex": "<64-hex>" }`, same shape as `tilo-app-package`.
#[derive(Clone)]
pub struct Identity {
    signing_key: ed25519_dalek::SigningKey,
}

impl Identity {
    /// Load from a JSON file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::IdentityIo`] if `path` can't be read, or
    /// any error [`Self::from_json_bytes`] returns for a malformed file.
    pub fn load(path: &Path) -> Result<Self, BundleError> {
        let bytes = fs::read(path).map_err(|e| BundleError::IdentityIo(e.to_string()))?;
        Self::from_json_bytes(&bytes)
    }

    /// Parse an identity JSON blob directly.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::IdentityParse`] if `bytes` isn't valid
    /// JSON for the expected shape, or if `secret_key_hex` doesn't
    /// decode to exactly 32 bytes.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, BundleError> {
        #[derive(Deserialize)]
        struct File {
            secret_key_hex: String,
        }
        let f: File =
            serde_json::from_slice(bytes).map_err(|e| BundleError::IdentityParse(e.to_string()))?;
        let raw = hex::decode(&f.secret_key_hex)
            .map_err(|e| BundleError::IdentityParse(format!("secret_key_hex: {e}")))?;
        if raw.len() != 32 {
            return Err(BundleError::IdentityParse(format!(
                "secret_key_hex must decode to 32 bytes (got {})",
                raw.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&raw);
        Ok(Self {
            signing_key: ed25519_dalek::SigningKey::from_bytes(&seed),
        })
    }

    /// Generate a fresh random identity; used by tests and the CLI's `key generate`.
    pub fn generate() -> Self {
        let mut rng = UnwrapErr(SysRng);
        Self {
            signing_key: ed25519_dalek::SigningKey::generate(&mut rng),
        }
    }

    /// Serialise as the same JSON shape `load` reads.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        let hex = hex::encode(self.signing_key.to_bytes());
        format!("{{\"secret_key_hex\":\"{hex}\"}}\n").into_bytes()
    }

    /// The 32-byte Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Hex encoding of the public key.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key_bytes())
    }
}

/// Preimage the publisher signs: `blake3(DOMAIN || version || root_hash)`, domain-separated against other blobs.
pub fn signing_preimage(version: &str, root_hash_hex: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUNDLE_SIGNING_DOMAIN);
    hasher.update(version.as_bytes());
    hasher.update(root_hash_hex.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Preimage of [`BundleManifest::verity_signature_hex`]:
/// `blake3(VERITY_DOMAIN || version || 0 || root_hash || 0 || lines)`, where
/// `lines` is `<path>\t<verity>\n` for every file with a digest, sorted by
/// path. Payload paths hold no control characters, so the lines parse one way.
pub fn verity_signing_preimage(
    version: &str,
    root_hash_hex: &str,
    files: &[BundleFile],
) -> [u8; 32] {
    let mut sorted: Vec<&BundleFile> = files.iter().filter(|f| f.verity.is_some()).collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = blake3::Hasher::new();
    hasher.update(VERITY_SIGNING_DOMAIN);
    hasher.update(version.as_bytes());
    hasher.update(b"\0");
    hasher.update(root_hash_hex.as_bytes());
    hasher.update(b"\0");
    for file in sorted {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\t");
        hasher.update(file.verity.as_deref().unwrap_or_default().as_bytes());
        hasher.update(b"\n");
    }
    *hasher.finalize().as_bytes()
}

/// Build a bundle at `out` for the host architecture; see [`build_bundle_with`] for the full entry point.
///
/// # Errors
///
/// See [`build_bundle_with`].
pub fn build_bundle(
    out: &Path,
    version: &str,
    from_host: Option<&Path>,
) -> Result<BundleManifest, BundleError> {
    build_bundle_with(
        out,
        version,
        &BuildOptions {
            from: from_host,
            ..Default::default()
        },
    )
}

/// Build (or rebuild) a bundle at `out`.
///
/// `version` is written into `bundle.toml`; `opts.from` overrides the
/// host-root prefix each source path resolves against (default `/`);
/// `opts.arch` picks the source-path table (default [`host_arch`]);
/// `opts.identity`, if set, signs the bundle. Missing source paths
/// are silently skipped, but if *no* source resolves this returns
/// `NoSources` so a wrong `--from` or arch mismatch surfaces immediately.
///
/// # Errors
///
/// Returns [`BundleError::UnsupportedArch`] if `opts.arch` (or
/// [`host_arch`]) has no source-path table; [`BundleError::NoSources`]
/// if none of that architecture's candidate paths exist under
/// `opts.from`; [`BundleError::IncludeMissing`]/[`IncludeUnnamed`] for
/// a bad `opts.extra_libs` entry; [`BundleError::Io`] on a filesystem
/// failure while copying or writing `bundle.toml`; and any error
/// [`Identity::load`] returns when `opts.identity` is set.
///
/// [`IncludeUnnamed`]: BundleError::IncludeUnnamed
pub fn build_bundle_with(
    out: &Path,
    version: &str,
    opts: &BuildOptions<'_>,
) -> Result<BundleManifest, BundleError> {
    let (arch, files) = stage_payload(out, opts)?;
    seal(out, version.to_string(), arch, files, opts.identity)
}

/// Build the runtime build of `release` under `out_root`, named by its ABI:
/// the bundle lands at `<out_root>/<release>+native.<digest>/` with that
/// version in its signed `bundle.toml`, where the digest is
/// [`abi_digest`] of the payload. Returns the manifest and the bundle's
/// directory. An existing bundle at that path is replaced: same name, same
/// bytes.
///
/// # Errors
///
/// [`BundleError::InvalidRelease`] for a release that cannot be a path
/// component or carry the marker, plus everything [`build_bundle_with`]
/// returns.
pub fn build_abi_bundle(
    out_root: &Path,
    release: &str,
    opts: &BuildOptions<'_>,
) -> Result<(BundleManifest, PathBuf), BundleError> {
    if release.is_empty() || release.contains(['/', '+', '\\']) || release.starts_with('.') {
        return Err(BundleError::InvalidRelease(release.to_string()));
    }
    fs::create_dir_all(out_root)?;
    let staging = out_root.join(format!(".staging-{release}-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let built = stage_payload(&staging, opts).and_then(|(arch, files)| {
        let version = abi_version(release, &compute_root_hash(&files));
        seal(&staging, version, arch, files, opts.identity)
    });
    let manifest = match built {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let dest = out_root.join(&manifest.version);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&staging, &dest)?;
    Ok((manifest, dest))
}

/// Copy the payload into `out` and hash it; returns the architecture and the
/// sorted file list.
fn stage_payload(
    out: &Path,
    opts: &BuildOptions<'_>,
) -> Result<(String, Vec<BundleFile>), BundleError> {
    let host_root = opts.from.unwrap_or_else(|| Path::new("/"));
    let arch: &str = opts.arch.unwrap_or_else(|| host_arch());
    let paths = host_minimal_paths_for(arch)
        .ok_or_else(|| BundleError::UnsupportedArch(arch.to_string()))?;
    fs::create_dir_all(out.join("lib"))?;

    // Copy each candidate, tracking what we got so the manifest is deterministic.
    let mut written: Vec<(String, PathBuf)> = Vec::new(); // relative-in-bundle → absolute-in-bundle

    for src_abs in paths {
        let src = resolve_from(host_root, src_abs);
        if !src.exists() {
            continue;
        }
        // Preserve the absolute-path shape under `lib/` (e.g.
        // /lib64/libc.so.6 → lib/lib64/libc.so.6) so LD_LIBRARY_PATH plus the
        // loader's own search finds everything with no symlink dances.
        let rel_no_slash = src_abs.trim_start_matches('/');
        let rel_in_bundle = format!("lib/{rel_no_slash}");
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(&src, &dst)?;
        written.push((rel_in_bundle, dst));
    }

    copy_includes(out, opts.extra_libs, &mut written)?;

    // Optional ld.so.cache.
    let cache_src = resolve_from(host_root, HOST_LD_SO_CACHE);
    if cache_src.exists() {
        let rel_in_bundle = "etc/ld.so.cache".to_string();
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(&cache_src, &dst)?;
        written.push((rel_in_bundle, dst));
    }

    if written.is_empty() {
        return Err(BundleError::NoSources {
            checked: paths.iter().map(|p| resolve_from(host_root, p)).collect(),
        });
    }

    // Hash payload: BLAKE3 for identity, fs-verity for launches.
    let mut files: Vec<BundleFile> = Vec::with_capacity(written.len());
    for (rel, abs) in &written {
        let digests = digest_file(abs, true)?;
        files.push(BundleFile {
            path: rel.clone(),
            hash: digests.blake3,
            size: digests.size,
            verity: digests.verity,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((arch.to_string(), files))
}

/// Sign (when `identity` is set) and write `bundle.toml` for a staged payload.
fn seal(
    out: &Path,
    version: String,
    arch: String,
    files: Vec<BundleFile>,
    identity: Option<&Path>,
) -> Result<BundleManifest, BundleError> {
    let root_hash = compute_root_hash(&files);

    // Optional signing.
    let (publisher_hex, signature_hex, verity_signature_hex) = match identity {
        Some(id_path) => {
            use ed25519_dalek::Signer;

            let identity = Identity::load(id_path)?;
            let preimage = signing_preimage(&version, &root_hash);
            let sig = identity.signing_key.sign(&preimage);
            let verity_sig = files.iter().any(|f| f.verity.is_some()).then(|| {
                let preimage = verity_signing_preimage(&version, &root_hash, &files);
                hex::encode(identity.signing_key.sign(&preimage).to_bytes())
            });
            (
                Some(identity.public_key_hex()),
                Some(hex::encode(sig.to_bytes())),
                verity_sig,
            )
        }
        None => (None, None, None),
    };

    let manifest = BundleManifest {
        version,
        arch,
        root_hash,
        files,
        produced_at: produced_at()?,
        publisher_hex,
        signature_hex,
        verity_signature_hex,
    };

    // Persist bundle.toml.
    let toml =
        toml::to_string_pretty(&manifest).map_err(|e| BundleError::ManifestParse(e.to_string()))?;
    fs::write(out.join(BUNDLE_MANIFEST_FILENAME), toml)?;

    Ok(manifest)
}

/// Verification policy — how strict [`verify_bundle_with`] should be.
/// [`verify_bundle`]'s `default()`: checks any present signature
/// (rejects a bad one), does not require one, and does not enforce a
/// specific publisher.
#[derive(Default, Debug, Clone)]
pub struct VerifyOptions {
    /// If set, require the bundle to be signed AND its publisher hex to equal this string.
    pub require_publisher: Option<String>,
    /// If set, refuse an unsigned bundle even without a specific required publisher.
    pub require_signature: bool,
    /// If set, refuse a bundle whose `arch` disagrees with this string; `None` accepts any arch.
    pub require_arch: Option<String>,
}

/// Verify an existing bundle: recompute every file's hash + the root
/// hash and compare against `bundle.toml`; returns the parsed
/// manifest on success.
///
/// Signature policy: a present signature must verify, but an unsigned
/// bundle is accepted — use [`verify_bundle_with`] with
/// `require_signature=true` to refuse those.
///
/// # Errors
///
/// See [`verify_bundle_with`].
pub fn verify_bundle(root: &Path) -> Result<BundleManifest, BundleError> {
    verify_bundle_with(root, &VerifyOptions::default())
}

/// Full-featured verify. See [`VerifyOptions`].
///
/// # Errors
///
/// Returns [`BundleError::ManifestMissing`]/[`ManifestParse`] if
/// `bundle.toml` is absent or malformed; [`BundleError::ArchMismatch`]
/// if `opts.require_arch` is set and disagrees; [`BundleError::FileMissing`]/
/// [`FileUnlisted`]/[`HashMismatch`] if the on-disk payload and the
/// manifest disagree; [`BundleError::NotSigned`] if `opts` requires a
/// signature the bundle lacks; and [`BundleError::SignatureFieldMalformed`]/
/// [`BadSignature`]/[`PublisherMismatch`] if a present signature is
/// malformed, doesn't verify, or names the wrong publisher.
///
/// [`ManifestParse`]: BundleError::ManifestParse
/// [`FileUnlisted`]: BundleError::FileUnlisted
/// [`HashMismatch`]: BundleError::HashMismatch
/// [`BadSignature`]: BundleError::BadSignature
/// [`PublisherMismatch`]: BundleError::PublisherMismatch
pub fn verify_bundle_with(
    root: &Path,
    opts: &VerifyOptions,
) -> Result<BundleManifest, BundleError> {
    verify_bundle_impl(root, opts, PayloadMode::Hash).map(|(manifest, _)| manifest)
}

/// How a verification established the payload bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadCheck {
    /// Files accepted by their kernel-measured fs-verity digest, unread.
    pub by_verity: usize,
    /// Files whose bytes were read and BLAKE3-hashed.
    pub hashed: usize,
    /// Payload bytes read to hash them.
    pub hashed_bytes: u64,
}

/// [`verify_bundle_with`] for the launch path: the same checks and the same
/// guarantee that every payload byte is the signed byte, without reading
/// the files fs-verity already vouches for.
///
/// A file is accepted unread only when the manifest's
/// [`BundleManifest::verity_signature_hex`] verifies against the bundle's
/// publisher, the file has fs-verity enabled, and the kernel's measured
/// digest equals the signed [`BundleFile::verity`]. fs-verity makes such a
/// file's contents immutable and checks every page read against the
/// measured tree, so equal digests mean equal bytes for as long as the file
/// exists. Every other file (no digest in the manifest, no fs-verity on the
/// filesystem, a digest that differs) is BLAKE3-hashed as
/// [`verify_bundle_with`] does, so this never accepts a byte the full
/// verification would refuse.
///
/// # Errors
///
/// Everything [`verify_bundle_with`] returns, plus
/// [`BundleError::BadVeritySignature`] for a verity signature that does not
/// verify.
pub fn verify_bundle_verity_with(
    root: &Path,
    opts: &VerifyOptions,
) -> Result<(BundleManifest, PayloadCheck), BundleError> {
    verify_bundle_impl(root, opts, PayloadMode::Verity)
}

/// Enable fs-verity on every payload file of the bundle at `root` that
/// `manifest` gives a digest, so [`verify_bundle_verity_with`] can trust it
/// unread. Enabling makes the file immutable and has the kernel read it once
/// to build its Merkle tree. Call it after a full verification: it does not
/// check the bytes itself (a file whose bytes differ measures a different
/// digest and is hashed at launch, then refused).
///
/// # Errors
///
/// The first file the kernel refuses: `ErrorKind::Unsupported` where the
/// filesystem has no fs-verity (launches then hash the payload as before), or
/// `InvalidInput` for a manifest path that leaves the bundle.
pub fn enable_bundle_verity(root: &Path, manifest: &BundleManifest) -> std::io::Result<usize> {
    let mut enabled = 0usize;
    for file in manifest.files.iter().filter(|f| f.verity.is_some()) {
        if !valid_payload_path(&file.path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("payload path {} leaves the bundle", file.path),
            ));
        }
        verity::enable(&root.join(&file.path))?;
        enabled = enabled.saturating_add(1);
    }
    Ok(enabled)
}

/// Verify a bundle's signed manifest, file set and sizes without reading every
/// payload byte. This is appropriate only when the caller independently proves
/// the payload is immutable (the launcher uses it for root-owned system
/// bundles). User-controlled bundles must use [`verify_bundle_with`] or
/// [`verify_bundle_verity_with`].
///
/// # Errors
///
/// Returns the same manifest, file-shape, architecture, and signature errors
/// as [`verify_bundle_with`], plus [`BundleError::SizeMismatch`] when a listed
/// file's length differs. It deliberately does not detect equal-sized content
/// changes; the caller's immutability proof is the protection against those.
pub fn verify_bundle_metadata_with(
    root: &Path,
    opts: &VerifyOptions,
) -> Result<BundleManifest, BundleError> {
    verify_bundle_impl(root, opts, PayloadMode::Size).map(|(manifest, _)| manifest)
}

/// How [`verify_bundle_impl`] establishes each payload file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PayloadMode {
    /// Read and hash every byte.
    Hash,
    /// Compare sizes only; the caller proves immutability.
    Size,
    /// Trust a signed, kernel-measured fs-verity digest; hash otherwise.
    Verity,
}

fn verify_bundle_impl(
    root: &Path,
    opts: &VerifyOptions,
    mode: PayloadMode,
) -> Result<(BundleManifest, PayloadCheck), BundleError> {
    let manifest_path = root.join(BUNDLE_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(BundleError::ManifestMissing {
            path: manifest_path,
        });
    }
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BundleManifest =
        toml::from_str(&text).map_err(|e| BundleError::ManifestParse(e.to_string()))?;

    // Optional arch policy.
    if let Some(required) = opts.require_arch.as_deref()
        && manifest.arch != required
    {
        return Err(BundleError::ArchMismatch {
            actual: manifest.arch,
            host: required.to_string(),
        });
    }

    // A launch checks the verity signature before it relies on any digest
    // under it; the other modes check it last, after the signature they
    // always checked. Every mode refuses a bad one, so the install-time
    // verification vouches for what a launch trusts.
    let verity_signed = mode == PayloadMode::Verity && verify_verity_signature(&manifest)?;
    let mut check = PayloadCheck::default();

    // Recompute per-file hashes and compare against disk: a listed file
    // missing on disk is corruption; an unlisted file present is suspect
    // (unknown-provenance content in a content-addressed dir).
    let mut on_disk: BTreeMap<String, PathBuf> = BTreeMap::new();
    walk_payload(root, &mut on_disk)?;
    // The manifest itself lives at the root; don't treat it as payload.
    on_disk.remove(BUNDLE_MANIFEST_FILENAME);

    let mut listed = BTreeSet::new();
    for expected in &manifest.files {
        if !valid_payload_path(&expected.path) || !listed.insert(expected.path.as_str()) {
            return Err(BundleError::InvalidPayloadPath {
                path: expected.path.clone(),
            });
        }
        let abs = root.join(&expected.path);
        if !abs.exists() {
            return Err(BundleError::FileMissing {
                path: expected.path.clone(),
            });
        }
        if !fs::symlink_metadata(&abs)?.file_type().is_file() {
            return Err(BundleError::NonRegularFile {
                path: expected.path.clone(),
            });
        }
        let by_verity = verity_signed
            && expected
                .verity
                .as_deref()
                .is_some_and(|digest| verity_vouches(&abs, expected.size, digest));
        if by_verity {
            check.by_verity = check.by_verity.saturating_add(1);
        } else if mode == PayloadMode::Size {
            let actual = abs.metadata()?.len();
            if actual != expected.size {
                return Err(BundleError::SizeMismatch {
                    path: expected.path.clone(),
                    expected: expected.size,
                    actual,
                });
            }
        } else {
            let digests = digest_file(&abs, false)?;
            check.hashed = check.hashed.saturating_add(1);
            check.hashed_bytes = check.hashed_bytes.saturating_add(digests.size);
            if digests.blake3 != expected.hash {
                return Err(BundleError::HashMismatch {
                    expected: expected.hash.clone(),
                    computed: digests.blake3,
                });
            }
        }
        on_disk.remove(&expected.path);
    }
    if let Some((path, _)) = on_disk.into_iter().next() {
        return Err(BundleError::FileUnlisted { path });
    }

    // Root hash is what the signature covers, so it's checked BEFORE the
    // signature — a tampered manifest that shifts root_hash without
    // updating files (or vice versa) fails here.
    let mut sorted = manifest.files.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let computed = compute_root_hash(&sorted);
    if computed != manifest.root_hash {
        return Err(BundleError::HashMismatch {
            expected: manifest.root_hash.clone(),
            computed,
        });
    }
    verify_abi_digest(&manifest)?;

    verify_manifest_signature(&manifest, opts)?;
    if mode != PayloadMode::Verity {
        verify_verity_signature(&manifest)?;
    }
    Ok((manifest, check))
}

/// Whether the file at `path` is a regular file of `size` bytes whose
/// kernel-measured fs-verity digest is `expected`. Reads no file data.
fn verity_vouches(path: &Path, size: u64, expected: &str) -> bool {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let Ok(file) = options.open(path) else {
        return false;
    };
    file.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == size)
        && verity::measure(&file).is_some_and(|digest| hex::encode(digest) == expected)
}

/// Whether the manifest carries a valid fs-verity signature: `false` when it
/// has none, an error when it has one that does not verify against the
/// embedded publisher.
fn verify_verity_signature(manifest: &BundleManifest) -> Result<bool, BundleError> {
    use ed25519_dalek::Verifier;

    let Some(sig_hex) = manifest.verity_signature_hex.as_deref() else {
        return Ok(false);
    };
    let publisher_hex = manifest
        .publisher_hex
        .as_deref()
        .ok_or(BundleError::BadVeritySignature)?;
    let pub_bytes: [u8; 32] = decode_fixed_hex(publisher_hex, "publisher_hex")?;
    let sig_bytes: [u8; 64] = decode_fixed_hex(sig_hex, "verity_signature_hex")?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes).map_err(|e| {
        BundleError::SignatureFieldMalformed {
            field: "publisher_hex",
            reason: e.to_string(),
        }
    })?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    let preimage = verity_signing_preimage(&manifest.version, &manifest.root_hash, &manifest.files);
    vk.verify(&preimage, &sig)
        .map(|()| true)
        .map_err(|_| BundleError::BadVeritySignature)
}

/// A version that names an ABI digest must name its own payload's: the
/// digest is what apps are matched against, so a bundle claiming another
/// build's digest would load apps it cannot run.
fn verify_abi_digest(manifest: &BundleManifest) -> Result<(), BundleError> {
    let Some((_, claimed)) = split_abi_version(&manifest.version) else {
        return Ok(());
    };
    let computed = abi_digest(&manifest.root_hash);
    if claimed == computed {
        Ok(())
    } else {
        Err(BundleError::AbiDigestMismatch {
            version: manifest.version.clone(),
            claimed: claimed.to_string(),
            computed,
        })
    }
}

fn verify_manifest_signature(
    manifest: &BundleManifest,
    opts: &VerifyOptions,
) -> Result<(), BundleError> {
    let need_sig = opts.require_signature || opts.require_publisher.is_some();
    match (
        manifest.publisher_hex.as_deref(),
        manifest.signature_hex.as_deref(),
    ) {
        (Some(publisher_hex), Some(sig_hex)) => {
            use ed25519_dalek::Verifier;

            let pub_bytes: [u8; 32] = decode_fixed_hex(publisher_hex, "publisher_hex")?;
            let sig_bytes: [u8; 64] = decode_fixed_hex(sig_hex, "signature_hex")?;

            let vk = ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes).map_err(|e| {
                BundleError::SignatureFieldMalformed {
                    field: "publisher_hex",
                    reason: e.to_string(),
                }
            })?;
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            let preimage = signing_preimage(&manifest.version, &manifest.root_hash);
            if vk.verify(&preimage, &sig).is_err() {
                return Err(BundleError::BadSignature);
            }

            if let Some(required) = opts.require_publisher.as_deref()
                && !eq_hex_ignore_case(publisher_hex, required)
            {
                return Err(BundleError::PublisherMismatch {
                    required: required.to_string(),
                    actual: publisher_hex.to_string(),
                });
            }
        }
        _ if need_sig => return Err(BundleError::NotSigned),
        _ => {}
    }
    Ok(())
}

fn valid_payload_path(path: &str) -> bool {
    !path.is_empty()
        && path != BUNDLE_MANIFEST_FILENAME
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn decode_fixed_hex<const N: usize>(s: &str, field: &'static str) -> Result<[u8; N], BundleError> {
    let raw = hex::decode(s).map_err(|e| BundleError::SignatureFieldMalformed {
        field,
        reason: e.to_string(),
    })?;
    if raw.len() != N {
        return Err(BundleError::SignatureFieldMalformed {
            field,
            reason: format!("expected {N} bytes, got {}", raw.len()),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&raw);
    Ok(out)
}

fn eq_hex_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// List the immediate contents of the bundle root that constitute the payload (used by `verify_bundle`); recursive.
fn walk_payload(root: &Path, out: &mut BTreeMap<String, PathBuf>) -> std::io::Result<()> {
    fn recurse(
        base: &Path,
        cur: &Path,
        out: &mut BTreeMap<String, PathBuf>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(cur)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                recurse(base, &path, out)?;
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, path);
            }
        }
        Ok(())
    }
    recurse(root, root, out)
}

/// BLAKE3 root: hash of the newline-delimited "<path>\t<file-hash>"
/// concatenation, sorted by path — stable across mtime/permissions/traversal order.
fn compute_root_hash(files: &[BundleFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    for f in files {
        hasher.update(f.path.as_bytes());
        hasher.update(b"\t");
        hasher.update(f.hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// A payload file's digests.
struct FileDigests {
    /// BLAKE3, hex: the file's identity in the manifest and the root hash.
    blake3: String,
    /// fs-verity digest, hex, when asked for.
    verity: Option<String>,
    size: u64,
}

/// Read `path` once, BLAKE3-hashing it and, with `with_verity`, computing its
/// fs-verity digest.
fn digest_file(path: &Path, with_verity: bool) -> Result<FileDigests, BundleError> {
    let mut f = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut verity = with_verity.then(verity::VerityHasher::new);
    // Large reads let BLAKE3 hash many chunks in parallel SIMD lanes.
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // `Read::read` never returns more bytes than the buffer it was
        // given, so `n` is always in-bounds; `.get()` + an empty
        // fallback avoids indexing without changing behaviour.
        let bytes = buf.get(..n).unwrap_or_default();
        hasher.update(bytes);
        if let Some(verity) = verity.as_mut() {
            verity.update(bytes);
        }
        total = total.saturating_add(n as u64);
    }
    Ok(FileDigests {
        blake3: hasher.finalize().to_hex().to_string(),
        verity: verity.map(|verity| hex::encode(verity.finalize())),
        size: total,
    })
}

/// Extra libs (libstd, `libruntime_dylib.so`) copy verbatim to the top of
/// `lib/` by file name for soname resolution via `LD_LIBRARY_PATH` — unlike
/// the loader/libc set, these are explicit absolute paths, not resolved
/// against the host root. A directory (an engine with its resources) keeps
/// its tree under `lib/<name>/`. Repeated source paths are copied once.
fn copy_includes(
    out: &Path,
    includes: &[PathBuf],
    written: &mut Vec<(String, PathBuf)>,
) -> Result<(), BundleError> {
    let mut sources = BTreeSet::new();
    for extra in includes {
        if !sources.insert(extra) {
            continue;
        }
        let name = extra
            .file_name()
            .ok_or_else(|| BundleError::IncludeUnnamed {
                path: extra.clone(),
            })?
            .to_string_lossy()
            .to_string();
        if extra.is_dir() {
            copy_tree(
                extra,
                &out.join("lib").join(&name),
                &format!("lib/{name}"),
                written,
            )?;
            continue;
        }
        if !extra.exists() {
            return Err(BundleError::IncludeMissing {
                path: extra.clone(),
            });
        }
        let rel_in_bundle = format!("lib/{name}");
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(extra, &dst)?;
        written.push((rel_in_bundle, dst));
    }
    Ok(())
}

/// Copy the tree at `src` to `dst`, recording each file as `rel/<path>`.
/// Links and special files are refused: a bundle is plain bytes.
fn copy_tree(
    src: &Path,
    dst: &Path,
    rel: &str,
    written: &mut Vec<(String, PathBuf)>,
) -> Result<(), BundleError> {
    fs::create_dir_all(dst)?;
    let mut entries = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let kind = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        let (from, to, rel) = (entry.path(), dst.join(&name), format!("{rel}/{name}"));
        if kind.is_dir() {
            copy_tree(&from, &to, &rel, written)?;
        } else if kind.is_file() {
            copy_file(&from, &to)?;
            written.push((rel, to));
        } else {
            return Err(BundleError::IncludeSpecialFile { path: from });
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), BundleError> {
    // `fs::copy` follows symlinks (intentional): the bundle must capture
    // actual bytes so a sandbox mount doesn't depend on the symlink
    // target being reachable.
    fs::copy(src, dst)?;
    Ok(())
}

fn resolve_from(host_root: &Path, abs: &str) -> PathBuf {
    if host_root == Path::new("/") {
        return PathBuf::from(abs);
    }
    // Strip the leading slash so `join()` doesn't discard host_root.
    let rel = abs.trim_start_matches('/');
    host_root.join(rel)
}

fn produced_at() -> Result<u64, BundleError> {
    produced_at_from(std::env::var("SOURCE_DATE_EPOCH"))
}

fn produced_at_from(value: Result<String, std::env::VarError>) -> Result<u64, BundleError> {
    match value {
        Ok(value) => value
            .parse()
            .map_err(|_| BundleError::InvalidSourceDateEpoch(value)),
        Err(std::env::VarError::NotPresent) => Ok(unix_now()),
        Err(std::env::VarError::NotUnicode(value)) => Err(BundleError::InvalidSourceDateEpoch(
            value.to_string_lossy().into_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_font_catalog_is_signed_and_corruption_does_not_change_a_retained_sdk() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let work = tempfile::tempdir().unwrap();
        let fonts = work.path().join("fonts");
        let prepared = std::process::Command::new("python3")
            .arg(repo.join("tools/font-catalog.py"))
            .arg("prepare")
            .arg(repo.join("crates/sdk/ui-core/assets/fonts"))
            .arg(&fonts)
            .args(["--scope", "product"])
            .output()
            .unwrap();
        assert!(
            prepared.status.success(),
            "{}",
            String::from_utf8_lossy(&prepared.stderr)
        );
        let identity = Identity::generate();
        let identity_path = work.path().join("identity.json");
        fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let bundles = work.path().join("bundles");
        let includes = [fonts];
        let options = BuildOptions {
            identity: Some(&identity_path),
            extra_libs: &includes,
            ..Default::default()
        };
        let (_, held) = build_abi_bundle(&bundles, "held", &options).unwrap();
        let (_, current) = build_abi_bundle(&bundles, "current", &options).unwrap();
        let verification = VerifyOptions {
            require_publisher: Some(identity.public_key_hex()),
            require_signature: true,
            require_arch: None,
        };
        let manifest = verify_bundle_with(&current, &verification).unwrap();
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file.path == "lib/fonts/manifest.toml")
        );
        let original = fs::read(held.join("lib/fonts/NotoSansMath-Regular.ttf")).unwrap();
        let changed = current.join("lib/fonts/NotoSansMath-Regular.ttf");
        let mut bytes = fs::read(&changed).unwrap();
        bytes[0] ^= 1;
        fs::write(changed, bytes).unwrap();
        assert!(verify_bundle_with(&current, &verification).is_err());
        verify_bundle_with(&held, &verification).unwrap();
        assert_eq!(
            fs::read(held.join("lib/fonts/NotoSansMath-Regular.ttf")).unwrap(),
            original
        );
    }

    #[test]
    fn an_abi_bundle_is_named_by_its_payload_and_cannot_claim_another_build() {
        // phase: the_version_carries_the_payload_digest
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let libs = tempfile::tempdir().unwrap();
        let dylib = libs.path().join("libruntime_dylib.so");
        std::fs::write(&dylib, b"runtime build one").unwrap();
        let root = tempfile::tempdir().unwrap();
        let build = |extra: &[PathBuf]| {
            build_abi_bundle(
                root.path(),
                "2026.07",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: extra,
                },
            )
            .expect("build")
        };
        let (first, first_dir) = build(std::slice::from_ref(&dylib));
        let (release, digest) = split_abi_version(&first.version).expect("abi version");
        assert_eq!(release, "2026.07");
        assert_eq!(digest, abi_digest(&first.root_hash));
        assert_eq!(digest.len(), ABI_DIGEST_HEX_LEN);
        assert_eq!(first_dir, root.path().join(&first.version));
        verify_bundle(&first_dir).expect("verify");

        // phase: rebuilding_the_same_bytes_names_the_same_build
        let (again, _) = build(std::slice::from_ref(&dylib));
        assert_eq!(again.version, first.version);

        // phase: a_different_dylib_is_a_different_runtime
        std::fs::write(&dylib, b"runtime build two").unwrap();
        let (second, second_dir) = build(std::slice::from_ref(&dylib));
        assert_ne!(second.version, first.version);
        assert!(first_dir.exists() && second_dir.exists());

        // phase: a_bundle_claiming_another_builds_digest_is_refused
        let text = std::fs::read_to_string(second_dir.join(BUNDLE_MANIFEST_FILENAME)).unwrap();
        let forged = text.replace(&second.version, &first.version);
        std::fs::write(second_dir.join(BUNDLE_MANIFEST_FILENAME), forged).unwrap();
        assert!(matches!(
            verify_bundle(&second_dir),
            Err(BundleError::AbiDigestMismatch { .. })
        ));

        // phase: a_release_must_be_one_path_component
        for bad in ["", "2026/07", "2026.07+native.00", ".hidden"] {
            assert!(matches!(
                build_abi_bundle(root.path(), bad, &BuildOptions::default()),
                Err(BundleError::InvalidRelease(_))
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn build_produces_deterministic_verifiable_bundles_per_layout() {
        // phase: source_date_epoch_controls_produced_at
        {
            assert_eq!(produced_at_from(Ok("123456".into())).unwrap(), 123_456);
            assert!(matches!(
                produced_at_from(Ok("not-a-timestamp".into())),
                Err(BundleError::InvalidSourceDateEpoch(_))
            ));
        }
        // phase: build_then_verify_roundtrips_x86_64
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            let manifest = build_bundle_with(
                out.path(),
                "2026.07",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .expect("build");
            assert_eq!(manifest.version, "2026.07");
            assert_eq!(manifest.arch, "x86_64");
            assert!(!manifest.files.is_empty());
            assert_eq!(manifest.root_hash.len(), 64);
            assert!(manifest.signature_hex.is_none());

            let verified = verify_bundle(out.path()).expect("verify");
            assert_eq!(verified.root_hash, manifest.root_hash);
            assert_eq!(verified.arch, "x86_64");
        }
        // phase: build_supports_aarch64_fedora_layout
        {
            let host = tempfile::tempdir().unwrap();
            fake_aarch64_fedora_host(host.path());
            let out = tempfile::tempdir().unwrap();
            let m = build_bundle_with(
                out.path(),
                "1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("aarch64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .expect("build");
            assert_eq!(m.arch, "aarch64");
            // Fedora layout: loader at /lib/, libc at /lib64/.
            let paths: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with("lib/ld-linux-aarch64.so.1"))
            );
            assert!(paths.iter().any(|p| p.ends_with("lib64/libc.so.6")));
            verify_bundle(out.path()).expect("verify");
        }
        // phase: build_supports_aarch64_debian_layout
        {
            let host = tempfile::tempdir().unwrap();
            fake_aarch64_debian_host(host.path());
            let out = tempfile::tempdir().unwrap();
            let m = build_bundle_with(
                out.path(),
                "1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("aarch64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .expect("build");
            let paths: Vec<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with("aarch64-linux-gnu/libc.so.6"))
            );
            verify_bundle(out.path()).expect("verify");
        }
        // phase: unsupported_arch_errors
        {
            let host = tempfile::tempdir().unwrap();
            let out = tempfile::tempdir().unwrap();
            let err = build_bundle_with(
                out.path(),
                "1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("mips64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap_err();
            matches!(err, BundleError::UnsupportedArch(_));
        }
        // phase: build_fails_when_no_sources_resolve
        {
            let host = tempfile::tempdir().unwrap();
            let out = tempfile::tempdir().unwrap();
            let err = build_bundle_with(
                out.path(),
                "0.0.1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap_err();
            matches!(err, BundleError::NoSources { .. });
        }
        // phase: root_hash_is_deterministic
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out1 = tempfile::tempdir().unwrap();
            let out2 = tempfile::tempdir().unwrap();
            let opts = BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: None,
                extra_libs: &[],
            };
            let a = build_bundle_with(out1.path(), "v", &opts).unwrap();
            let b = build_bundle_with(out2.path(), "v", &opts).unwrap();
            assert_eq!(a.root_hash, b.root_hash);
        }
        // phase: extra_libs_land_at_lib_root_and_are_hashed_and_verify
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());

            // Two stand-in shared objects a thin app links dynamically (Rust std
            // + Tilo runtime dylib); content is arbitrary — the point is they land
            // in the bundle, content-addressed, and pass verification.
            let extras_dir = tempfile::tempdir().unwrap();
            let libstd = extras_dir.path().join("libstd-deadbeef.so");
            let libruntime = extras_dir.path().join("libruntime_dylib.so");
            fs::write(&libstd, b"fake libstd bytes").unwrap();
            fs::write(&libruntime, b"fake libruntime bytes").unwrap();

            let out = tempfile::tempdir().unwrap();
            let manifest = build_bundle_with(
                out.path(),
                "thin-1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[libstd, libruntime],
                },
            )
            .expect("build with extras");

            // Both extras appear at the TOP of lib/ (by file name) for soname resolution via LD_LIBRARY_PATH.
            assert!(out.path().join("lib/libstd-deadbeef.so").exists());
            assert!(out.path().join("lib/libruntime_dylib.so").exists());

            // Both are recorded in the manifest — hashed, covered by the root hash, and thus any signature.
            let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
            assert!(paths.contains(&"lib/libstd-deadbeef.so"), "{paths:?}");
            assert!(paths.contains(&"lib/libruntime_dylib.so"), "{paths:?}");

            // Full verify roundtrips — proves the extras are part of the
            // content-addressed set, not loose files verify would reject.
            verify_bundle(out.path()).expect("verify with extras");
        }
        // phase: an_included_directory_keeps_its_tree
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let engine = tempfile::tempdir().unwrap();
            let engine_dir = engine.path().join("chromium");
            std::fs::create_dir_all(engine_dir.join("locales")).unwrap();
            std::fs::write(engine_dir.join("tilo-chromium-host"), b"host").unwrap();
            std::fs::write(engine_dir.join("locales/en-US.pak"), b"pak").unwrap();
            let out = tempfile::tempdir().unwrap();
            let manifest = build_bundle_with(
                out.path(),
                "2026.10.0-engine",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: std::slice::from_ref(&engine_dir),
                },
            )
            .expect("build with a directory");
            let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
            assert!(
                paths.contains(&"lib/chromium/tilo-chromium-host"),
                "{paths:?}"
            );
            assert!(
                paths.contains(&"lib/chromium/locales/en-US.pak"),
                "{paths:?}"
            );
            verify_bundle(out.path()).expect("verify with a directory");

            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("tilo-chromium-host", engine_dir.join("link")).unwrap();
                let out = tempfile::tempdir().unwrap();
                let err = build_bundle_with(
                    out.path(),
                    "2026.10.0-engine",
                    &BuildOptions {
                        from: Some(host.path()),
                        arch: Some("x86_64"),
                        identity: None,
                        extra_libs: std::slice::from_ref(&engine_dir),
                    },
                )
                .unwrap_err();
                assert!(matches!(err, BundleError::IncludeSpecialFile { .. }));
            }
        }
        // phase: missing_extra_lib_is_an_error
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            let missing = out.path().join("does-not-exist.so");
            let err = build_bundle_with(
                out.path(),
                "thin-1",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[missing],
                },
            )
            .unwrap_err();
            assert!(matches!(err, BundleError::IncludeMissing { .. }));
        }
    }

    #[test]
    fn repeated_extra_inputs_build_one_signed_abi_payload() {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let identity = Identity::generate();
        let identity_path = host.path().join("identity.json");
        fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let libstd = host.path().join("libstd-fixture.so");
        let libruntime = host.path().join("libruntime_dylib.so");
        fs::write(&libstd, b"Rust standard library fixture").unwrap();
        fs::write(&libruntime, b"Tilo runtime fixture").unwrap();
        let unique = tempfile::tempdir().unwrap();
        let repeated = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: Some(&identity_path),
            extra_libs: &[libstd.clone(), libruntime.clone()],
        };
        let (expected, _) = build_abi_bundle(unique.path(), "2026.07", &opts).unwrap();
        // Explicit includes and ldd's transitive closure name the same inputs.
        let (actual, root) = build_abi_bundle(
            repeated.path(),
            "2026.07",
            &BuildOptions {
                extra_libs: &[libruntime.clone(), libstd.clone(), libstd, libruntime],
                ..opts
            },
        )
        .unwrap();
        verify_bundle_with(
            &root,
            &VerifyOptions {
                require_publisher: Some(identity.public_key_hex()),
                require_signature: true,
                require_arch: Some("x86_64".to_string()),
            },
        )
        .expect("repeated source inputs still produce a valid signed bundle");
        assert_eq!(actual.version, expected.version);
        assert_eq!(actual.root_hash, expected.root_hash);
        assert_eq!(actual.files.len(), expected.files.len());
    }

    #[test]
    fn different_extra_inputs_with_the_same_name_are_still_rejected() {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let name = "libruntime_dylib.so";
        fs::write(first.path().join(name), b"first runtime").unwrap();
        fs::write(second.path().join(name), b"conflicting runtime").unwrap();
        let out = tempfile::tempdir().unwrap();
        let (_, root) = build_abi_bundle(
            out.path(),
            "2026.07",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: None,
                extra_libs: &[first.path().join(name), second.path().join(name)],
            },
        )
        .unwrap();
        assert!(matches!(
            verify_bundle(&root),
            Err(BundleError::InvalidPayloadPath { path }) if path == format!("lib/{name}")
        ));
    }

    fn fake_x86_64_host(root: &Path) {
        for rel in [
            "lib64/ld-linux-x86-64.so.2",
            "lib64/libc.so.6",
            "lib64/libm.so.6",
        ] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
    }

    fn fake_aarch64_fedora_host(root: &Path) {
        // Fedora-style aarch64 rootfs: loader at /lib/, libc at /lib64/.
        for rel in [
            "lib/ld-linux-aarch64.so.1",
            "lib64/libc.so.6",
            "lib64/libm.so.6",
            "lib64/libgcc_s.so.1",
        ] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
    }

    fn fake_aarch64_debian_host(root: &Path) {
        for rel in [
            "lib/ld-linux-aarch64.so.1",
            "lib/aarch64-linux-gnu/libc.so.6",
            "lib/aarch64-linux-gnu/libm.so.6",
            "lib/aarch64-linux-gnu/libgcc_s.so.1",
        ] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn verify_rejects_modified_unlisted_and_malformed_payloads() {
        // phase: verify_detects_a_modified_byte
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            build_bundle_with(
                out.path(),
                "2026.07",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap();
            let libc = out.path().join("lib/lib64/libc.so.6");
            let mut bytes = fs::read(&libc).unwrap();
            bytes[0] ^= 0xff;
            fs::write(&libc, bytes).unwrap();

            let err = verify_bundle(out.path()).unwrap_err();
            matches!(err, BundleError::HashMismatch { .. });
        }
        // phase: verify_detects_unlisted_file
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            build_bundle_with(
                out.path(),
                "2026.07",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap();
            fs::write(out.path().join("lib/lib64/extra.so"), b"surprise").unwrap();
            let err = verify_bundle(out.path()).unwrap_err();
            matches!(err, BundleError::FileUnlisted { .. });
        }
        // phase: metadata_verify_checks_shape_but_deliberately_not_content
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let identity_path = host.path().join("identity.json");
            fs::write(&identity_path, Identity::generate().to_json_bytes()).unwrap();
            let out = tempfile::tempdir().unwrap();
            let manifest = build_bundle_with(
                out.path(),
                "system-fast-path",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: Some(&identity_path),
                    extra_libs: &[],
                },
            )
            .unwrap();
            let opts = VerifyOptions {
                require_signature: true,
                ..VerifyOptions::default()
            };
            verify_bundle_metadata_with(out.path(), &opts).expect("untouched metadata verifies");

            let payload = out.path().join(&manifest.files[0].path);
            let mut same_size = fs::read(&payload).unwrap();
            same_size[0] ^= 1;
            fs::write(&payload, &same_size).unwrap();
            verify_bundle_metadata_with(out.path(), &opts)
                .expect("caller-owned immutability is what protects equal-sized content");
            assert!(matches!(
                verify_bundle_with(out.path(), &opts),
                Err(BundleError::HashMismatch { .. })
            ));

            fs::write(&payload, [same_size, vec![0]].concat()).unwrap();
            assert!(matches!(
                verify_bundle_metadata_with(out.path(), &opts),
                Err(BundleError::SizeMismatch { .. })
            ));
        }
        // phase: verify_rejects_traversal_and_duplicate_manifest_paths
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            let mut manifest = build_bundle_with(
                out.path(),
                "malformed-paths",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap();
            manifest.files[0].path = "../outside".to_string();
            fs::write(
                out.path().join(BUNDLE_MANIFEST_FILENAME),
                toml::to_string_pretty(&manifest).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                verify_bundle(out.path()),
                Err(BundleError::InvalidPayloadPath { .. })
            ));

            let mut manifest = build_bundle_with(
                out.path(),
                "duplicate-paths",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap();
            manifest.files.push(manifest.files[0].clone());
            fs::write(
                out.path().join(BUNDLE_MANIFEST_FILENAME),
                toml::to_string_pretty(&manifest).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                verify_bundle(out.path()),
                Err(BundleError::InvalidPayloadPath { .. })
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn bundle_signatures_bind_the_publisher_and_domain() {
        // phase: signed_bundle_roundtrips_and_verify_accepts
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let identity_path = host.path().join("identity.json");
            let identity = Identity::generate();
            fs::write(&identity_path, identity.to_json_bytes()).unwrap();

            let out = tempfile::tempdir().unwrap();
            let manifest = build_bundle_with(
                out.path(),
                "signed",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: Some(&identity_path),
                    extra_libs: &[],
                },
            )
            .expect("build signed");
            assert_eq!(
                manifest.publisher_hex.as_deref(),
                Some(identity.public_key_hex()).as_deref()
            );
            assert!(manifest.signature_hex.is_some());

            // Default verify accepts a valid signed bundle.
            let v = verify_bundle(out.path()).expect("verify");
            assert_eq!(v.publisher_hex, manifest.publisher_hex);

            // require_publisher matching same key accepts.
            verify_bundle_with(
                out.path(),
                &VerifyOptions {
                    require_publisher: Some(identity.public_key_hex()),
                    require_signature: true,
                    require_arch: None,
                },
            )
            .expect("required-publisher matches");
        }
        // phase: verify_refuses_bad_publisher
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let identity_path = host.path().join("identity.json");
            let identity = Identity::generate();
            fs::write(&identity_path, identity.to_json_bytes()).unwrap();

            let out = tempfile::tempdir().unwrap();
            build_bundle_with(
                out.path(),
                "signed",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: Some(&identity_path),
                    extra_libs: &[],
                },
            )
            .unwrap();

            let other = Identity::generate();
            let err = verify_bundle_with(
                out.path(),
                &VerifyOptions {
                    require_publisher: Some(other.public_key_hex()),
                    require_signature: true,
                    require_arch: None,
                },
            )
            .unwrap_err();
            matches!(err, BundleError::PublisherMismatch { .. });
        }
        // phase: verify_refuses_tampered_signature
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let identity_path = host.path().join("identity.json");
            let identity = Identity::generate();
            fs::write(&identity_path, identity.to_json_bytes()).unwrap();

            let out = tempfile::tempdir().unwrap();
            build_bundle_with(
                out.path(),
                "signed",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: Some(&identity_path),
                    extra_libs: &[],
                },
            )
            .unwrap();

            // Root-hash tampering is caught by the integrity check before the
            // signature; to test the signature path specifically, flip a byte in
            // signature_hex instead.
            let bundle_toml = out.path().join(BUNDLE_MANIFEST_FILENAME);
            let text = fs::read_to_string(&bundle_toml).unwrap();
            let mut manifest: BundleManifest = toml::from_str(&text).unwrap();
            let sig = manifest.signature_hex.as_mut().unwrap();
            // Flip one hex nibble deterministically.
            let bytes: &mut [u8] = unsafe { sig.as_bytes_mut() };
            bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
            fs::write(&bundle_toml, toml::to_string_pretty(&manifest).unwrap()).unwrap();

            let err = verify_bundle(out.path()).unwrap_err();
            matches!(err, BundleError::BadSignature);
        }
        // phase: require_signature_refuses_unsigned_bundle
        {
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            build_bundle_with(
                out.path(),
                "unsigned",
                &BuildOptions {
                    from: Some(host.path()),
                    arch: Some("x86_64"),
                    identity: None,
                    extra_libs: &[],
                },
            )
            .unwrap();
            let err = verify_bundle_with(
                out.path(),
                &VerifyOptions {
                    require_signature: true,
                    ..VerifyOptions::default()
                },
            )
            .unwrap_err();
            matches!(err, BundleError::NotSigned);
        }
        // phase: signing_preimage_is_domain_separated
        {
            let a = signing_preimage("v1", "aa");
            let b = signing_preimage("v2", "aa");
            let c = signing_preimage("v1", "bb");
            assert_ne!(a, b);
            assert_ne!(a, c);
            // Domain prefix cannot be reproduced from bare inputs.
            let hasher_bare = {
                let mut h = blake3::Hasher::new();
                h.update(b"v1");
                h.update(b"aa");
                *h.finalize().as_bytes()
            };
            assert_ne!(a, hasher_bare);
        }
    }

    #[cfg(unix)]
    #[test]
    fn verify_rejects_non_regular_payload_entries() {
        use std::os::unix::fs::symlink;

        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let out = tempfile::tempdir().unwrap();
        let mut manifest = build_bundle_with(
            out.path(),
            "plain-files-only",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: None,
                extra_libs: &[],
            },
        )
        .unwrap();
        symlink("lib/lib64/libc.so.6", out.path().join("unexpected-link")).unwrap();
        assert!(matches!(
            verify_bundle(out.path()),
            Err(BundleError::FileUnlisted { path }) if path == "unexpected-link"
        ));

        let digests = digest_file(&out.path().join("unexpected-link"), false).unwrap();
        manifest.files.push(BundleFile {
            path: "unexpected-link".to_string(),
            hash: digests.blake3,
            size: digests.size,
            verity: None,
        });
        manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
        manifest.root_hash = compute_root_hash(&manifest.files);
        fs::write(
            out.path().join(BUNDLE_MANIFEST_FILENAME),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            verify_bundle(out.path()),
            Err(BundleError::NonRegularFile { path }) if path == "unexpected-link"
        ));
    }

    #[test]
    fn bundle_root_resolves_from_the_environment_or_data_dir() {
        // phase: default_bundle_path_uses_data_dir
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove(RUNTIME_DIR_ENV);
            if let Ok(p) = default_bundle_path("v") {
                assert!(p.ends_with("tilo-runtime/v"));
            }
        }
        // phase: the_env_override_names_the_bundle_root
        // A session that repoints `XDG_DATA_HOME` for its own documents must
        // still reach the machine's runtime bundles, or every app in that
        // session fails to launch on a missing bundle.
        {
            let shared = tempfile::tempdir().expect("tempdir");
            let resolved = {
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set("XDG_DATA_HOME", "/nonexistent/session/data")
                    .set(RUNTIME_DIR_ENV, shared.path());
                default_bundle_path("2026.07")
            };
            assert_eq!(
                resolved.expect("the override needs no data dir"),
                shared.path().join("2026.07"),
                "the override must win over an isolated XDG_DATA_HOME",
            );
        }
        // phase: an_empty_override_falls_back_to_the_data_dir
        // An empty value is not a path — treating it as one resolves to
        // `/<version>` (nowhere), turning a stray `TILO_RUNTIME_BUNDLE_DIR=`
        // into "no app on this machine launches".
        {
            let resolved = {
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set(RUNTIME_DIR_ENV, "");
                default_bundle_path("v")
            };
            if let Ok(p) = resolved {
                assert!(p.ends_with("tilo-runtime/v"), "got {p:?}");
            }
        }
    }
}
