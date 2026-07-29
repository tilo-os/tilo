//! qOS runtime bundle producer + verifier.
//!
//! ## What a runtime bundle is
//!
//! A directory laid out as
//!
//! ```text
//! <root>/
//!   bundle.toml           metadata: version, root hash, payload manifest, signature
//!   lib/                  host-copied loader + libc + friends
//!     ld-linux-x86-64.so.2       (or ld-linux-aarch64.so.1 for aarch64 bundles)
//!     libc.so.6
//!     libgcc_s.so.1
//!     …
//!   etc/ld.so.cache       (optional; copied from host if present)
//! ```
//!
//! Content-addressed: `bundle.toml.root_hash` is a BLAKE3 root over the
//! sorted list of `(payload-relative-path, blake3(file))` pairs. Any
//! byte change in any file — or in the manifest of paths itself —
//! flips the root hash.
//!
//! ## Cross-architecture
//!
//! The set of host source paths depends on target architecture. The
//! [`host_minimal_paths_for`] table encodes what to copy for each
//! architecture qOS targets (`x86_64`, `aarch64` today). At build time
//! we probe the host for existence and silently skip anything absent —
//! Debian layouts (`/lib/aarch64-linux-gnu/libc.so.6`) and Fedora
//! layouts (`/usr/lib64/libc.so.6`) are both listed, only what exists
//! is copied, the manifest records what was actually included.
//!
//! ## Signing
//!
//! When [`build_bundle_with`] is given a [`BuildOptions::identity`],
//! it appends a detached ed25519 signature to `bundle.toml`:
//!
//! - `publisher_hex` — the identity's 32-byte public key, hex.
//! - `signature_hex` — the 64-byte signature, hex.
//!
//! Preimage: `blake3(b"qos-runtime-bundle-v1\0" || version || root_hash)`.
//!
//! [`verify_bundle`] recomputes the hash covenants and (when a
//! signature is present) checks the signature against the embedded
//! publisher. Callers can also require a specific publisher via
//! [`verify_bundle_with`] — see the launcher's `--require-publisher`
//! passthrough.
//!
//! ## Discovery
//!
//! The launcher looks a bundle up by its version tag. Search order:
//! 1. explicit `--runtime <path>` from the caller (not the library's job)
//! 2. `$XDG_DATA_HOME/qos-runtime/<version>/` — per-user (default when
//!    installed with `qos-install runtime install`).
//! 3. `/usr/lib/qos-runtime/<version>/` — system-wide (baked into the
//!    OS image at build time by `os/scripts/bake-runtime-bundle.sh`).
//!
//! The `default_bundle_path` and `system_bundle_path` helpers surface
//! these paths from the library so both the CLI and the launcher agree
//! on where to look.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Standard bundle manifest filename inside a bundle directory.
pub const BUNDLE_MANIFEST_FILENAME: &str = "bundle.toml";

/// Domain-separation prefix for the bundle-signing preimage. Bumped
/// only on a wire-breaking change.
pub const BUNDLE_SIGNING_DOMAIN: &[u8] = b"qos-runtime-bundle-v1\0";

/// Bundle metadata written into `bundle.toml`. All fields are stable
/// so that a `verify` roundtrip is meaningful across tool versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Human-readable version tag — matches the manifest string
    /// `qos-runtime/<version>` in a package manifest.
    pub version: String,
    /// Target architecture the bundle was built for. Matches
    /// `std::env::consts::ARCH` values (`x86_64`, `aarch64`, ...).
    /// A launcher on the wrong arch refuses the bundle rather than
    /// getting the loader wrong at exec time.
    pub arch: String,
    /// BLAKE3 root hash over the sorted `(path, file-hash)` list.
    /// Hex, lowercase, 64 chars.
    pub root_hash: String,
    /// Sorted list of every file in the bundle (relative to the
    /// bundle root) with its individual BLAKE3. Sorted so the
    /// encoding is deterministic across filesystem traversal
    /// orderings.
    #[serde(default)]
    pub files: Vec<BundleFile>,
    /// Produced-at unix time. Informational only — the hash is what
    /// matters for identity.
    #[serde(default)]
    pub produced_at: u64,
    /// Optional publisher's Ed25519 public key, hex-encoded (64
    /// chars). Present iff the bundle was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_hex: Option<String>,
    /// Optional Ed25519 signature over
    /// `blake3(DOMAIN || version || root_hash)`, hex-encoded (128
    /// chars). Present iff the bundle was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hex: Option<String>,
}

/// One `(payload-relative-path, hash)` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Path relative to the bundle root, always using `/` as
    /// separator so the encoding is portable.
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
    #[error("home / data-dir not resolvable — set XDG_DATA_HOME or HOME")]
    NoDataDir,
    #[error("bundle is not signed — signature required (pass --allow-unsigned-runtime to opt out)")]
    NotSigned,
    #[error("bundle signature does not verify against embedded publisher")]
    BadSignature,
    #[error("bundle publisher {actual} does not match required {required}")]
    PublisherMismatch { required: String, actual: String },
    #[error("bundle arch {actual:?} does not match host arch {host:?}")]
    ArchMismatch { actual: String, host: String },
    // The trailing argument is named: thiserror 2 rejects a format string that
    // mixes `{0}` (the tuple field) with a bare `{}` (the extra argument),
    // because it cannot tell which one the `{}` means.
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
/// These are absolute paths on the *host*. The builder resolves each
/// against `--from` (default `/`), and any that don't exist are
/// silently skipped so a bundle can be built on a system whose loader
/// lives at only one of the alternative locations. The result is still
/// hash-stable — the manifest lists what was actually included.
///
/// Two layouts are covered per arch:
///  * Fedora / RHEL-ish: `/lib64` + `/usr/lib64`.
///  * Debian / Ubuntu-ish: `/lib/<arch>-linux-gnu`.
///
/// Only what exists on the source host lands in the bundle. Adding a
/// new architecture is a matter of adding a row here (and to
/// `CANONICAL_LOADER_PATHS` in the sandbox side).
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

/// `std::env::consts::ARCH` at build time. The default target for
/// `build_bundle` when no explicit arch is supplied.
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// x86_64 loader + libc set. Covers Fedora (`/lib64`), Debian
/// (`/lib/x86_64-linux-gnu` via the `/lib64` symlink on modern
/// distros), and the legacy `/lib/ld-linux.so.2` fallback.
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

/// aarch64 loader + libc set. Covers Debian (`/lib/aarch64-linux-gnu`)
/// and Fedora (`/lib64` on aarch64 too — same convention as x86_64).
pub const HOST_MINIMAL_PATHS_AARCH64: &[&str] = &[
    // Dynamic loader itself.
    "/lib/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
    "/usr/lib64/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    // Fedora aarch64 uses `/lib64` just like x86_64. The `/lib64`
    // dir on aarch64 Fedora is a real directory, not a symlink to
    // `/usr/lib64`, though `/usr/lib64` exists too — bundle both.
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

/// Legacy alias for [`HOST_MINIMAL_PATHS_X86_64`]. Kept only so
/// out-of-tree consumers aren't broken by the rename. New code should
/// use [`host_minimal_paths_for`] — the arch-aware entry point.
#[deprecated(note = "use host_minimal_paths_for(arch) — HOST_MINIMAL_PATHS is x86_64-only")]
pub const HOST_MINIMAL_PATHS: &[&str] = HOST_MINIMAL_PATHS_X86_64;

/// The optional `/etc/ld.so.cache` — the loader falls back to a
/// slower search without it; still worth including when present.
pub const HOST_LD_SO_CACHE: &str = "/etc/ld.so.cache";

/// Default per-user bundle directory: `$XDG_DATA_HOME/qos-runtime/<v>/`
/// (or `~/.local/share/qos-runtime/<v>/`).
pub fn default_bundle_path(version: &str) -> Result<PathBuf, BundleError> {
    let base = dirs::data_dir().ok_or(BundleError::NoDataDir)?;
    Ok(base.join("qos-runtime").join(version))
}

/// System-wide fallback: `/usr/lib/qos-runtime/<v>/`.
pub fn system_bundle_path(version: &str) -> PathBuf {
    PathBuf::from("/usr/lib/qos-runtime").join(version)
}

/// Walk the search path and return the first existing bundle directory
/// that already has a `bundle.toml`. Ordering: per-user, then system.
pub fn resolve_bundle_path(version: &str) -> Option<PathBuf> {
    let mut checked: Vec<PathBuf> = Vec::new();
    if let Ok(p) = default_bundle_path(version) {
        checked.push(p.clone());
        if p.join(BUNDLE_MANIFEST_FILENAME).exists() {
            return Some(p);
        }
    }
    let sys = system_bundle_path(version);
    checked.push(sys.clone());
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
    /// Target architecture. Defaults to [`host_arch`]. Explicit
    /// override lets tests fabricate a bundle for a non-host arch by
    /// pre-populating a fake root.
    pub arch: Option<&'a str>,
    /// Path to an identity JSON file (see [`Identity::load`]). If
    /// provided, the finished bundle is signed and the manifest gains
    /// `publisher_hex` + `signature_hex`.
    pub identity: Option<&'a Path>,
    /// Extra shared objects to place directly under `lib/` in the
    /// bundle, by file name — the Rust `libstd-<hash>.so` and the qOS
    /// `libruntime_dylib.so` that a thin app (`-C prefer-dynamic`)
    /// links dynamically. Unlike the loader/libc set (re-bound at
    /// their canonical `/lib64` paths so an ELF's `INTERP` resolves),
    /// these sit at the top of `lib/` so the launcher's
    /// `LD_LIBRARY_PATH=<mount>/lib` resolves them by soname. Each is
    /// hashed into the manifest and covered by the signature like any
    /// other bundle file. Absolute host paths; must exist.
    pub extra_libs: &'a [PathBuf],
}

/// The signing identity used by [`build_bundle_with`]. Format: JSON
/// `{ "secret_key_hex": "<64-hex-chars>" }` — matches the shape used
/// by `qos-pkg` so operators can reuse the same key material.
#[derive(Clone)]
pub struct Identity {
    signing_key: ed25519_dalek::SigningKey,
}

impl Identity {
    /// Load from a JSON file on disk.
    pub fn load(path: &Path) -> Result<Self, BundleError> {
        let bytes = fs::read(path).map_err(|e| BundleError::IdentityIo(e.to_string()))?;
        Self::from_json_bytes(&bytes)
    }

    /// Parse an identity JSON blob directly.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, BundleError> {
        #[derive(Deserialize)]
        struct File {
            secret_key_hex: String,
        }
        let f: File = serde_json::from_slice(bytes)
            .map_err(|e| BundleError::IdentityParse(e.to_string()))?;
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

    /// Generate a fresh random identity. Handy for tests; also used
    /// by the CLI's `key generate` subcommand.
    pub fn generate() -> Self {
        let mut rng = rand::rngs::OsRng;
        Self {
            signing_key: ed25519_dalek::SigningKey::generate(&mut rng),
        }
    }

    /// Serialise as the same JSON shape [`load`] reads.
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

/// Preimage the publisher signs: `blake3(DOMAIN || version || root_hash)`.
/// Domain-separated so signatures over other blobs can't collide.
pub fn signing_preimage(version: &str, root_hash_hex: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUNDLE_SIGNING_DOMAIN);
    hasher.update(version.as_bytes());
    hasher.update(root_hash_hex.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Build a bundle at `out` for the host architecture. See
/// [`build_bundle_with`] for the full-featured entry point.
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
/// - `version` names the bundle (also written into `bundle.toml`).
/// - `opts.from` overrides the host-root prefix each source path is
///   resolved against; defaults to `/`. Useful for tests and for
///   building against a distrobox rootfs from outside the container.
/// - `opts.arch` picks the source-path table. Defaults to
///   [`host_arch`]; explicit override lets a cross-arch fixture be
///   built (e.g. an aarch64 bundle from an x86_64 CI runner, as long
///   as `--from` points at an aarch64 rootfs).
/// - `opts.identity` signs the bundle. When set, the manifest gains
///   `publisher_hex` and `signature_hex`.
/// - Missing source paths are silently skipped; the bundle's hash
///   reflects what was actually copied. If *no* source resolves, we
///   return `NoSources` so a build against the wrong `--from` or an
///   arch mismatch surfaces immediately.
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

    // Copy each candidate. Track what we got so the manifest is
    // deterministic.
    let mut written: Vec<(String, PathBuf)> = Vec::new(); // relative-in-bundle → absolute-in-bundle

    for src_abs in paths {
        let src = resolve_from(host_root, src_abs);
        if !src.exists() {
            continue;
        }
        // Preserve the absolute-path shape under `lib/` — e.g.
        // `/lib64/libc.so.6` lives at `lib/lib64/libc.so.6` in the
        // bundle. This way `LD_LIBRARY_PATH=<bundle>/lib` combined
        // with the loader's own search inside the bundle-mount finds
        // everything without extra symlink dances.
        let rel_no_slash = src_abs.trim_start_matches('/');
        let rel_in_bundle = format!("lib/{}", rel_no_slash);
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(&src, &dst)?;
        written.push((rel_in_bundle, dst));
    }

    // Extra libs (libstd, libruntime_dylib.so): copied verbatim to the
    // TOP of `lib/` by file name so `LD_LIBRARY_PATH=<mount>/lib`
    // resolves them by soname. Unlike the loader/libc set these are NOT
    // resolved against `host_root` — they're explicit absolute paths
    // (build outputs), used as given.
    for extra in opts.extra_libs {
        if !extra.exists() {
            return Err(BundleError::IncludeMissing { path: extra.clone() });
        }
        let name = extra
            .file_name()
            .ok_or_else(|| BundleError::IncludeUnnamed { path: extra.clone() })?
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
            checked: paths
                .iter()
                .map(|p| resolve_from(host_root, p))
                .collect(),
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
            let identity = Identity::load(id_path)?;
            let preimage = signing_preimage(version, &root_hash);
            use ed25519_dalek::Signer;
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
        produced_at: unix_now(),
        publisher_hex,
        signature_hex,
    };

    // Persist bundle.toml.
    let toml = toml::to_string_pretty(&manifest)
        .map_err(|e| BundleError::ManifestParse(e.to_string()))?;
    fs::write(out.join(BUNDLE_MANIFEST_FILENAME), toml)?;

    Ok(manifest)
}

/// Verification policy — how strict [`verify_bundle_with`] should be.
///
/// [`verify_bundle`] uses `default()`, which:
///   * checks any signature that is present (rejects a bad sig)
///   * does NOT require a signature (an unsigned bundle passes)
///   * does NOT enforce a specific publisher
#[derive(Default, Debug, Clone)]
pub struct VerifyOptions {
    /// If set, require the bundle to be signed AND its publisher
    /// hex to equal this string.
    pub require_publisher: Option<String>,
    /// If set, refuse an unsigned bundle even without a specific
    /// required-publisher.
    pub require_signature: bool,
    /// If set, refuse a bundle whose `arch` field disagrees with
    /// this string. `None` = accept any arch.
    pub require_arch: Option<String>,
}

/// Verify an existing bundle: recompute every file's hash + the root
/// hash, then compare against `bundle.toml`. Returns the parsed
/// manifest on success so callers can print / inspect it.
///
/// Signature policy: any signature present must verify, but a bundle
/// without a signature is accepted. Use [`verify_bundle_with`] with
/// `require_signature=true` (or the launcher's default policy) to
/// refuse unsigned bundles.
pub fn verify_bundle(root: &Path) -> Result<BundleManifest, BundleError> {
    verify_bundle_with(root, &VerifyOptions::default())
}

/// Full-featured verify. See [`VerifyOptions`].
pub fn verify_bundle_with(
    root: &Path,
    opts: &VerifyOptions,
) -> Result<BundleManifest, BundleError> {
    let manifest_path = root.join(BUNDLE_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(BundleError::ManifestMissing {
            path: manifest_path,
        });
    }
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BundleManifest = toml::from_str(&text)
        .map_err(|e| BundleError::ManifestParse(e.to_string()))?;

    // Optional arch policy.
    if let Some(required) = opts.require_arch.as_deref() {
        if manifest.arch != required {
            return Err(BundleError::ArchMismatch {
                actual: manifest.arch.clone(),
                host: required.to_string(),
            });
        }
    }

    // Recompute per-file hashes; also compare against what's actually
    // on disk. Any file listed in bundle.toml but missing on disk is
    // a corruption; any file on disk that isn't listed is likewise
    // suspect (unknown-provenance content in a supposedly
    // content-addressed dir).
    let mut on_disk: BTreeMap<String, PathBuf> = BTreeMap::new();
    walk_payload(root, &mut on_disk)?;
    // The manifest itself lives at the root; don't treat it as payload.
    on_disk.remove(BUNDLE_MANIFEST_FILENAME);

    for expected in &manifest.files {
        let abs = root.join(&expected.path);
        if !abs.exists() {
            return Err(BundleError::FileMissing {
                path: expected.path.clone(),
            });
        }
        let (hash, _size) = hash_file(&abs)?;
        if hash != expected.hash {
            return Err(BundleError::HashMismatch {
                expected: expected.hash.clone(),
                computed: hash,
            });
        }
        on_disk.remove(&expected.path);
    }
    if let Some((path, _)) = on_disk.into_iter().next() {
        return Err(BundleError::FileUnlisted { path });
    }

    // Root hash. This is what a signature covers, so it's checked
    // BEFORE the signature — a tampered manifest that shifts
    // root_hash but forgets to update files (or vice versa) fails
    // here.
    let mut sorted = manifest.files.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let computed = compute_root_hash(&sorted);
    if computed != manifest.root_hash {
        return Err(BundleError::HashMismatch {
            expected: manifest.root_hash.clone(),
            computed,
        });
    }

    // Signature policy.
    let has_sig = manifest.publisher_hex.is_some() && manifest.signature_hex.is_some();
    let need_sig = opts.require_signature || opts.require_publisher.is_some();
    if !has_sig && need_sig {
        return Err(BundleError::NotSigned);
    }

    if has_sig {
        let publisher_hex = manifest.publisher_hex.as_deref().unwrap();
        let sig_hex = manifest.signature_hex.as_deref().unwrap();

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
        use ed25519_dalek::Verifier;
        if vk.verify(&preimage, &sig).is_err() {
            return Err(BundleError::BadSignature);
        }

        if let Some(required) = opts.require_publisher.as_deref() {
            if !eq_hex_ignore_case(publisher_hex, required) {
                return Err(BundleError::PublisherMismatch {
                    required: required.to_string(),
                    actual: publisher_hex.to_string(),
                });
            }
        }
    }

    Ok(manifest)
}

fn decode_fixed_hex<const N: usize>(
    s: &str,
    field: &'static str,
) -> Result<[u8; N], BundleError> {
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

/// List the immediate contents of the bundle root that constitute the
/// payload (used by `verify_bundle`). Recursive.
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
            } else if ty.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, path);
            }
            // symlinks / other: ignore. bundles should be plain files.
        }
        Ok(())
    }
    recurse(root, root, out)
}

/// BLAKE3 root: hash the newline-delimited `"<path>\t<file-hash>"`
/// concatenation, sorted by `path`. Stable + independent of file
/// mtime / permissions / directory-traversal order.
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
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), BundleError> {
    // `fs::copy` follows symlinks by default — we WANT that: the
    // point of the bundle is to capture the actual bytes, so mounting
    // it into a sandbox doesn't depend on the symlink's target being
    // reachable.
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_x86_64_host(root: &Path) {
        for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6", "lib64/libm.so.6"] {
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
    fn build_then_verify_roundtrips_x86_64() {
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

    #[test]
    fn build_supports_aarch64_fedora_layout() {
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
        assert!(paths.iter().any(|p| p.ends_with("lib/ld-linux-aarch64.so.1")));
        assert!(paths.iter().any(|p| p.ends_with("lib64/libc.so.6")));
        verify_bundle(out.path()).expect("verify");
    }

    #[test]
    fn build_supports_aarch64_debian_layout() {
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
        assert!(paths
            .iter()
            .any(|p| p.ends_with("aarch64-linux-gnu/libc.so.6")));
        verify_bundle(out.path()).expect("verify");
    }

    #[test]
    fn unsupported_arch_errors() {
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

    #[test]
    fn verify_detects_a_modified_byte() {
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

    #[test]
    fn verify_detects_unlisted_file() {
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

    #[test]
    fn build_fails_when_no_sources_resolve() {
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

    #[test]
    fn root_hash_is_deterministic() {
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

    #[test]
    fn signed_bundle_roundtrips_and_verify_accepts() {
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
        assert_eq!(manifest.publisher_hex.as_deref(), Some(identity.public_key_hex()).as_deref());
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

    #[test]
    fn verify_refuses_bad_publisher() {
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

    #[test]
    fn verify_refuses_tampered_signature() {
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

        // Tamper: rewrite root_hash to something plausible-looking. Bundle
        // integrity check catches this before the signature — good, but we
        // want to prove the signature side too. Do it in the other order:
        // flip a byte in `signature_hex` and re-file the manifest.
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

    #[test]
    fn require_signature_refuses_unsigned_bundle() {
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

    #[test]
    fn signing_preimage_is_domain_separated() {
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

    #[test]
    fn default_bundle_path_uses_data_dir() {
        if let Ok(p) = default_bundle_path("v") {
            assert!(p.ends_with("qos-runtime/v"));
        }
    }

    #[test]
    fn extra_libs_land_at_lib_root_and_are_hashed_and_verify() {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());

        // Two stand-in shared objects a thin app links dynamically:
        // the Rust std dylib and the qOS runtime dylib. Content is
        // arbitrary — the point is that they end up in the bundle,
        // content-addressed, and pass verification.
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
                extra_libs: &[libstd.clone(), libruntime.clone()],
            },
        )
        .expect("build with extras");

        // Both extras appear at the TOP of lib/ (by file name), so
        // LD_LIBRARY_PATH=<mount>/lib resolves them by soname.
        assert!(out.path().join("lib/libstd-deadbeef.so").exists());
        assert!(out.path().join("lib/libruntime_dylib.so").exists());

        // Both are recorded in the manifest (hashed + covered by the
        // root hash → covered by any signature).
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"lib/libstd-deadbeef.so"), "{paths:?}");
        assert!(paths.contains(&"lib/libruntime_dylib.so"), "{paths:?}");

        // Full verify roundtrips — proves the extras are part of the
        // content-addressed set, not loose files verify would reject.
        verify_bundle(out.path()).expect("verify with extras");
    }

    #[test]
    fn missing_extra_lib_is_an_error() {
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
