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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

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
    Ok(xdg_paths::data_dir()
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

    // Extra libs (libstd, libruntime_dylib.so) copy verbatim to the TOP
    // of lib/ by file name for soname resolution via LD_LIBRARY_PATH —
    // unlike the loader/libc set, these are explicit absolute paths, not
    // resolved against host_root.
    for extra in opts.extra_libs {
        if !extra.exists() {
            return Err(BundleError::IncludeMissing {
                path: extra.clone(),
            });
        }
        let name = extra
            .file_name()
            .ok_or_else(|| BundleError::IncludeUnnamed {
                path: extra.clone(),
            })?
            .to_string_lossy()
            .to_string();
        let rel_in_bundle = format!("lib/{name}");
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(extra, &dst)?;
        written.push((rel_in_bundle, dst));
    }

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

    // Hash payload.
    let mut files: Vec<BundleFile> = Vec::with_capacity(written.len());
    for (rel, abs) in &written {
        let (hash, size) = hash_file(abs)?;
        files.push(BundleFile {
            path: rel.clone(),
            hash,
            size,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let root_hash = compute_root_hash(&files);

    // Optional signing.
    let (publisher_hex, signature_hex) = match opts.identity {
        Some(id_path) => {
            use ed25519_dalek::Signer;

            let identity = Identity::load(id_path)?;
            let preimage = signing_preimage(version, &root_hash);
            let sig = identity.signing_key.sign(&preimage);
            (
                Some(identity.public_key_hex()),
                Some(hex::encode(sig.to_bytes())),
            )
        }
        None => (None, None),
    };

    let manifest = BundleManifest {
        version: version.to_string(),
        arch: arch.to_string(),
        root_hash,
        files,
        produced_at: produced_at()?,
        publisher_hex,
        signature_hex,
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
    verify_bundle_impl(root, opts, true)
}

/// Verify a bundle's signed manifest, file set and sizes without reading every
/// payload byte. This is appropriate only when the caller independently proves
/// the payload is immutable (the launcher uses it for root-owned system
/// bundles). User-controlled bundles must use [`verify_bundle_with`].
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
    verify_bundle_impl(root, opts, false)
}

fn verify_bundle_impl(
    root: &Path,
    opts: &VerifyOptions,
    hash_payload: bool,
) -> Result<BundleManifest, BundleError> {
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
        if hash_payload {
            let (hash, _size) = hash_file(&abs)?;
            if hash != expected.hash {
                return Err(BundleError::HashMismatch {
                    expected: expected.hash.clone(),
                    computed: hash,
                });
            }
        } else {
            let actual = abs.metadata()?.len();
            if actual != expected.size {
                return Err(BundleError::SizeMismatch {
                    path: expected.path.clone(),
                    expected: expected.size,
                    actual,
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

    verify_manifest_signature(&manifest, opts)?;
    Ok(manifest)
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

fn hash_file(path: &Path) -> Result<(String, u64), BundleError> {
    let mut f = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8192];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // `Read::read` never returns more bytes than the buffer it was
        // given, so `n` is always in-bounds; `.get()` + an empty
        // fallback avoids indexing without changing behaviour.
        hasher.update(buf.get(..n).unwrap_or_default());
        total = total.saturating_add(n as u64);
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
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

        let (hash, size) = hash_file(&out.path().join("unexpected-link")).unwrap();
        manifest.files.push(BundleFile {
            path: "unexpected-link".to_string(),
            hash,
            size,
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
