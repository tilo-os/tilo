//! `qos-pkg` — CLI driver for the `qos-pkg` foundation crate.
//!
//! ## `.qpkg` on-disk format
//!
//! A `.qpkg` is a plain (uncompressed) tar archive containing:
//!
//! ```text
//! qpkg.toml          # tiny header: package_id (qpkg:<hex>) + format version
//! signed-root        # the 173-byte signed-root blob (binary)
//! manifest.toml      # the manifest, UTF-8 TOML
//! collection.qcl     # the encoded payload Collection
//! blobs/<hex>        # each payload blob, named by its BLAKE3 hex hash
//! ```
//!
//! Why uncompressed: each blob inside is already content-addressed
//! and (in deployed qOS) lives inside an iroh-blobs store that
//! does chunk-level deduplication / streaming. Adding gzip over
//! that would just waste CPU; the loader streams blobs straight
//! out of the tar at install time.
//!
//! The `qpkg.toml` header is convention-only — strictly speaking
//! it duplicates information already present in the signed-root.
//! It exists so a human running `tar tf foo.qpkg` sees the
//! PackageId without decoding the binary signed-root.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use qos_pkg::blobs::{BlobHash, BlobStore, Collection};
use qos_pkg::signed::SignedRoot;
use qos_pkg::{Manifest, MemoryBlobStore, PackageId, SigningIdentity, Verification, pack, unpack, verify};

/// The header entry inside a `.qpkg` tar. Tiny, deliberately
/// minimal so it can be deserialised without pulling the rest
/// of the crate.
#[derive(Debug, Serialize, Deserialize)]
struct QpkgHeader {
    /// `qpkg:<hex>` form.
    package_id: String,
    /// Format version of the .qpkg layout itself. Bumped
    /// independently from `qos-pkg`'s signed-root version.
    format: u32,
}

const QPKG_FORMAT: u32 = 1;

#[derive(Parser)]
#[command(
    name = "qos-pkg",
    about = "Pack / sign / verify / inspect qOS packages (.qpkg)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pack a payload directory + manifest into a .qpkg, signing
    /// it with a freshly-generated identity unless --identity is
    /// passed.
    Pack {
        /// Path to the manifest (TOML).
        #[arg(long)]
        manifest: PathBuf,
        /// Path to the payload directory.
        #[arg(long)]
        payload: PathBuf,
        /// Output .qpkg path.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Optional identity JSON. If absent, a fresh keypair is
        /// generated and its public key replaces
        /// `publisher.identity` in the embedded manifest copy.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Re-sign an existing .qpkg with the given identity, writing
    /// the result back into the same file.
    Sign {
        #[arg(long)]
        identity: PathBuf,
        /// Path to the .qpkg.
        file: PathBuf,
    },
    /// Verify a .qpkg — checks every cryptographic invariant
    /// and reports the first one that breaks. By default the
    /// embedded publisher identity is treated as trusted (so the
    /// command succeeds on a self-signed package); pass
    /// --trusted-key to override.
    Verify {
        /// Hex-encoded trusted Ed25519 public key (32 bytes).
        /// Repeatable. If empty, the embedded publisher key is
        /// auto-trusted.
        #[arg(long)]
        trusted_key: Vec<String>,
        file: PathBuf,
    },
    /// Print the manifest + payload tree of a .qpkg.
    Inspect {
        file: PathBuf,
    },
    /// Extract the payload + a copy of the manifest into `dest`.
    Unpack {
        #[arg(long)]
        dest: PathBuf,
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
    }
}

fn cmd_pack(
    manifest_path: &Path,
    payload_dir: &Path,
    output: &Path,
    identity_path: Option<&Path>,
) -> Result<()> {
    let manifest_src = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest {manifest_path:?}"))?;
    let mut manifest =
        Manifest::from_toml_str(&manifest_src).context("decode manifest TOML")?;

    let identity = match identity_path {
        Some(p) => load_identity(p)?,
        None => SigningIdentity::generate(),
    };

    // If the manifest's `publisher.identity` was a placeholder
    // (zeros, or a different hex) we patch it to the signing
    // identity's public key. This matches the CLI's documented
    // behaviour: pack-fresh keys produce a self-consistent
    // manifest without the author needing to round-trip through
    // sign first.
    let pub_hex = hex::encode(identity.public_key_bytes());
    manifest.publisher.identity = format!("ed25519:{pub_hex}");

    let mut store = MemoryBlobStore::new();
    let id = pack(payload_dir, &manifest, &identity, &mut store)
        .map_err(|e| anyhow!("pack failed: {e}"))?;
    write_qpkg(output, id, &store)?;
    eprintln!("packed {} ({} blobs)", id, store.len());
    Ok(())
}

fn cmd_sign(identity_path: &Path, file: &Path) -> Result<()> {
    let identity = load_identity(identity_path)?;
    let (id, store) = read_qpkg(file)?;
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
    // Sign-with-has_signature-bit-already-set so verify can
    // recompute the same bytes on its side.
    let mut pre_signed = signed.clone();
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
    write_qpkg(file, new_id, &new_store)?;
    eprintln!("re-signed {} -> {}", id, new_id);
    Ok(())
}

fn cmd_verify(trusted_hex: &[String], file: &Path) -> Result<()> {
    let (id, store) = read_qpkg(file)?;
    // Trust set: caller's keys + (if empty) the embedded
    // publisher key for convenience.
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
    let (id, store) = read_qpkg(file)?;
    println!("package-id: {id}");
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
        if signed.signature.is_some() { "yes" } else { "NO" }
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
    println!("--- payload ({} files) ---", collection.entries.len());
    for (p, h) in &collection.entries {
        let size = store
            .get(h)
            .ok()
            .flatten()
            .map(|b| b.len())
            .unwrap_or(0);
        println!("  {p}  (blake3:{}, {size} bytes)", h);
    }
    Ok(())
}

fn cmd_unpack(dest: &Path, file: &Path) -> Result<()> {
    let (id, store) = read_qpkg(file)?;
    let manifest = unpack(id, &store, dest).map_err(|e| anyhow!("unpack: {e}"))?;
    // Also drop a manifest.toml at the destination root for
    // convenience — unpack only writes the payload tree, but
    // a developer inspecting an unpacked package expects to
    // see the manifest too.
    let toml = manifest
        .to_toml_string()
        .map_err(|e| anyhow!("re-encode manifest: {e}"))?;
    std::fs::write(dest.join("manifest.toml"), toml).context("write manifest.toml")?;
    eprintln!("unpacked {id} -> {dest:?}");
    Ok(())
}

// ----- .qpkg I/O ---------------------------------------------------

fn write_qpkg(path: &Path, id: PackageId, store: &MemoryBlobStore) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {path:?}"))?;
    let mut tar = tar::Builder::new(file);

    let header = QpkgHeader {
        package_id: id.to_string(),
        format: QPKG_FORMAT,
    };
    let header_toml = toml_to_string(&header)?;
    tar_append_str(&mut tar, "qpkg.toml", &header_toml)?;

    // signed-root
    let signed = store
        .get(&BlobHash(*id.as_bytes()))
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("signed-root blob missing"))?;
    tar_append_bytes(&mut tar, "signed-root", &signed)?;

    // manifest + collection (named for the human reader; the
    // verifier doesn't actually use these names — it walks via
    // the hashes in the signed-root).
    let signed_decoded = SignedRoot::decode(&signed)?;
    let manifest = store
        .get(&signed_decoded.manifest_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("manifest blob missing"))?;
    tar_append_bytes(&mut tar, "manifest.toml", &manifest)?;
    let collection = store
        .get(&signed_decoded.payload_root_hash)
        .map_err(|e| anyhow!("store: {e}"))?
        .ok_or_else(|| anyhow!("collection blob missing"))?;
    tar_append_bytes(&mut tar, "collection.qcl", &collection)?;

    // Per-file blobs.
    for (h, b) in store.iter() {
        // Skip the three blobs we already wrote under their
        // human-readable names; they live under `blobs/<hex>` too
        // for downstream tooling that wants to dedupe by hash, so
        // write them once under both names.
        let name = format!("blobs/{}", h);
        tar_append_bytes(&mut tar, &name, b)?;
    }
    tar.finish().context("finalise tar")?;
    Ok(())
}

fn read_qpkg(path: &Path) -> Result<(PackageId, MemoryBlobStore)> {
    let file = File::open(path).with_context(|| format!("open {path:?}"))?;
    let mut ar = tar::Archive::new(file);
    let mut header: Option<QpkgHeader> = None;
    let mut store = MemoryBlobStore::new();
    for entry in ar.entries().context("read tar entries")? {
        let mut entry = entry?;
        let entry_path = entry
            .path()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let name = entry_path.to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read tar entry")?;
        match name.as_str() {
            "qpkg.toml" => {
                let s = String::from_utf8(buf).context("qpkg.toml not UTF-8")?;
                header = Some(toml_from_str(&s)?);
            }
            n if n.starts_with("blobs/") => {
                store.put(&buf).map_err(|e| anyhow!("store: {e}"))?;
            }
            // signed-root, manifest.toml, collection.qcl are
            // duplicated under blobs/<hex> too, so we can
            // ingest them generically there. But if a packager
            // omitted the blobs/ duplicates we still want the
            // store populated, so ingest these too.
            "signed-root" | "manifest.toml" | "collection.qcl" => {
                store.put(&buf).map_err(|e| anyhow!("store: {e}"))?;
            }
            _ => { /* unknown entry — ignore */ }
        }
    }
    let header = header.ok_or_else(|| anyhow!(".qpkg is missing qpkg.toml"))?;
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
    Ok(toml::to_string_pretty(v).context("toml encode qpkg.toml")?)
}

fn toml_from_str(s: &str) -> Result<QpkgHeader> {
    Ok(toml::from_str(s).context("toml decode qpkg.toml")?)
}

// ----- identity I/O ------------------------------------------------

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    /// 32-byte secret-key seed, hex-encoded.
    secret_key_hex: String,
}

fn load_identity(path: &Path) -> Result<SigningIdentity> {
    let bytes = std::fs::read(path).with_context(|| format!("read identity {path:?}"))?;
    let f: IdentityFile = serde_json::from_slice(&bytes).context("parse identity JSON")?;
    let raw = hex::decode(&f.secret_key_hex).context("identity secret-key hex")?;
    let raw_arr: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("identity secret-key must be 32 bytes"))?;
    let sk = SigningKey::from_bytes(&raw_arr);
    Ok(SigningIdentity::from_signing_key(sk))
}
