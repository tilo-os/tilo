//! qos-install — the one-command user-facing install surface.
//!
//! Layout on disk (per user, under $XDG_DATA_HOME or ~/.local/share):
//!
//! ```text
//! qos-apps/
//!   registry.toml        (one entry per installed package)
//!   payloads/<pkg-id>/   (unpacked payload, manifest.toml + payload files)
//! ```
//!
//! Trust config (per user, under $XDG_CONFIG_HOME or ~/.config/qos):
//!
//! ```text
//! trusted-attestors.toml
//! revoked-keys.toml
//! ```
//!
//! Subcommands:
//!
//! ```text
//! qos-install install  <path.qpkg> [--require-attestor <id>] [--allow-self-signed] [--pinned] [--force]
//! qos-install list
//! qos-install remove   <pkg-id>
//! qos-install run      <pkg-id> [--require-attestor <id>] [--allow-self-signed] [--no-services]
//! qos-install inspect  <pkg-id>
//! qos-install trust add    --id <id> --pubkey <hex> [--scope <spec>]
//! qos-install trust list
//! qos-install trust remove <id>
//! qos-install revoke   <pubkey_hex> [--reason <text>]
//! qos-install revoked list
//! ```
//!
//! Verifies the .qpkg signature and the trust chain before
//! extracting. Refuses to install if the publisher key is on the
//! revocation list. Warns on self-signed unless `--allow-self-signed`
//! is set; refuses if `--require-attestor` is set and no matching
//! attestation is found.

use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args, Parser, Subcommand};
use install::shim;
use pkg::blobs::BlobStore;
use pkg::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, TrustedAttestors,
    default_revoked_keys_path, default_trusted_attestors_path,
};
use pkg::{MemoryBlobStore, PackageId, Verification, unpack, verify_with_trust};
use serde::{Deserialize, Serialize};

/// Header block written into every .qpkg tar. Mirrors qos-pkg-cli's
/// QpkgHeader — kept local so this crate doesn't have a circular dep.
#[derive(Debug, Deserialize)]
struct QpkgHeader {
    /// `qpkg:<hex>` form of the PackageId.
    package_id: String,
    #[allow(dead_code)]
    format: u32,
}

#[derive(Debug, Parser)]
#[command(name = "qos-install", about = "Install, launch, list, or remove qOS packages.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Trust-related flags shared by `install` and `run`.
#[derive(Debug, Args, Clone, Default)]
struct TrustFlags {
    /// Refuse to proceed unless the .qpkg is signed by this
    /// attestor id (as found in `trusted-attestors.toml`).
    #[arg(long)]
    require_attestor: Option<String>,
    /// Permit self-signed packages that lack a trusted-attestor
    /// signature. Without this flag AND without --require-attestor,
    /// the current warn-then-proceed behaviour is preserved.
    /// Combined with --require-attestor: the attestor match is
    /// preferred; self-signed is the fallback when the attestor
    /// match fails.
    #[arg(long)]
    allow_self_signed: bool,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Install a .qpkg into the local store. Verifies signature first.
    Install {
        /// Path to the .qpkg file.
        qpkg: PathBuf,
        /// Mark the package as pinned. GC (when it lands) will never
        /// evict pinned packages.
        #[arg(long)]
        pinned: bool,
        /// Overwrite an existing installation with the same package id.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        trust: TrustFlags,
    },
    /// List installed packages.
    List,
    /// Remove an installed package by id.
    Remove {
        /// Package id (as printed by `qos-install list`).
        pkg_id: String,
    },
    /// Launch an installed package via qos-launcher.
    Run {
        /// Package id.
        pkg_id: String,
        /// Forwarded to qos-launcher.
        #[arg(long)]
        no_services: bool,
        /// Forwarded to qos-launcher. Deprecated: apps that declare
        /// `runtime = "qos-runtime/<v>"` in their manifest and have the
        /// bundle installed don't need this. Silently overridden when a
        /// runtime bundle resolves.
        #[arg(long, hide = true)]
        minimal_libs: bool,
        /// Forwarded to qos-launcher. Fall back to --minimal-libs when
        /// the manifest declares a runtime bundle that isn't installed.
        #[arg(long)]
        allow_missing_runtime: bool,
        /// Turn off the Landlock enforcement. Debug only.
        #[arg(long, hide = true)]
        no_landlock: bool,
        #[command(flatten)]
        trust: TrustFlags,
        /// Everything after `--` is forwarded verbatim to the
        /// entrypoint as its argv[1..]. Example:
        /// `qos-install run dev.qos.notes-cli -- create "hello"`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough_argv: Vec<String>,
    },
    /// Print manifest + install info for an installed package.
    Inspect {
        /// Package id.
        pkg_id: String,
    },
    /// Manage the trusted-attestor registry.
    Trust {
        #[command(subcommand)]
        cmd: TrustCmd,
    },
    /// Revoke a publisher key.
    Revoke {
        /// The publisher's Ed25519 public key, hex encoded (64 chars).
        pubkey_hex: String,
        /// Free-form reason recorded next to the entry.
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// List revoked keys.
    Revoked {
        #[command(subcommand)]
        cmd: RevokedCmd,
    },
    /// Print the directory the shim installer writes PATH shims
    /// into, plus a snippet the user can drop into their shell rc.
    /// Prepend this to PATH to make `notes`, `notes-import`, etc.
    /// resolve.
    Path,
    /// Manage installed qOS runtime bundles.
    Runtime {
        #[command(subcommand)]
        cmd: RuntimeCmd,
    },
}

#[derive(Debug, Subcommand)]
enum TrustCmd {
    /// Add or replace a trusted attestor.
    Add {
        /// Attestor id (as it appears in `[[trust.attested_by]].attestor`).
        #[arg(long)]
        id: String,
        /// The attestor's Ed25519 public key, hex encoded (64 chars).
        #[arg(long)]
        pubkey: String,
        /// Scope: `all` | `mime:<a>[,<b>...]` | `prefix:<a>[,<b>...]`.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    /// List trusted attestors.
    List,
    /// Remove a trusted attestor by id.
    Remove {
        /// Attestor id to remove.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum RevokedCmd {
    /// List revoked keys.
    List,
}

#[derive(Debug, Subcommand)]
enum RuntimeCmd {
    /// Build + install a runtime bundle into
    /// `$XDG_DATA_HOME/qos-runtime/<version>/`. `--from-host` copies
    /// from the current host root; `--from <path>` from a specific
    /// prefix (e.g. a distrobox rootfs).
    Install {
        /// Bundle version tag. Written into `bundle.toml` and used
        /// to look the bundle up at launch time.
        #[arg(long)]
        version: String,
        /// Copy from the running host root. Equivalent to `--from /`.
        #[arg(long)]
        from_host: bool,
        /// Copy from an explicit host prefix. Wins over `--from-host`.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Target architecture. Defaults to the host arch. Explicit
        /// override lets you install a cross-arch bundle from a
        /// fabricated rootfs.
        #[arg(long)]
        arch: Option<String>,
        /// Path to a signing identity JSON file (see
        /// `qos-runtime-bundle key generate`). Optional — the
        /// launcher refuses unsigned bundles by default, so most
        /// installs should sign.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Overwrite an existing bundle at the same version.
        #[arg(long)]
        force: bool,
    },
    /// List installed runtime bundle versions.
    List,
    /// Remove an installed runtime bundle by version tag.
    Remove {
        version: String,
    },
    /// Print the resolved path for a version. Same lookup order as
    /// the launcher.
    Path {
        #[arg(long)]
        version: String,
    },
    /// Verify an installed bundle's on-disk hash matches its manifest.
    Verify {
        version: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("data-dir resolution failed (XDG_DATA_HOME / HOME unset?)")]
    NoDataDir,
    #[error("config-dir resolution failed (XDG_CONFIG_HOME / HOME unset?)")]
    NoConfigDir,
    #[error("qpkg verification failed: {0}")]
    Verify(String),
    #[error("publisher key is revoked ({reason})")]
    Revoked { reason: String },
    #[error("policy refused: --require-attestor {required:?} but package verified as {found}")]
    AttestorMismatch { required: String, found: String },
    #[error("policy refused: --require-attestor {required:?} and package is self-signed (use --allow-self-signed to override)")]
    RequireAttestorWithoutAttestation { required: String },
    #[error("qpkg parse: {0}")]
    Parse(String),
    #[error("package {0} already installed (use --force to overwrite)")]
    AlreadyInstalled(String),
    #[error("no such package installed: {0}")]
    NotInstalled(String),
    #[error("registry read: {0}")]
    RegistryRead(String),
    #[error("registry write: {0}")]
    RegistryWrite(String),
    #[error("trust registry: {0}")]
    Trust(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("unpack: {0}")]
    Unpack(String),
    #[error("shim install/remove: {0}")]
    Shim(String),
    #[error("runtime-bundle: {0}")]
    RuntimeBundle(String),
    #[error("runtime bundle qos-runtime/{0} is already installed (use --force to overwrite)")]
    RuntimeBundleExists(String),
    #[error("runtime bundle qos-runtime/{0} is not installed")]
    RuntimeBundleNotInstalled(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    /// Reverse-DNS id from manifest [package].id.
    id: String,
    /// Semver from manifest.
    version: String,
    /// Content-addressed root hash (hex, no `qpkg:` prefix).
    package_id_hex: String,
    /// Absolute path to the payload directory.
    payload_root: PathBuf,
    /// Absolute path to the extracted manifest.
    manifest_path: PathBuf,
    /// Publisher public key hex.
    publisher_key_hex: String,
    /// Pinned = never GC'd (when GC lands).
    pinned: bool,
    /// Unix seconds of install.
    installed_at: u64,
    /// Attestor id if the install was verified via an attestor
    /// chain. Empty for self-signed installs.
    #[serde(default)]
    attested_by: String,
    /// PATH command names the package published shims for. Retained
    /// on the entry so `remove` can un-write them without re-reading
    /// the manifest. Empty when the package has no `[cli]` block.
    #[serde(default)]
    cli_commands: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    entry: Vec<RegistryEntry>,
}

fn data_root() -> Result<PathBuf, Error> {
    // Honour QOS_APPS_DATA_DIR when set (integration tests, dev
    // installs). Matches what qos-launcher::registry reads.
    if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_dir().ok_or(Error::NoDataDir)?;
    Ok(base.join("qos-apps"))
}

fn registry_path() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("registry.toml"))
}

fn payloads_root() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("payloads"))
}

fn trusted_attestors_path() -> Result<PathBuf, Error> {
    default_trusted_attestors_path().ok_or(Error::NoConfigDir)
}

fn revoked_keys_path() -> Result<PathBuf, Error> {
    default_revoked_keys_path().ok_or(Error::NoConfigDir)
}

fn load_trusted_attestors() -> Result<TrustedAttestors, Error> {
    let path = trusted_attestors_path()?;
    TrustedAttestors::load(&path).map_err(|e| Error::Trust(e.to_string()))
}

fn load_revocation_list() -> Result<RevocationList, Error> {
    let path = revoked_keys_path()?;
    RevocationList::load(&path).map_err(|e| Error::Trust(e.to_string()))
}

fn bin_root() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("bin"))
}

fn load_registry() -> Result<Registry, Error> {
    let p = registry_path()?;
    if !p.exists() {
        return Ok(Registry::default());
    }
    let bytes = fs::read(&p).map_err(|e| Error::RegistryRead(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(|e| Error::RegistryRead(e.to_string()))?;
    toml::from_str(&text).map_err(|e| Error::RegistryRead(e.to_string()))
}

fn save_registry(reg: &Registry) -> Result<(), Error> {
    let p = registry_path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string(reg).map_err(|e| Error::RegistryWrite(e.to_string()))?;
    // Atomic rewrite: write to sibling tmp, rename over.
    let tmp = p.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, p)?;
    Ok(())
}

/// Verify `pkg_id` against the on-disk trust config and apply
/// the caller's `TrustFlags` policy. Returns the manifest and
/// the attestor id (empty string for self-signed installs).
fn verify_and_gate(
    pkg_id: PackageId,
    store: &MemoryBlobStore,
    trust: &TrustFlags,
) -> Result<(pkg::Manifest, String, String), Error> {
    let attestors = load_trusted_attestors()?;
    let revocation = load_revocation_list()?;
    let outcome = verify_with_trust(pkg_id, store, &[], attestors.as_slice(), &revocation);
    match outcome {
        Verification::AttestedBy { attestor, manifest } => {
            if let Some(required) = &trust.require_attestor {
                if required != &attestor {
                    return Err(Error::AttestorMismatch {
                        required: required.clone(),
                        found: attestor,
                    });
                }
            }
            let pub_key_hex =
                extract_publisher_key(&pkg_id, store).map(|k| hex_of(&k)).unwrap_or_default();
            Ok((manifest, pub_key_hex, attestor))
        }
        Verification::UntrustedPublisher { identity, manifest } => {
            // --require-attestor set: refuse unless --allow-self-signed
            // rescues us.
            if let Some(required) = &trust.require_attestor {
                if !trust.allow_self_signed {
                    return Err(Error::RequireAttestorWithoutAttestation {
                        required: required.clone(),
                    });
                }
                // fallback: --allow-self-signed permits this
                eprintln!(
                    "qos-install: --require-attestor {required:?} unmatched; --allow-self-signed permits self-signed publisher ed25519:{}",
                    hex_of(&identity)
                );
            } else if !trust.allow_self_signed {
                // Default (current) behavior: warn and proceed.
                eprintln!(
                    "qos-install: WARN — package self-signed by ed25519:{} (no trusted-attestor signature); use --require-attestor <id> to demand an attestation",
                    hex_of(&identity)
                );
            }
            Ok((manifest, hex_of(&identity), String::new()))
        }
        Verification::Revoked { reason, pubkey, .. } => {
            eprintln!(
                "qos-install: REFUSED — publisher ed25519:{} is revoked: {reason}",
                hex_of(&pubkey)
            );
            Err(Error::Revoked { reason })
        }
        Verification::Trusted { manifest } => {
            let pub_key_hex =
                extract_publisher_key(&pkg_id, store).map(|k| hex_of(&k)).unwrap_or_default();
            if let Some(required) = &trust.require_attestor {
                // Trusted-key path bypasses attestor chain — the
                // user configured a trusted publisher key elsewhere.
                // --require-attestor is strict: refuse.
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: "trusted-key".into(),
                });
            }
            Ok((manifest, pub_key_hex, String::new()))
        }
        other => Err(Error::Verify(format!("{other:?}"))),
    }
}

fn install(qpkg: &Path, pinned: bool, force: bool, trust: TrustFlags) -> Result<(), Error> {
    // 1. Read + ingest the .qpkg tar into an in-memory blob store.
    let (pkg_id, store) = read_qpkg(qpkg)?;

    // 2. Verify + apply trust policy.
    let (manifest, publisher_key_hex, attested_by) = verify_and_gate(pkg_id, &store, &trust)?;

    // 3. Registry check.
    let id_field = manifest.package.id.clone();
    let version_field = manifest.package.version.clone();
    let mut reg = load_registry()?;
    if reg.entry.iter().any(|e| e.id == id_field) && !force {
        return Err(Error::AlreadyInstalled(id_field));
    }
    reg.entry.retain(|e| e.id != id_field);

    // 4. Unpack into payloads/<id_field>/. Using the reverse-DNS id
    // (not the content hash) means updates overwrite the same directory
    // — the installer treats the package name as the install slot.
    let dest = payloads_root()?.join(&id_field);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;
    let manifest = unpack(pkg_id, &store, &dest).map_err(|e| Error::Unpack(e.to_string()))?;
    // Write the manifest into the install dir explicitly so the launcher
    // can find it without re-reading the .qpkg.
    let manifest_path = dest.join("manifest.toml");
    fs::write(
        &manifest_path,
        manifest
            .to_toml_string()
            .map_err(|e| Error::Unpack(e.to_string()))?,
    )?;

    let installed_at = now();
    let package_id_hex = hex_of(pkg_id.as_bytes());

    // If the manifest has a [cli] block, drop a shim into
    // ~/.local/share/qos-apps/bin/ per declared command. The
    // launcher override picks up an env var so integration
    // tests can point the shim at a target/debug/qos-launcher.
    let cli_commands: Vec<String> = manifest
        .cli
        .as_ref()
        .map(|c| c.commands.clone())
        .unwrap_or_default();
    if !cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        let shim_source = shim::locate_shim_source();
        if !shim_source.exists() {
            eprintln!(
                "qos-install: WARNING — cannot find qos-cli-shim at {}. \
                 Skipping shim install. Set QOS_CLI_SHIM_PATH or install the qOS runtime first.",
                shim_source.display()
            );
        } else {
            let launcher_override_owned = std::env::var_os("QOS_LAUNCHER_PATH")
                .map(PathBuf::from);
            let launcher_override = launcher_override_owned.as_deref();
            let spec = shim::ShimInstall {
                package_id: &id_field,
                commands: &cli_commands,
                shim_source: &shim_source,
                bin_dir: &bin_dir,
                launcher_override,
            };
            shim::write_shims(&spec).map_err(|e| Error::Shim(e.to_string()))?;
            println!("  shims    : wrote {} shim(s) into {}", cli_commands.len(), bin_dir.display());
            println!(
                "  PATH     : prepend {} to your shell's PATH to pick them up (see `qos-install path`).",
                bin_dir.display()
            );
        }
    }

    reg.entry.push(RegistryEntry {
        id: id_field.clone(),
        version: version_field.clone(),
        package_id_hex,
        payload_root: dest.clone(),
        manifest_path,
        publisher_key_hex,
        pinned,
        installed_at,
        attested_by: attested_by.clone(),
        cli_commands,
    });
    save_registry(&reg)?;

    println!("installed: {id_field} {version_field}");
    println!("  payload  : {}", dest.display());
    println!("  pinned   : {pinned}");
    if !attested_by.is_empty() {
        println!("  attested : {attested_by}");
    }
    println!("run with:  qos-install run {id_field}");
    Ok(())
}

fn list() -> Result<(), Error> {
    let reg = load_registry()?;
    if reg.entry.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    println!(
        "{:<32}  {:<8}  {:<6}  {:<18}  {}",
        "id", "version", "pinned", "attested-by", "installed"
    );
    for e in &reg.entry {
        let ago = age(e.installed_at);
        let attested = if e.attested_by.is_empty() {
            "-"
        } else {
            &e.attested_by
        };
        println!(
            "{:<32}  {:<8}  {:<6}  {:<18}  {}",
            e.id,
            e.version,
            if e.pinned { "yes" } else { "no" },
            attested,
            ago
        );
    }
    Ok(())
}

fn remove(pkg_id: &str) -> Result<(), Error> {
    let mut reg = load_registry()?;
    let idx = reg
        .entry
        .iter()
        .position(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;
    let entry = reg.entry.remove(idx);
    if entry.payload_root.exists() {
        fs::remove_dir_all(&entry.payload_root)?;
    }
    // Undo the PATH shims. Best-effort — a missing bin_dir means
    // the shims weren't installed in the first place.
    if !entry.cli_commands.is_empty() {
        let bin_dir = bin_root()?;
        shim::remove_shims(&bin_dir, &entry.cli_commands)
            .map_err(|e| Error::Shim(e.to_string()))?;
        println!("  removed {} PATH shim(s) from {}", entry.cli_commands.len(), bin_dir.display());
    }
    save_registry(&reg)?;
    println!("removed: {pkg_id}");
    Ok(())
}

fn run(
    pkg_id: &str,
    no_services: bool,
    minimal_libs: bool,
    allow_missing_runtime: bool,
    no_landlock: bool,
    trust: TrustFlags,
    passthrough_argv: Vec<String>,
) -> Result<(), Error> {
    let reg = load_registry()?;
    let entry = reg
        .entry
        .iter()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;

    // Re-check trust policy at launch: revocation may have been
    // added after install. --require-attestor is honoured against
    // the recorded attested-by field.
    let attestors = load_trusted_attestors()?;
    let revocation = load_revocation_list()?;
    if let Ok(pk_bytes) = decode_hex_32(&entry.publisher_key_hex) {
        if let Some(rev) = revocation.find(&pk_bytes) {
            eprintln!(
                "qos-install: REFUSED — installed package's publisher is revoked: {}",
                rev.reason
            );
            return Err(Error::Revoked { reason: rev.reason.clone() });
        }
    }
    if let Some(required) = &trust.require_attestor {
        // The recorded attested_by is fixed at install time; it can
        // become stale if the registry entry was pinned before a
        // change in `trusted-attestors.toml`. Re-check: is the
        // recorded attestor still in the registry?
        let still_trusted = attestors.get(&entry.attested_by).is_some();
        if entry.attested_by != *required || !still_trusted {
            if !trust.allow_self_signed {
                return Err(Error::AttestorMismatch {
                    required: required.clone(),
                    found: if entry.attested_by.is_empty() {
                        "self-signed".into()
                    } else {
                        entry.attested_by.clone()
                    },
                });
            }
            eprintln!(
                "qos-install: --require-attestor {required:?} unmatched; --allow-self-signed permits install-time attested-by {:?}",
                entry.attested_by
            );
        }
    }

    // Find qos-launcher on PATH; fall back to a sibling of this binary.
    let launcher = which_launcher();
    let mut cmd = Command::new(&launcher);
    cmd.arg("run")
        .arg("--manifest")
        .arg(&entry.manifest_path)
        .arg("--payload-root")
        .arg(&entry.payload_root);
    if no_services {
        cmd.arg("--no-services");
    }
    if minimal_libs {
        cmd.arg("--minimal-libs");
    }
    if allow_missing_runtime {
        cmd.arg("--allow-missing-runtime");
    }
    if no_landlock {
        eprintln!("qos-install: WARNING — --no-landlock disables kernel-level FS/net restrictions. Only for debugging.");
        // Not implemented on the CLI yet — surface as a runtime flag if / when needed.
    }
    // Forward trailing argv verbatim: qos-install run <pkg> -- <argv...>
    // reaches the sandboxed process as its argv[1..]. `--` separator keeps
    // qos-launcher's clap from interpreting the tail as its own flags.
    if !passthrough_argv.is_empty() {
        cmd.arg("--");
        for a in &passthrough_argv {
            cmd.arg(a);
        }
    }
    // exec()-replace so this process's exit code IS the launcher's.
    let err = cmd.exec();
    Err(Error::Io(err))
}

fn print_path() -> Result<(), Error> {
    let bin_dir = bin_root()?;
    println!("{}", bin_dir.display());
    println!();
    println!("# Prepend the above to your shell's PATH so CLI commands");
    println!("# installed via `qos-install install <pkg.qpkg>` are picked up.");
    println!("#");
    println!("# bash / zsh (~/.bashrc, ~/.zshrc):");
    println!("export PATH=\"{}:$PATH\"", bin_dir.display());
    println!("#");
    println!("# fish (~/.config/fish/config.fish):");
    println!("fish_add_path -pg {}", bin_dir.display());
    Ok(())
}

fn inspect(pkg_id: &str) -> Result<(), Error> {
    let reg = load_registry()?;
    let entry = reg
        .entry
        .iter()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;
    println!("id            : {}", entry.id);
    println!("version       : {}", entry.version);
    println!("package-id    : qpkg:{}", entry.package_id_hex);
    println!("publisher     : ed25519:{}", entry.publisher_key_hex);
    println!("payload-root  : {}", entry.payload_root.display());
    println!("manifest      : {}", entry.manifest_path.display());
    println!("pinned        : {}", entry.pinned);
    println!(
        "attested-by   : {}",
        if entry.attested_by.is_empty() {
            "(self-signed)"
        } else {
            &entry.attested_by
        }
    );
    println!("installed     : {} seconds ago", now() - entry.installed_at);
    Ok(())
}

fn trust_add(id: &str, pubkey_hex: &str, scope: &str) -> Result<(), Error> {
    // Validate the scope string + pubkey hex up-front so we
    // reject garbage before touching the file.
    let scope_parsed =
        AttestorScope::parse(scope).map_err(|e| Error::Trust(e.to_string()))?;
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut attestors = load_trusted_attestors()?;
    attestors.attestor.retain(|a| a.id != id);
    attestors.attestor.push(TrustedAttestor {
        id: id.into(),
        pubkey_hex: pubkey_hex.into(),
        scope: scope_parsed,
    });
    let path = trusted_attestors_path()?;
    attestors.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: added {id} (scope: {scope})");
    println!("  file: {}", path.display());
    Ok(())
}

fn trust_list() -> Result<(), Error> {
    let attestors = load_trusted_attestors()?;
    if attestors.attestor.is_empty() {
        println!("(no trusted attestors)");
        return Ok(());
    }
    println!("{:<24}  {:<12}  pubkey", "id", "scope");
    for a in &attestors.attestor {
        println!("{:<24}  {:<12}  {}", a.id, a.scope.render(), a.pubkey_hex);
    }
    Ok(())
}

fn trust_remove(id: &str) -> Result<(), Error> {
    let mut attestors = load_trusted_attestors()?;
    let before = attestors.attestor.len();
    attestors.attestor.retain(|a| a.id != id);
    if attestors.attestor.len() == before {
        return Err(Error::Trust(format!("no trusted attestor with id {id:?}")));
    }
    let path = trusted_attestors_path()?;
    attestors.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    println!("trust: removed {id}");
    Ok(())
}

fn revoke(pubkey_hex: &str, reason: &str) -> Result<(), Error> {
    let _ = decode_hex_32(pubkey_hex).map_err(Error::Trust)?;
    let mut list = load_revocation_list()?;
    list.revoked.retain(|k| k.pubkey_hex != pubkey_hex);
    list.revoked.push(RevokedKey {
        pubkey_hex: pubkey_hex.into(),
        reason: reason.into(),
    });
    let path = revoked_keys_path()?;
    list.save(&path).map_err(|e| Error::Trust(e.to_string()))?;
    println!("revoked: ed25519:{pubkey_hex}");
    if !reason.is_empty() {
        println!("  reason: {reason}");
    }
    println!("  file: {}", path.display());
    Ok(())
}

fn revoked_list() -> Result<(), Error> {
    let list = load_revocation_list()?;
    if list.revoked.is_empty() {
        println!("(no revoked keys)");
        return Ok(());
    }
    println!("{:<66}  reason", "pubkey");
    for k in &list.revoked {
        println!("{:<66}  {}", k.pubkey_hex, k.reason);
    }
    Ok(())
}

fn runtime_install(
    version: &str,
    from_host: bool,
    from: Option<&Path>,
    arch: Option<&str>,
    identity: Option<&Path>,
    force: bool,
) -> Result<(), Error> {
    let dest = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if dest.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() && !force {
        return Err(Error::RuntimeBundleExists(version.to_string()));
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;
    let host_root: Option<&Path> = if let Some(p) = from {
        Some(p)
    } else if from_host {
        Some(Path::new("/"))
    } else {
        None
    };
    let manifest = runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from: host_root,
            arch,
            identity,
        },
    )
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("installed runtime bundle: qos-runtime/{version}");
    println!("  path     : {}", dest.display());
    println!("  arch     : {}", manifest.arch);
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    if let Some(pk) = &manifest.publisher_hex {
        println!("  publisher: ed25519:{pk}");
    }
    Ok(())
}

fn runtime_list() -> Result<(), Error> {
    let base = dirs::data_dir().ok_or(Error::NoDataDir)?.join("qos-runtime");
    if !base.exists() {
        println!("(no runtime bundles installed)");
        return Ok(());
    }
    let mut versions: Vec<String> = Vec::new();
    for entry in fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if entry
            .path()
            .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
            .exists()
        {
            versions.push(name);
        }
    }
    if versions.is_empty() {
        println!("(no runtime bundles installed)");
        return Ok(());
    }
    versions.sort();
    println!("{:<20}  {}", "version", "path");
    for v in &versions {
        println!("{:<20}  {}", v, base.join(v).display());
    }
    Ok(())
}

fn runtime_remove(version: &str) -> Result<(), Error> {
    let path = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if !path.exists() {
        return Err(Error::RuntimeBundleNotInstalled(version.to_string()));
    }
    fs::remove_dir_all(&path)?;
    println!("removed runtime bundle: qos-runtime/{version}");
    Ok(())
}

fn runtime_path(version: &str) -> Result<(), Error> {
    if let Some(p) = runtime_bundle::resolve_bundle_path(version) {
        println!("{}", p.display());
        return Ok(());
    }
    let per_user = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("{}", per_user.display());
    Ok(())
}

fn runtime_verify(version: &str) -> Result<(), Error> {
    let path = runtime_bundle::resolve_bundle_path(version)
        .ok_or_else(|| Error::RuntimeBundleNotInstalled(version.to_string()))?;
    let manifest =
        runtime_bundle::verify_bundle(&path).map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    println!("verified: qos-runtime/{version}");
    println!("  path     : {}", path.display());
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Install { qpkg, pinned, force, trust } => install(&qpkg, pinned, force, trust),
        Cmd::List => list(),
        Cmd::Remove { pkg_id } => remove(&pkg_id),
        Cmd::Run {
            pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            no_landlock,
            trust,
            passthrough_argv,
        } => run(
            &pkg_id,
            no_services,
            minimal_libs,
            allow_missing_runtime,
            no_landlock,
            trust,
            passthrough_argv,
        ),
        Cmd::Inspect { pkg_id } => inspect(&pkg_id),
        Cmd::Trust { cmd } => match cmd {
            TrustCmd::Add { id, pubkey, scope } => trust_add(&id, &pubkey, &scope),
            TrustCmd::List => trust_list(),
            TrustCmd::Remove { id } => trust_remove(&id),
        },
        Cmd::Revoke { pubkey_hex, reason } => revoke(&pubkey_hex, &reason),
        Cmd::Revoked { cmd } => match cmd {
            RevokedCmd::List => revoked_list(),
        },
        Cmd::Path => print_path(),
        Cmd::Runtime { cmd } => match cmd {
            RuntimeCmd::Install {
                version,
                from_host,
                from,
                arch,
                identity,
                force,
            } => runtime_install(
                &version,
                from_host,
                from.as_deref(),
                arch.as_deref(),
                identity.as_deref(),
                force,
            ),
            RuntimeCmd::List => runtime_list(),
            RuntimeCmd::Remove { version } => runtime_remove(&version),
            RuntimeCmd::Path { version } => runtime_path(&version),
            RuntimeCmd::Verify { version } => runtime_verify(&version),
        },
    };
    if let Err(e) = result {
        eprintln!("qos-install: {e}");
        std::process::exit(1);
    }
}

// ---- helpers -----------------------------------------------------------

/// Read a .qpkg tar: reconstruct the [`MemoryBlobStore`] and PackageId.
///
/// Mirrors `qos-pkg-cli::read_qpkg` (deliberately duplicated so this
/// crate stays independent of qos-pkg-cli).
fn read_qpkg(path: &Path) -> Result<(PackageId, MemoryBlobStore), Error> {
    let file = fs::File::open(path)?;
    let mut ar = tar::Archive::new(file);
    let mut header: Option<QpkgHeader> = None;
    let mut store = MemoryBlobStore::new();
    for entry in ar.entries().map_err(|e| Error::Parse(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Parse(e.to_string()))?;
        let entry_path = entry
            .path()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| Error::Parse(e.to_string()))?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf)
                    .map_err(|e| Error::Parse(format!("qpkg.toml not UTF-8: {e}")))?;
                header = Some(
                    toml::from_str(&s).map_err(|e| Error::Parse(format!("qpkg.toml: {e}")))?,
                );
            }
            n if n.starts_with("blobs/") => {
                store
                    .put(&buf)
                    .map_err(|e| Error::Parse(format!("blob store: {e}")))?;
            }
            // signed-root / manifest.toml / collection.qcl are also
            // duplicated under blobs/<hex>, but ingest defensively in
            // case a packager omitted the duplicate.
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                store
                    .put(&buf)
                    .map_err(|e| Error::Parse(format!("blob store: {e}")))?;
            }
            _ => {}
        }
    }
    let header = header.ok_or_else(|| Error::Parse(".qpkg is missing qpkg.toml".into()))?;
    let id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| Error::Parse(format!("qpkg.toml package_id: {e:?}")))?;
    Ok((id, store))
}

/// Best-effort publisher key extraction. Reads the signed-root blob
/// out of the store by the PackageId (which IS the signed-root's hash)
/// and decodes just enough to lift the 32-byte publisher_identity.
fn extract_publisher_key(id: &PackageId, store: &MemoryBlobStore) -> Option<[u8; 32]> {
    let blob = store
        .get(&pkg::blobs::BlobHash(*id.as_bytes()))
        .ok()
        .flatten()?;
    let signed = pkg::signed::SignedRoot::decode(&blob).ok()?;
    Some(signed.publisher_identity)
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn age(installed_at: u64) -> String {
    let n = now();
    let elapsed = n.saturating_sub(installed_at);
    match elapsed {
        0..=59 => format!("{elapsed}s ago"),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86400),
    }
}

fn which_launcher() -> PathBuf {
    // Explicit override wins — same env var the shim installer honours.
    // Lets integration tests point at a fake shim without disturbing
    // the shared target/ directory.
    if let Some(p) = std::env::var_os("QOS_LAUNCHER_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    // Prefer a sibling binary — matches how the demo installs both under
    // the same target/release/ dir.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sib = parent.join("qos-launcher");
            if sib.exists() {
                return sib;
            }
        }
    }
    PathBuf::from("qos-launcher")
}
