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
//! Subcommands:
//!
//! ```text
//! qos-install install  <path.qpkg> [--pinned]
//! qos-install list
//! qos-install remove   <pkg-id>
//! qos-install run      <pkg-id> [--no-services]     # forwards to qos-launcher
//! qos-install inspect  <pkg-id>
//! ```
//!
//! Verifies the .qpkg signature before extracting. Refuses to overwrite
//! an existing installation unless `--force`. Every mutation persists
//! the registry immediately so a crash doesn't leave the system in an
//! inconsistent state.

use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};
use qos_pkg::blobs::BlobStore;
use qos_pkg::{MemoryBlobStore, PackageId, Verification, unpack, verify};
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
        /// Forwarded to qos-launcher. Stopgap while apps don't yet
        /// carry a runtime bundle reference.
        #[arg(long)]
        minimal_libs: bool,
        /// Turn off the Landlock enforcement. Debug only.
        #[arg(long, hide = true)]
        no_landlock: bool,
    },
    /// Print manifest + install info for an installed package.
    Inspect {
        /// Package id.
        pkg_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("data-dir resolution failed (XDG_DATA_HOME / HOME unset?)")]
    NoDataDir,
    #[error("qpkg verification failed: {0}")]
    Verify(String),
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
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("unpack: {0}")]
    Unpack(String),
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
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    entry: Vec<RegistryEntry>,
}

fn data_root() -> Result<PathBuf, Error> {
    let base = dirs::data_dir().ok_or(Error::NoDataDir)?;
    Ok(base.join("qos-apps"))
}

fn registry_path() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("registry.toml"))
}

fn payloads_root() -> Result<PathBuf, Error> {
    Ok(data_root()?.join("payloads"))
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

fn install(qpkg: &Path, pinned: bool, force: bool) -> Result<(), Error> {
    // 1. Read + ingest the .qpkg tar into an in-memory blob store.
    let (pkg_id, store) = read_qpkg(qpkg)?;

    // 2. Verify signature. Self-signed acceptable for v0 (see CRITIQUE.md
    // security-#3: publisher trust chain is a follow-up).
    let (manifest, publisher_key_hex) = match verify(pkg_id, &store, &[]) {
        Verification::Trusted { manifest } => {
            // We asked for no trusted keys, so this branch is unreachable
            // unless the caller wired a keyring in. Included for
            // future-compat.
            let pk = hex_of(&extract_publisher_key(&pkg_id, &store).unwrap_or([0u8; 32]));
            (manifest, pk)
        }
        Verification::UntrustedPublisher { identity, manifest } => {
            eprintln!(
                "qos-install: package self-signed by ed25519:{} — installing (no trusted-attestor list yet; see CRITIQUE.md security-#3)",
                hex_of(&identity)
            );
            (manifest, hex_of(&identity))
        }
        other => {
            return Err(Error::Verify(format!("{other:?}")));
        }
    };

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

    reg.entry.push(RegistryEntry {
        id: id_field.clone(),
        version: version_field.clone(),
        package_id_hex,
        payload_root: dest.clone(),
        manifest_path,
        publisher_key_hex,
        pinned,
        installed_at,
    });
    save_registry(&reg)?;

    println!("installed: {id_field} {version_field}");
    println!("  payload  : {}", dest.display());
    println!("  pinned   : {pinned}");
    println!("run with:  qos-install run {id_field}");
    Ok(())
}

fn list() -> Result<(), Error> {
    let reg = load_registry()?;
    if reg.entry.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    println!("{:<32}  {:<8}  {:<6}  {}", "id", "version", "pinned", "installed");
    for e in &reg.entry {
        let ago = age(e.installed_at);
        println!(
            "{:<32}  {:<8}  {:<6}  {}",
            e.id, e.version, if e.pinned { "yes" } else { "no" }, ago
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
    save_registry(&reg)?;
    println!("removed: {pkg_id}");
    Ok(())
}

fn run(pkg_id: &str, no_services: bool, minimal_libs: bool, no_landlock: bool) -> Result<(), Error> {
    let reg = load_registry()?;
    let entry = reg
        .entry
        .iter()
        .find(|e| e.id == pkg_id)
        .ok_or_else(|| Error::NotInstalled(pkg_id.into()))?;

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
    if no_landlock {
        eprintln!("qos-install: WARNING — --no-landlock disables kernel-level FS/net restrictions. Only for debugging.");
        // Not implemented on the CLI yet — surface as a runtime flag if / when needed.
    }
    // exec()-replace so this process's exit code IS the launcher's.
    let err = cmd.exec();
    Err(Error::Io(err))
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
    println!("installed     : {} seconds ago", now() - entry.installed_at);
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Install { qpkg, pinned, force } => install(&qpkg, pinned, force),
        Cmd::List => list(),
        Cmd::Remove { pkg_id } => remove(&pkg_id),
        Cmd::Run { pkg_id, no_services, minimal_libs, no_landlock } => {
            run(&pkg_id, no_services, minimal_libs, no_landlock)
        }
        Cmd::Inspect { pkg_id } => inspect(&pkg_id),
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
        .get(&qos_pkg::blobs::BlobHash(*id.as_bytes()))
        .ok()
        .flatten()?;
    let signed = qos_pkg::signed::SignedRoot::decode(&blob).ok()?;
    Some(signed.publisher_identity)
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
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
