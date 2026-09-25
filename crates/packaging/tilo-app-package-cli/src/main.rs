//! `tilo-app-package` — CLI driver for the `tilo-app-package` foundation crate.
//!
//! A `.tapp` is a plain (uncompressed) tar archive:
//!
//! ```text
//! tapp.toml          # tiny header: package_id (tapp:<hex>) + format version
//! signed-root        # the 173-byte signed-root blob (binary)
//! manifest.toml      # the manifest, UTF-8 TOML
//! collection.qcl     # the encoded payload Collection
//! blobs/<hex>        # each payload blob, named by its BLAKE3 hex hash
//! ```
//!
//! Uncompressed because each blob is already content-addressed and,
//! deployed, lives in an iroh-blobs store doing chunk-level
//! dedup/streaming — gzip would only waste CPU. `tapp.toml` is
//! convention-only (duplicates what's in signed-root) so `tar tf` shows
//! the `PackageId` without decoding binary.

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
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use tilo_app_package::blobs::{BlobHash, BlobStore, Collection};
use tilo_app_package::signed::SignedRoot;
use tilo_app_package::{
    Manifest, MemoryBlobStore, PackageId, SigningIdentity, Verification, pack, unpack, verify,
};

/// The header entry inside a .tapp tar. Tiny and minimal so it can be
/// deserialised without pulling in the rest of the crate.
#[derive(Debug, Serialize, Deserialize)]
struct TappHeader {
    /// `tapp:<hex>` form.
    package_id: String,
    /// Format version of the .tapp layout itself; bumped independently from tilo-app-package's signed-root version.
    format: u32,
}

const TAPP_FORMAT: u32 = 1;

#[derive(Parser)]
#[command(
    name = "tilo-app-package",
    about = "Pack / sign / verify / inspect Tilo packages (.tapp)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RegistryAction {
    Add,
    Update,
    Delete,
}

/// Publisher-authored, localizable store presentation. JSON uses the same
/// signed wire types as the registry entry, so no lossy conversion or locale
/// normalization happens between authoring and signing.
#[derive(Clone, Debug, Deserialize)]
struct EntryMetadata {
    name: registry_types::LocalizedText,
    description: registry_types::LocalizedText,
    tagline: registry_types::LocalizedText,
    #[serde(default)]
    images: registry_types::LocalizedImages,
    supported_languages: Vec<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
}

impl From<RegistryAction> for registry_types::EntryAction {
    fn from(value: RegistryAction) -> Self {
        match value {
            RegistryAction::Add => Self::Add,
            RegistryAction::Update => Self::Update,
            RegistryAction::Delete => Self::Delete,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Pack a payload directory + manifest into a .tapp, signing with a fresh identity unless --identity is passed.
    Pack {
        /// Path to the manifest (TOML).
        #[arg(long)]
        manifest: PathBuf,
        /// Path to the payload directory.
        #[arg(long)]
        payload: PathBuf,
        /// Output .tapp path.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Optional identity JSON; if absent, a fresh keypair is generated and
        /// its public key replaces `publisher.identity` in the embedded manifest copy.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Re-sign an existing .tapp with the given identity, writing the result back into the same file.
    Sign {
        #[arg(long)]
        identity: PathBuf,
        /// Path to the .tapp.
        file: PathBuf,
    },
    /// Verify a .tapp — checks every cryptographic invariant and reports
    /// the first one that breaks. By default the embedded publisher
    /// identity is trusted (so a self-signed package succeeds); pass
    /// --trusted-key to override.
    Verify {
        /// Hex-encoded trusted Ed25519 public key (32 bytes), repeatable; if
        /// empty the embedded publisher key is auto-trusted.
        #[arg(long)]
        trusted_key: Vec<String>,
        file: PathBuf,
    },
    /// Print the manifest + payload tree of a .tapp.
    Inspect { file: PathBuf },
    /// Extract the payload + a copy of the manifest into `dest`.
    Unpack {
        #[arg(long)]
        dest: PathBuf,
        file: PathBuf,
    },
    /// Publish a signed .tapp to a registry: sign an index entry, upload
    /// the blob to iroh-blobs, and emit the entry to the target registry.
    ///
    /// `--registry` is `file://`/local (self-hosted) or `https://`.
    /// `--dry-run` prints the signed entry + content hash without uploading.
    Publish {
        /// Target registry: a local dir / `file://…` (self-hosted) or an `https://…` endpoint.
        #[arg(long)]
        registry: String,
        /// Publisher identity JSON; required unless --dry-run, and must be the key that signed the package.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Registry operator identity signing the served index (file:// only);
        /// defaults to --identity for single-operator hosting.
        #[arg(long = "registry-key")]
        registry_key: Option<PathBuf>,
        /// `registry_id` stamped into a freshly-created index.
        #[arg(long = "registry-id", default_value = "local")]
        registry_id: String,
        /// Target arch when the manifest declares more than one (`x86_64` | aarch64); inferred otherwise.
        #[arg(long)]
        arch: Option<String>,
        /// Build + sign the entry and print it (with content hash); upload nothing.
        #[arg(long)]
        dry_run: bool,
        /// After publishing to a file:// registry, keep serving the uploaded
        /// blobs so index tickets resolve (blocks until Ctrl-C).
        #[arg(long)]
        serve: bool,
        /// The .tapp to publish.
        file: PathBuf,
    },
    /// Generate a publisher identity, written as the JSON --identity accepts elsewhere; refuses to overwrite.
    Keygen {
        /// Where to write the identity JSON.
        #[arg(long)]
        out: PathBuf,
    },
    /// Print a signed registry entry for a .tapp — the JSON shape a
    /// registry's `/entries` serves. For a static-directory self-hosted
    /// registry, collect printed entries into an `entries` file and serve
    /// the directory over HTTP.
    Entry {
        /// Public URL the .tapp will be downloadable from.
        #[arg(long)]
        url: String,
        /// Publisher identity JSON that signed the tapp and signs this entry.
        #[arg(long)]
        identity: PathBuf,
        /// Target architecture (`x86_64` or `aarch64`); inferred when the
        /// package manifest declares exactly one architecture.
        #[arg(long)]
        arch: Option<String>,
        /// JSON presentation metadata: localized name, product description,
        /// tagline and images plus the app's supported language tags.
        #[arg(long)]
        metadata: Option<PathBuf>,
        /// Wrap the entry in the signed mutation shape accepted by POST /entries.
        #[arg(long)]
        action: Option<RegistryAction>,
        /// Current signed registry entry JSON. Required for updates so the
        /// emitted request preserves immutable version history. A retry whose
        /// artifact is already present reproduces the current entry exactly.
        #[arg(long, requires = "action")]
        previous_entry: Option<PathBuf>,
        /// The .tapp to describe.
        file: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Pack {
            manifest,
            payload,
            output,
            identity,
        } => cmd_pack(&manifest, &payload, &output, identity.as_deref()),
        Command::Sign { identity, file } => cmd_sign(&identity, &file),
        Command::Verify { trusted_key, file } => cmd_verify(&trusted_key, &file),
        Command::Inspect { file } => cmd_inspect(&file),
        Command::Unpack { dest, file } => cmd_unpack(&dest, &file),
        Command::Publish {
            registry,
            identity,
            registry_key,
            registry_id,
            arch,
            dry_run,
            serve,
            file,
        } => cmd_publish(
            &file,
            &registry,
            identity.as_deref(),
            registry_key.as_deref(),
            &registry_id,
            arch.as_deref(),
            dry_run,
            serve,
        ),
        Command::Keygen { out } => cmd_keygen(&out),
        Command::Entry {
            url,
            identity,
            arch,
            metadata,
            action,
            previous_entry,
            file,
        } => cmd_entry(
            &file,
            &url,
            &identity,
            arch.as_deref(),
            metadata.as_deref(),
            action,
            previous_entry.as_deref(),
        ),
    }
}

fn cmd_pack(
    manifest_path: &Path,
    payload_dir: &Path,
    output: &Path,
    identity_path: Option<&Path>,
) -> Result<()> {
    let manifest_src = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let mut manifest = Manifest::from_toml_str(&manifest_src).context("decode manifest TOML")?;

    let identity = match identity_path {
        Some(p) => load_identity(p)?,
        None => SigningIdentity::generate(),
    };

    // If the manifest's publisher.identity was a placeholder (zeros or a
    // different hex), patch it to the signing identity's public key —
    // pack-fresh keys should produce a self-consistent manifest without
    // round-tripping through sign first.
    let pub_hex = hex::encode(identity.public_key_bytes());
    manifest.publisher.identity = format!("ed25519:{pub_hex}");

    let mut store = MemoryBlobStore::new();
    let id = pack(payload_dir, &manifest, &identity, &mut store)
        .map_err(|e| anyhow!("pack failed: {e}"))?;
    write_tapp(output, id, &store)?;
    eprintln!("packed {} ({} blobs)", id, store.len());
    Ok(())
}

fn cmd_sign(identity_path: &Path, file: &Path) -> Result<()> {
    let identity = load_identity(identity_path)?;
    let (id, store) = read_tapp(file)?;
    // Re-construct the signed-root with a new signature.
    let signed_bytes = store
        .get(&BlobHash(*id.as_bytes()))
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    let signed = SignedRoot::decode(&signed_bytes)?;
    if signed.publisher_identity != identity.public_key_bytes() {
        bail!(
            "identity does not match the package's publisher identity (pkg={}, ours={})",
            hex::encode(signed.publisher_identity),
            hex::encode(identity.public_key_bytes())
        );
    }
    // Sign with the has_signature bit already set so verify recomputes the same bytes.
    let mut pre_signed = signed;
    pre_signed.signature = Some([0u8; 64]);
    let sig = identity
        .sign_signed_root(&pre_signed)
        .map_err(|e| anyhow!("sign: {e}"))?;
    let new_signed = SignedRoot {
        signature: Some(sig),
        ..pre_signed
    };
    let mut new_store = MemoryBlobStore::new();
    for (h, b) in store.iter() {
        if h == &BlobHash(*id.as_bytes()) {
            continue;
        }
        new_store.put(b).map_err(|e| anyhow!("store: {e}"))?;
    }
    new_store
        .put(&new_signed.encode())
        .map_err(|e| anyhow!("store: {e}"))?;
    let new_id = new_signed.package_id();
    write_tapp(file, new_id, &new_store)?;
    eprintln!("re-signed {id} -> {new_id}");
    Ok(())
}

#[allow(
    clippy::exit,
    reason = "the verify subcommand's contract is a 3-way exit code (0 trusted, 1 failed, 2 \
              signature-ok-but-untrusted), which anyhow's Result<()> exit mapping in main can't \
              express; this is the CLI's terminal command handler, nothing here holds a resource \
              that needs Drop-based cleanup"
)]
fn cmd_verify(trusted_hex: &[String], file: &Path) -> Result<()> {
    let (id, store) = read_tapp(file)?;
    // Trust set: caller's keys + (if empty) the embedded publisher key for convenience.
    let mut keys: Vec<[u8; 32]> = Vec::new();
    for hex in trusted_hex {
        let b = hex::decode(hex).with_context(|| format!("invalid trusted-key hex {hex:?}"))?;
        let arr: [u8; 32] = b
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("trusted-key must be 32 bytes"))?;
        keys.push(arr);
    }
    if keys.is_empty() {
        // Auto-trust the embedded publisher.
        let signed_bytes = store
            .get(&BlobHash(*id.as_bytes()))
            .map_err(|e| anyhow!("store: {e}"))?
            .ok_or_else(|| anyhow!("signed-root blob missing"))?;
        let signed = SignedRoot::decode(&signed_bytes)?;
        keys.push(signed.publisher_identity);
    }
    let outcome = verify(id, &store, &keys);
    match outcome {
        Verification::Trusted { .. } => {
            println!("ok: {id} verifies and publisher is trusted");
            Ok(())
        }
        Verification::UntrustedPublisher { identity, .. } => {
            println!(
                "signature-ok-but-untrusted: {id} (publisher ed25519:{})",
                hex::encode(identity)
            );
            std::process::exit(2);
        }
        other => {
            println!("FAILED: {other:?}");
            std::process::exit(1);
        }
    }
}

fn cmd_inspect(file: &Path) -> Result<()> {
    let archive_size = std::fs::metadata(file)
        .with_context(|| format!("stat {}", file.display()))?
        .len();
    let (id, store) = read_tapp(file)?;
    println!("package-id: {id}");
    println!("archive-size: {archive_size} bytes");
    let signed_bytes = store
        .get(&BlobHash(*id.as_bytes()))
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    let signed = SignedRoot::decode(&signed_bytes)?;
    println!("manifest-hash: blake3:{}", signed.manifest_hash);
    println!("payload-root-hash: blake3:{}", signed.payload_root_hash);
    println!(
        "publisher-identity: ed25519:{}",
        hex::encode(signed.publisher_identity)
    );
    println!(
        "signed: {}",
        if signed.signature.is_some() {
            "yes"
        } else {
            "NO"
        }
    );
    let manifest_bytes = store
        .get(&signed.manifest_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("manifest blob missing"))?;
    let manifest_str = std::str::from_utf8(&manifest_bytes).context("manifest is not UTF-8")?;
    println!("\n--- manifest.toml ---");
    println!("{manifest_str}");
    let collection_bytes = store
        .get(&signed.payload_root_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("collection blob missing"))?;
    let collection = Collection::decode(&collection_bytes)?;
    let payload_size: u64 = collection
        .entries
        .values()
        .filter_map(|hash| store.get(hash).ok().flatten())
        .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        .sum();
    println!("payload-size: {payload_size} bytes");
    println!(
        "package-overhead: {} bytes",
        archive_size.saturating_sub(payload_size)
    );
    println!("--- payload ({} files) ---", collection.entries.len());
    for (p, h) in &collection.entries {
        let size = store.get(h).ok().flatten().map_or(0, |b| b.len());
        println!("  {p}  (blake3:{h}, {size} bytes)");
    }
    Ok(())
}

fn cmd_unpack(dest: &Path, file: &Path) -> Result<()> {
    let (id, store) = read_tapp(file)?;
    let manifest = unpack(id, &store, dest).map_err(|e| anyhow!("unpack: {e}"))?;
    // Also drop a manifest.toml at the destination root for convenience —
    // unpack only writes the payload tree, but a developer inspecting it
    // expects to see the manifest too.
    let toml = manifest
        .to_toml_string()
        .map_err(|e| anyhow!("re-encode manifest: {e}"))?;
    std::fs::write(dest.join("manifest.toml"), toml).context("write manifest.toml")?;
    eprintln!("unpacked {id} -> {}", dest.display());
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the publish subcommand's explicit CLI options are kept visible at the command boundary"
)]
fn cmd_publish(
    file: &Path,
    registry: &str,
    identity: Option<&Path>,
    registry_key: Option<&Path>,
    registry_id: &str,
    arch: Option<&str>,
    dry_run: bool,
    serve: bool,
) -> Result<()> {
    let tapp_bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let arch = match arch {
        None => None,
        Some("x86_64") => Some(tilo_app_package::manifest::Arch::X86_64),
        Some("aarch64") => Some(tilo_app_package::manifest::Arch::AArch64),
        Some(other) => bail!("unknown --arch {other:?} (expected x86_64 or aarch64)"),
    };

    // --dry-run: emit the signed entry + content hash, no network; a
    // fresh key still yields an inspectable signature, but a real publish
    // must pin --identity.
    if dry_run {
        let publisher = match identity {
            Some(p) => load_identity(p)?,
            None => SigningIdentity::generate(),
        };
        let (entry, package_id) = app_registry::build_entry(&tapp_bytes, &publisher, arch, None)
            .map_err(|e| anyhow!("build entry: {e}"))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&entry).context("encode entry")?
        );
        eprintln!("content-hash: blake3:{}", entry.tapp_hash);
        eprintln!("package-id:   {package_id}");
        return Ok(());
    }

    let identity_path = identity.ok_or_else(|| {
        anyhow!("--identity is required to publish (only --dry-run may run keyless)")
    })?;
    let publisher = load_identity(identity_path)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    // HTTPS target: emit the signed entry to the registry endpoint, which
    // ingests the blob out of band via the entry's ticket.
    if registry.starts_with("http://") || registry.starts_with("https://") {
        let (entry, _package_id) = app_registry::build_entry(&tapp_bytes, &publisher, arch, None)
            .map_err(|e| anyhow!("build entry: {e}"))?;
        rt.block_on(app_registry::publish::emit_entry_https(registry, &entry))
            .map_err(|e| anyhow!("emit to registry: {e}"))?;
        eprintln!(
            "emitted signed entry for {}@{} to {registry}",
            entry.app_id, entry.version
        );
        return Ok(());
    }

    // file:// / local directory registry (self-hosted reference).
    let dir = PathBuf::from(registry.strip_prefix("file://").unwrap_or(registry));
    let reg_key = match registry_key {
        Some(p) => load_identity(p)?,
        None => load_identity(identity_path)?,
    };

    rt.block_on(async move {
        let node =
            app_registry::BlobNode::persistent(app_registry::registry_dir::blobs_dir(&dir)).await?;
        let receipt = app_registry::publish_to_dir(
            &dir,
            &node,
            &tapp_bytes,
            &publisher,
            &reg_key,
            registry_id,
            arch,
        )
        .await?;
        eprintln!(
            "published {}@{} ({}) to {}",
            receipt.app_id,
            receipt.version,
            receipt.package_id,
            dir.display()
        );
        eprintln!("content-hash: blake3:{}", receipt.tapp_hash);
        if serve {
            eprintln!("serving blobs — Ctrl-C to stop");
            tokio::signal::ctrl_c().await.context("await Ctrl-C")?;
        } else {
            eprintln!(
                "note: blob uploaded to {}/blobs; re-run with --serve (or run a persistent \
                 registry node) so the index ticket resolves for clients",
                dir.display()
            );
        }
        anyhow::Ok(())
    })?;
    Ok(())
}

// .tapp I/O

struct PendingTapp(PathBuf);

impl Drop for PendingTapp {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

fn write_tapp(path: &Path, id: PackageId, store: &MemoryBlobStore) -> Result<()> {
    let temp = path.with_extension(format!("tapp.tmp.{}", std::process::id()));
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("create temporary package {}", temp.display()))?;
    let mut pending = PendingTapp(temp.clone());
    let mut tar = tar::Builder::new(file);

    let header = TappHeader {
        package_id: id.to_string(),
        format: TAPP_FORMAT,
    };
    let header_toml = toml_to_string(&header)?;
    tar_append_str(&mut tar, "tapp.toml", &header_toml)?;

    // signed-root
    let signed = store
        .get(&BlobHash(*id.as_bytes()))
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    tar_append_bytes(&mut tar, "signed-root", &signed)?;

    // manifest + collection named for the human reader — the verifier
    // actually walks via the hashes in signed-root, not these names.
    let signed_decoded = SignedRoot::decode(&signed)?;
    let manifest = store
        .get(&signed_decoded.manifest_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("manifest blob missing"))?;
    let package_kind = std::str::from_utf8(&manifest)
        .context("manifest is not UTF-8")
        .and_then(|source| Manifest::from_toml_str(source).context("decode package manifest"))?
        .package
        .kind;
    tar_append_bytes(&mut tar, "manifest.toml", &manifest)?;
    let collection = store
        .get(&signed_decoded.payload_root_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("collection blob missing"))?;
    tar_append_bytes(&mut tar, "collection.qcl", &collection)?;

    // Per-file blobs.
    for (h, b) in store.iter() {
        // Skip the three blobs already written under human-readable names;
        // readers ingest those names into the same content-addressed store.
        if *h == BlobHash(*id.as_bytes())
            || *h == signed_decoded.manifest_hash
            || *h == signed_decoded.payload_root_hash
        {
            continue;
        }
        let name = format!("blobs/{h}");
        tar_append_bytes(&mut tar, &name, b)?;
    }
    if let Err(error) = tar.finish().context("finalise tar") {
        drop(tar);
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    drop(tar);
    let archive_size = std::fs::metadata(&temp)
        .with_context(|| format!("stat temporary package {}", temp.display()))?
        .len();
    if !tilo_app_package::tapp_size_allowed(package_kind, archive_size) {
        let _ = std::fs::remove_file(&temp);
        bail!(
            "application tapp is {archive_size} bytes; limit is {} bytes",
            tilo_app_package::MAX_APP_TAPP_BYTES
        );
    }
    if let Err(error) =
        std::fs::rename(&temp, path).with_context(|| format!("install package {}", path.display()))
    {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    pending.0.clear();
    Ok(())
}

fn read_tapp(path: &Path) -> Result<(PackageId, MemoryBlobStore)> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut ar = tar::Archive::new(file);
    let mut header: Option<TappHeader> = None;
    let mut store = MemoryBlobStore::new();
    for entry in ar.entries().context("read tar entries")? {
        let mut entry = entry?;
        let entry_path = entry.path().map(|p| p.to_path_buf()).unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read tar entry")?;
        match name.as_str() {
            "tapp.toml" => {
                let s = String::from_utf8(buf).context("tapp.toml not UTF-8")?;
                header = Some(toml_from_str(&s)?);
            }
            n if n.starts_with("blobs/") => {
                store.put(&buf).map_err(|e| anyhow!("store: {e}"))?;
            }
            // signed-root/manifest.toml/collection.qcl are usually duplicated
            // under blobs/<hex>; ingest them here too in case a packager omitted the duplicate.
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                store.put(&buf).map_err(|e| anyhow!("store: {e}"))?;
            }
            _ => { /* unknown entry — ignore */ }
        }
    }
    let header = header.ok_or_else(|| anyhow!(".tapp is missing tapp.toml"))?;
    let id: PackageId = header
        .package_id
        .parse()
        .map_err(|e| anyhow!("bad package id {:?}: {e}", header.package_id))?;
    Ok((id, store))
}

fn tar_append_bytes<W: Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .with_context(|| format!("write tar entry {name}"))?;
    Ok(())
}

fn tar_append_str<W: Write>(tar: &mut tar::Builder<W>, name: &str, s: &str) -> Result<()> {
    tar_append_bytes(tar, name, s.as_bytes())
}

fn toml_to_string<T: serde::Serialize>(v: &T) -> Result<String> {
    toml::to_string_pretty(v).context("toml encode tapp.toml")
}

fn toml_from_str(s: &str) -> Result<TappHeader> {
    toml::from_str(s).context("toml decode tapp.toml")
}

// identity I/O

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    /// 32-byte secret-key seed, hex-encoded.
    secret_key_hex: String,
}

fn load_identity(path: &Path) -> Result<SigningIdentity> {
    let bytes = std::fs::read(path).with_context(|| format!("read identity {}", path.display()))?;
    let f: IdentityFile = serde_json::from_slice(&bytes).context("parse identity JSON")?;
    let raw = hex::decode(&f.secret_key_hex).context("identity secret-key hex")?;
    let raw_arr: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("identity secret-key must be 32 bytes"))?;
    let sk = SigningKey::from_bytes(&raw_arr);
    Ok(SigningIdentity::from_signing_key(sk))
}

/// The raw signing key behind an identity file — for signatures that
/// aren't package signatures (registry entries).
fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let bytes = std::fs::read(path).with_context(|| format!("read identity {}", path.display()))?;
    let f: IdentityFile = serde_json::from_slice(&bytes).context("parse identity JSON")?;
    let raw = hex::decode(&f.secret_key_hex).context("identity secret-key hex")?;
    let raw_arr: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("identity secret-key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&raw_arr))
}

fn cmd_keygen(out: &Path) -> Result<()> {
    if out.exists() {
        bail!(
            "{} exists; refusing to overwrite a signing identity",
            out.display()
        );
    }
    let sk = SigningKey::generate(&mut UnwrapErr(SysRng));
    let file = IdentityFile {
        secret_key_hex: hex::encode(sk.to_bytes()),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    std::fs::write(
        out,
        serde_json::to_vec_pretty(&file).context("encode identity")?,
    )
    .with_context(|| format!("write {}", out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600));
    }
    eprintln!("wrote {}", out.display());
    println!("{}", hex::encode(sk.verifying_key().to_bytes()));
    Ok(())
}

/// Build the entry from the package's own manifest: id, name,
/// category, version come from inside the signed package, so a
/// registry row cannot describe a package as something it is not.
#[allow(
    clippy::too_many_lines,
    reason = "registry entry assembly must bind package inspection, target selection, metadata, prior-entry continuity, validation, and signing in one auditable sequence"
)]
fn registry_entry_for(
    tapp_bytes: &[u8],
    url: &str,
    signer: &SigningKey,
    arch: Option<tilo_app_package::manifest::Arch>,
    previous: Option<&registry_types::RegistryEntry>,
    metadata: Option<&EntryMetadata>,
) -> Result<registry_types::RegistryEntry> {
    let publisher = SigningIdentity::from_signing_key(signer.clone());
    // Use the same trust-boundary path as `publish`: this verifies every
    // signed blob, materialized payload limits and payload shape before any
    // registry metadata is signed.
    let (built, package_id) = app_registry::build_entry(tapp_bytes, &publisher, arch, None)
        .map_err(|error| anyhow!("verify tapp for registry entry: {error}"))?;
    let meta = app_registry::tapp::load(tapp_bytes).map_err(|e| anyhow!("load tapp: {e}"))?;
    let package = &meta.manifest.package;
    let now = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs())
        });
    let version = registry_types::RegistryVersion {
        version: package.version.clone(),
        tapp_url: url.to_string(),
        tapp_root_hash_hex: package_id.to_hex(),
        tapp_size_bytes: tapp_bytes.len() as u64,
        released_at_unix: now,
        tapp_blake3_hex: built.tapp_hash.clone(),
        target: registry_types::RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: app_registry::index::arch_wire(built.arch).to_string(),
            isa: built.isa,
            runtime: built.runtime,
        },
    };
    let mut artifact_exists = false;
    if let Some(previous) = previous {
        if previous.package_id != package.id
            || !previous
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&hex::encode(signer.verifying_key().to_bytes()))
        {
            bail!("--previous-entry does not belong to this package and publisher");
        }
        if let Some(existing) = previous
            .versions
            .iter()
            .find(|existing| same_version_identity(existing, &version))
        {
            if !same_artifact(existing, &version) {
                bail!(
                    "version {} for target {} already exists with different package bytes or metadata",
                    version.version,
                    app_registry::index::arch_wire(built.arch),
                );
            }
            if metadata.is_none() {
                return Ok(previous.clone());
            }
            artifact_exists = true;
        }
        if candidate_is_older_than_published(previous, &version) {
            // A slower CI run may finish after a newer per-target release.
            // Preserve the already-published entry instead of allowing stale
            // package bytes or metadata to race the registry transition.
            return Ok(previous.clone());
        }
    }

    let mut versions = previous.map_or_else(Vec::new, |entry| entry.versions.clone());
    if !artifact_exists {
        versions.push(version);
    }
    versions.sort_by(compare_registry_versions);
    let display_name = package.name.clone().unwrap_or_else(|| package.id.clone());
    let mut entry = registry_types::RegistryEntry {
        package_id: package.id.clone(),
        name: previous.map_or_else(|| display_name.clone().into(), |entry| entry.name.clone()),
        description: previous.map_or_else(
            || format!("{display_name} is a native Tilo OS application.").into(),
            |entry| entry.description.clone(),
        ),
        tagline: previous.map_or_else(
            || format!("{display_name} for Tilo OS").into(),
            |entry| entry.tagline.clone(),
        ),
        images: previous.map_or_else(Default::default, |entry| entry.images.clone()),
        supported_languages: previous.map_or_else(
            || vec!["en".into()],
            |entry| entry.supported_languages.clone(),
        ),
        categories: package.category.clone().into_iter().collect(),
        source_url: previous.and_then(|entry| entry.source_url.clone()),
        homepage: previous.and_then(|entry| entry.homepage.clone()),
        publisher_pubkey_hex: hex::encode(signer.verifying_key().to_bytes()),
        versions,
        updated_at_unix: previous.map_or(now, |entry| {
            now.max(entry.updated_at_unix.saturating_add(1))
        }),
        signature_hex: String::new(),
    };
    if let Some(metadata) = metadata {
        entry.name.clone_from(&metadata.name);
        entry.description.clone_from(&metadata.description);
        entry.tagline.clone_from(&metadata.tagline);
        entry.images.clone_from(&metadata.images);
        entry
            .supported_languages
            .clone_from(&metadata.supported_languages);
        entry.source_url.clone_from(&metadata.source_url);
        entry.homepage.clone_from(&metadata.homepage);
    }
    entry
        .validate()
        .map_err(|error| anyhow!("invalid registry metadata: {error}"))?;
    registry_types::sign_entry(&mut entry, signer);
    Ok(entry)
}

fn same_version_identity(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    tilo_app_package::version::compare(&left.version, &right.version).is_eq()
        && left.target.platform == right.target.platform
        && left.target.architecture == right.target.architecture
}

fn same_artifact(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    left.tapp_root_hash_hex == right.tapp_root_hash_hex
        && left.tapp_size_bytes == right.tapp_size_bytes
        && left.tapp_blake3_hex == right.tapp_blake3_hex
        && left.target == right.target
}

fn candidate_is_older_than_published(
    previous: &registry_types::RegistryEntry,
    candidate: &registry_types::RegistryVersion,
) -> bool {
    previous.versions.iter().any(|existing| {
        existing.target.platform == candidate.target.platform
            && existing.target.architecture == candidate.target.architecture
            && tilo_app_package::version::compare(&candidate.version, &existing.version).is_lt()
    })
}

fn compare_registry_versions(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> std::cmp::Ordering {
    let semantic = tilo_app_package::version::compare(&right.version, &left.version);
    semantic.then_with(|| left.target.architecture.cmp(&right.target.architecture))
}

fn cmd_entry(
    file: &Path,
    url: &str,
    identity: &Path,
    arch: Option<&str>,
    metadata: Option<&Path>,
    action: Option<RegistryAction>,
    previous_entry: Option<&Path>,
) -> Result<()> {
    let tapp_bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let signer = load_signing_key(identity)?;
    let arch = match arch {
        None => None,
        Some("x86_64") => Some(tilo_app_package::manifest::Arch::X86_64),
        Some("aarch64") => Some(tilo_app_package::manifest::Arch::AArch64),
        Some(other) => bail!("unknown --arch {other:?} (expected x86_64 or aarch64)"),
    };
    if matches!(action, Some(RegistryAction::Update)) && previous_entry.is_none() {
        bail!("--previous-entry is required with --action update");
    }
    let previous = previous_entry
        .map(|path| {
            let bytes = std::fs::read(path)
                .with_context(|| format!("read previous entry {}", path.display()))?;
            let entry: registry_types::RegistryEntry =
                serde_json::from_slice(&bytes).context("decode previous registry entry")?;
            if !registry_types::verify_entry_signature(&entry) {
                bail!("previous registry entry has an invalid publisher signature");
            }
            anyhow::Ok(entry)
        })
        .transpose()?;
    let metadata = metadata
        .map(|path| {
            let bytes = std::fs::read(path)
                .with_context(|| format!("read entry metadata {}", path.display()))?;
            serde_json::from_slice::<EntryMetadata>(&bytes).context("decode entry metadata JSON")
        })
        .transpose()?;
    let entry = registry_entry_for(
        &tapp_bytes,
        url,
        &signer,
        arch,
        previous.as_ref(),
        metadata.as_ref(),
    )?;
    let output = action.map_or_else(
        || serde_json::to_value(&entry),
        |action| {
            serde_json::to_value(registry_types::sign_change_request(
                action.into(),
                entry.clone(),
                &signer,
            ))
        },
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("encode entry")?
    );
    // `registry_entry_for` always builds exactly one version; `.first()`
    // avoids indexing so a future refactor there can't turn this into a panic.
    let version = entry.versions.first().map_or("?", |v| v.version.as_str());
    eprintln!("entry: {}@{version} -> {url}", entry.package_id);
    Ok(())
}

#[cfg(test)]
mod version_identity_tests {
    use super::*;

    fn version(value: &str) -> registry_types::RegistryVersion {
        registry_types::RegistryVersion {
            version: value.into(),
            tapp_url: String::new(),
            tapp_root_hash_hex: String::new(),
            tapp_size_bytes: 0,
            released_at_unix: 0,
            tapp_blake3_hex: String::new(),
            target: registry_types::RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
            },
        }
    }

    #[test]
    fn source_hash_cannot_create_a_second_artifact_at_the_same_precedence() {
        assert!(same_version_identity(
            &version("0.1.1-dev.10+g111"),
            &version("0.1.1-dev.10+g222")
        ));
        assert!(!same_version_identity(
            &version("0.1.1-dev.10+g111"),
            &version("0.1.1-dev.11+g000")
        ));
        assert!(
            compare_registry_versions(&version("0.1.1-dev.10+g111"), &version("0.1.1-dev.9+gzzz"))
                .is_lt()
        );
    }

    #[test]
    fn slower_release_is_stale_only_for_the_same_target() {
        let current = version("0.1.42+gnew");
        let entry = registry_types::RegistryEntry {
            package_id: "so.tilo.browser".into(),
            name: "Browser".into(),
            description: "Browser".into(),
            tagline: "Browse".into(),
            images: registry_types::LocalizedImages::default(),
            supported_languages: vec!["en".into()],
            categories: Vec::new(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: String::new(),
            versions: vec![current],
            updated_at_unix: 0,
            signature_hex: String::new(),
        };

        assert!(candidate_is_older_than_published(
            &entry,
            &version("0.1.41+gold")
        ));
        assert!(!candidate_is_older_than_published(
            &entry,
            &version("0.1.43+gnext")
        ));
        let mut arm = version("0.1.1+gold");
        arm.target.architecture = "aarch64".into();
        arm.target.isa = "aarch64-v8.2".into();
        assert!(!candidate_is_older_than_published(&entry, &arm));
    }
}
