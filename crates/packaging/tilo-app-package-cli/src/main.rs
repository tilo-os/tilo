//! `tilo-app-package` — CLI driver for the `tilo-app-package` format crate.
//!
//! A `.tapp` is a plain (uncompressed) tar archive; see
//! [`tilo_app_package::tapp`] for the layout. `pack` writes the full form,
//! which carries every payload blob for offline transfer; `pack --thin` and
//! `thin` write the thin form (signed root, manifest and collection only),
//! whose payload an installer fetches from whoever holds the bytes.
//!
//! Uncompressed because each blob is already content-addressed and,
//! deployed, lives in a content store doing dedup/streaming — gzip would
//! only waste CPU.

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
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use tilo_app_package::signed::SignedRoot;
use tilo_app_package::{
    BlobHash, BlobSource, DiscardBlobs, Manifest, PackageMeta, SigningIdentity, Tapp, Verification,
    pack, unpack, verify,
};

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

/// Registry platform family of the artifact `entry` describes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum TargetPlatform {
    /// Native Tilo OS thin app (`x86_64` or `aarch64`).
    #[default]
    TiloOs,
    /// WebAssembly side module for the browser toolkit core (`wasm32`).
    Web,
    /// Self-contained Windows executable (`x86_64` or `aarch64`).
    Windows,
    /// Self-contained macOS executable (`x86_64` or `aarch64`).
    Macos,
}

impl TargetPlatform {
    fn wire(self) -> &'static str {
        match self {
            Self::TiloOs => registry_types::TILO_OS_PLATFORM,
            Self::Web => registry_types::WEB_PLATFORM,
            Self::Windows => registry_types::WINDOWS_PLATFORM,
            Self::Macos => registry_types::MACOS_PLATFORM,
        }
    }
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
        /// Write the thin form: signed root, manifest and payload collection
        /// only. An installer fetches the payload bytes from its store, a
        /// peer or a registry.
        #[arg(long)]
        thin: bool,
    },
    /// Pack the Windows or macOS build of an app: one self-contained
    /// executable with the toolkit linked in, shipped as `bin/<file name>`
    /// under a signed `[target]`.
    PackDesktop {
        /// The authored manifest (TOML), the same one the Tilo OS build uses.
        /// Its `[package].runtime` names the toolkit release linked in.
        #[arg(long)]
        manifest: PathBuf,
        /// The release executable.
        #[arg(long)]
        executable: PathBuf,
        /// `windows` or `macos`.
        #[arg(long, value_enum)]
        platform: DesktopPlatform,
        /// `x86_64` or `aarch64`.
        #[arg(long)]
        arch: String,
        /// Feature level the executable was compiled for; defaults to the
        /// architecture's baseline (`x86_64`, `aarch64`).
        #[arg(long)]
        isa: Option<String>,
        /// Output .tapp path.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Identity JSON; a fresh keypair is generated when absent.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Pack the web build of an app (or of the toolkit core) from its
    /// authored manifest and WebAssembly module. The module's
    /// `tilo.runtime_abi` and `target_features` custom sections become the
    /// signed `[target]`; nothing in the manifest is web-specific.
    PackWeb {
        /// The authored manifest (TOML), the same one the native build uses.
        #[arg(long)]
        manifest: PathBuf,
        /// The app's WebAssembly side module (or the toolkit core module
        /// for `kind = "runtime"`). Packed at the payload root under its file name.
        #[arg(long)]
        module: PathBuf,
        /// Extra file shipped as `assets/<file name>`; repeatable.
        #[arg(long = "asset")]
        assets: Vec<PathBuf>,
        /// Directory whose files ship under `assets/`, keeping relative paths.
        #[arg(long)]
        asset_dir: Option<PathBuf>,
        /// Output .tapp path.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// Identity JSON; a fresh keypair is generated when absent.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Canonical wasm isa, for a module without a `target_features` section.
        #[arg(long)]
        wasm_isa: Option<String>,
        /// Toolkit-core ABI id, `tilo-runtime/<version>+wasm.<32 hex>`, for a
        /// module without a `tilo.runtime_abi` section; must match the
        /// section when both exist.
        #[arg(long)]
        runtime_abi: Option<String>,
        /// For `kind = "runtime"`: the release this core must provide,
        /// `tilo-runtime/<version>`; checked against the ABI id.
        #[arg(long)]
        provides: Option<String>,
        /// Write the thin form (metadata only).
        #[arg(long)]
        thin: bool,
    },
    /// Write the thin form of an existing .tapp (metadata only) to `output`.
    Thin {
        /// Output path for the thin .tapp.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// The .tapp to thin.
        file: PathBuf,
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
        /// Platform family of the tapp: a native Tilo OS build or a web
        /// build made by `pack-web`.
        #[arg(long, value_enum, default_value_t)]
        platform: TargetPlatform,
        /// Target architecture: `x86_64` or `aarch64` (inferred when the
        /// package manifest declares exactly one), or `wasm32` for web.
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
            thin,
        } => cmd_pack(&manifest, &payload, &output, identity.as_deref(), thin),
        Command::PackWeb {
            manifest,
            module,
            assets,
            asset_dir,
            output,
            identity,
            wasm_isa,
            runtime_abi,
            provides,
            thin,
        } => cmd_pack_web(&PackWebArgs {
            manifest: &manifest,
            module: &module,
            assets: &assets,
            asset_dir: asset_dir.as_deref(),
            output: &output,
            identity: identity.as_deref(),
            options: tilo_app_package::web::WebBuildOptions {
                isa: wasm_isa.as_deref(),
                runtime_abi: runtime_abi.as_deref(),
                provides: provides.as_deref(),
            },
            thin,
        }),
        Command::PackDesktop {
            manifest,
            executable,
            platform,
            arch,
            isa,
            output,
            identity,
        } => cmd_pack_desktop(&PackDesktopArgs {
            manifest: &manifest,
            executable: &executable,
            platform,
            isa: isa.as_deref().unwrap_or(&arch),
            arch: &arch,
            output: &output,
            identity: identity.as_deref(),
        }),
        Command::Thin { output, file } => cmd_thin(&file, &output),
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
            platform,
            arch,
            metadata,
            action,
            previous_entry,
            file,
        } => cmd_entry(
            &file,
            &url,
            &identity,
            platform,
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
    thin: bool,
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

    let packed =
        pack(payload_dir, &manifest, &identity).map_err(|e| anyhow!("pack failed: {e}"))?;
    let payload: Option<&dyn BlobSource> = if thin { None } else { Some(&packed) };
    write_tapp(output, &packed.meta, payload)?;
    eprintln!(
        "packed {} ({} form, {} payload blobs)",
        packed.id(),
        if thin { "thin" } else { "full" },
        packed.meta.payload_hashes().len()
    );
    Ok(())
}

struct PackWebArgs<'a> {
    manifest: &'a Path,
    module: &'a Path,
    assets: &'a [PathBuf],
    asset_dir: Option<&'a Path>,
    output: &'a Path,
    identity: Option<&'a Path>,
    options: tilo_app_package::web::WebBuildOptions<'a>,
    thin: bool,
}

fn cmd_pack_web(args: &PackWebArgs<'_>) -> Result<()> {
    let manifest_src = std::fs::read_to_string(args.manifest)
        .with_context(|| format!("read manifest {}", args.manifest.display()))?;
    let mut authored = Manifest::from_toml_str(&manifest_src).context("decode manifest TOML")?;
    let identity = match args.identity {
        Some(path) => load_identity(path)?,
        None => SigningIdentity::generate(),
    };
    authored.publisher.identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));

    let module_bytes = std::fs::read(args.module)
        .with_context(|| format!("read module {}", args.module.display()))?;
    let module_name = args
        .module
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "module path {} has no UTF-8 file name",
                args.module.display()
            )
        })?;
    let info = tilo_app_package::web::inspect_wasm_module(&module_bytes)
        .with_context(|| format!("inspect {}", args.module.display()))?;
    let manifest =
        tilo_app_package::web::web_manifest(&authored, module_name, &info, &args.options)?;

    let staging = tempfile::tempdir().context("create staging directory")?;
    std::fs::write(staging.path().join(module_name), &module_bytes)
        .context("stage WebAssembly module")?;
    let assets_root = staging.path().join("assets");
    for asset in args.assets {
        let name = asset
            .file_name()
            .ok_or_else(|| anyhow!("asset {} has no file name", asset.display()))?;
        std::fs::create_dir_all(&assets_root).context("create assets directory")?;
        let destination = assets_root.join(name);
        if destination.exists() {
            bail!("two assets are named {}", name.to_string_lossy());
        }
        std::fs::copy(asset, &destination)
            .with_context(|| format!("stage asset {}", asset.display()))?;
    }
    if let Some(dir) = args.asset_dir {
        copy_regular_tree(dir, &assets_root)?;
    }

    let packed = pack(staging.path(), &manifest, &identity)
        .map_err(|error| anyhow!("pack failed: {error}"))?;
    let payload: Option<&dyn BlobSource> = if args.thin { None } else { Some(&packed) };
    write_tapp(args.output, &packed.meta, payload)?;
    let target = manifest
        .target
        .as_ref()
        .ok_or_else(|| anyhow!("web manifest lost its target"))?;
    eprintln!(
        "packed web {} for {} ({} payload blobs)",
        packed.id(),
        target.runtime,
        packed.meta.payload_hashes().len()
    );
    Ok(())
}

/// The desktop systems `pack-desktop` builds for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DesktopPlatform {
    Windows,
    Macos,
}

struct PackDesktopArgs<'a> {
    manifest: &'a Path,
    executable: &'a Path,
    platform: DesktopPlatform,
    arch: &'a str,
    isa: &'a str,
    output: &'a Path,
    identity: Option<&'a Path>,
}

fn cmd_pack_desktop(args: &PackDesktopArgs<'_>) -> Result<()> {
    let manifest_src = std::fs::read_to_string(args.manifest)
        .with_context(|| format!("read manifest {}", args.manifest.display()))?;
    let mut authored = Manifest::from_toml_str(&manifest_src).context("decode manifest TOML")?;
    let identity = match args.identity {
        Some(path) => load_identity(path)?,
        None => SigningIdentity::generate(),
    };
    authored.publisher.identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
    let name = args
        .executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "executable path {} has no UTF-8 file name",
                args.executable.display()
            )
        })?;
    let module = format!("bin/{name}");
    let platform = match args.platform {
        DesktopPlatform::Windows => registry_types::WINDOWS_PLATFORM,
        DesktopPlatform::Macos => registry_types::MACOS_PLATFORM,
    };
    let manifest = tilo_app_package::desktop::desktop_manifest(
        &authored, platform, args.arch, args.isa, &module,
    )?;

    let staging = tempfile::tempdir().context("create staging directory")?;
    std::fs::create_dir_all(staging.path().join("bin")).context("create bin directory")?;
    std::fs::copy(args.executable, staging.path().join(&module))
        .with_context(|| format!("stage executable {}", args.executable.display()))?;
    let packed = pack(staging.path(), &manifest, &identity)
        .map_err(|error| anyhow!("pack failed: {error}"))?;
    write_tapp(args.output, &packed.meta, Some(&packed))?;
    eprintln!(
        "packed {platform}/{} {} linking {}",
        args.arch,
        packed.id(),
        manifest.package.runtime.as_deref().unwrap_or_default()
    );
    Ok(())
}

/// Copy every regular file under `from` to the same relative path under
/// `to`. Symlinks and special files are refused: a package ships bytes.
fn copy_regular_tree(from: &Path, to: &Path) -> Result<()> {
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let source = from.join(&relative);
        for entry in std::fs::read_dir(&source)
            .with_context(|| format!("read asset directory {}", source.display()))?
        {
            let entry = entry.with_context(|| format!("read {}", source.display()))?;
            let kind = entry.file_type()?;
            let path = relative.join(entry.file_name());
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let destination = to.join(&path);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if destination.exists() {
                    bail!("asset {} is given twice", path.display());
                }
                std::fs::copy(entry.path(), &destination)
                    .with_context(|| format!("stage asset {}", entry.path().display()))?;
            } else {
                bail!(
                    "asset {} is not a regular file or directory",
                    entry.path().display()
                );
            }
        }
    }
    Ok(())
}

fn cmd_thin(file: &Path, output: &Path) -> Result<()> {
    let (tapp, _) = read_tapp(file, false)?;
    write_tapp(output, &tapp.meta, None)?;
    eprintln!("wrote thin {} -> {}", tapp.meta.id, output.display());
    Ok(())
}

fn cmd_sign(identity_path: &Path, file: &Path) -> Result<()> {
    let identity = load_identity(identity_path)?;
    let (tapp, blobs) = read_tapp(file, true)?;
    let signed = &tapp.meta.root;
    if signed.publisher_identity != identity.public_key_bytes() {
        bail!(
            "identity does not match the package's publisher identity (pkg={}, ours={})",
            hex::encode(signed.publisher_identity),
            hex::encode(identity.public_key_bytes())
        );
    }
    // Sign with the has_signature bit already set so verify recomputes the same bytes.
    let mut pre_signed = signed.clone();
    pre_signed.signature = Some([0u8; 64]);
    let sig = identity
        .sign_signed_root(&pre_signed)
        .map_err(|e| anyhow!("sign: {e}"))?;
    let new_meta = PackageMeta::from_root(
        SignedRoot {
            signature: Some(sig),
            ..pre_signed
        },
        tapp.meta.manifest_bytes.clone(),
        tapp.meta.collection_bytes.clone(),
    )?;
    let payload: Option<&dyn BlobSource> = if tapp.carried.is_empty() {
        None
    } else {
        Some(&blobs)
    };
    write_tapp(file, &new_meta, payload)?;
    eprintln!("re-signed {} -> {}", tapp.meta.id, new_meta.id);
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
    // Decoding checks the hash chain and every carried payload blob.
    let tapp = match read_tapp(file, false) {
        Ok((tapp, _)) => tapp,
        Err(error) => {
            println!("FAILED: {error:#}");
            std::process::exit(1);
        }
    };
    let id = tapp.meta.id;
    let missing = tapp.missing();
    if !tapp.carried.is_empty() && !missing.is_empty() {
        println!(
            "FAILED: {id} carries some payload blobs but lacks {} others",
            missing.len()
        );
        std::process::exit(1);
    }
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
        keys.push(tapp.meta.publisher());
    }
    let form = if missing.is_empty() {
        "full"
    } else {
        "thin: payload bytes are fetched on install"
    };
    match verify(&tapp.meta, &keys) {
        Verification::Trusted { .. } => {
            println!("ok: {id} verifies and publisher is trusted ({form})");
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
    let (tapp, _) = read_tapp(file, false)?;
    let meta = &tapp.meta;
    println!("package-id: {}", meta.id);
    println!("archive-size: {archive_size} bytes");
    println!(
        "form: {}",
        if tapp.is_complete() {
            "full"
        } else if tapp.carried.is_empty() {
            "thin"
        } else {
            "partial"
        }
    );
    println!("manifest-hash: blake3:{}", meta.root.manifest_hash);
    println!("payload-root-hash: blake3:{}", meta.root.payload_root_hash);
    println!(
        "publisher-identity: ed25519:{}",
        hex::encode(meta.publisher())
    );
    println!(
        "signed: {}",
        if meta.root.signature.is_some() {
            "yes"
        } else {
            "NO"
        }
    );
    let manifest_str =
        std::str::from_utf8(&meta.manifest_bytes).context("manifest is not UTF-8")?;
    println!("\n--- manifest.toml ---");
    println!("{manifest_str}");
    if tapp.is_complete() {
        let payload_size: u64 = meta
            .collection
            .entries
            .values()
            .filter_map(|hash| tapp.carried.get(hash))
            .sum();
        println!("payload-size: {payload_size} bytes");
        println!(
            "package-overhead: {} bytes",
            archive_size.saturating_sub(payload_size)
        );
    }
    println!("--- payload ({} files) ---", meta.collection.entries.len());
    for (p, h) in &meta.collection.entries {
        match tapp.carried.get(h) {
            Some(size) => println!("  {p}  (blake3:{h}, {size} bytes)"),
            None => println!("  {p}  (blake3:{h}, not carried)"),
        }
    }
    Ok(())
}

fn cmd_unpack(dest: &Path, file: &Path) -> Result<()> {
    let (tapp, blobs) = read_tapp(file, true)?;
    if !tapp.is_complete() {
        bail!(
            "{} is thin: {} payload blob(s) are not in the archive; install it instead",
            file.display(),
            tapp.missing().len()
        );
    }
    unpack(&tapp.meta, &blobs, dest).map_err(|e| anyhow!("unpack: {e}"))?;
    // Also drop a manifest.toml at the destination root for convenience —
    // unpack only writes the payload tree, but a developer inspecting it
    // expects to see the manifest too.
    std::fs::write(dest.join("manifest.toml"), &tapp.meta.manifest_bytes)
        .context("write manifest.toml")?;
    eprintln!("unpacked {} -> {}", tapp.meta.id, dest.display());
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

    // HTTPS target: emit the signed entry to the registry endpoint, which
    // ingests the blob out of band via the entry's ticket.
    if registry.starts_with("http://") || registry.starts_with("https://") {
        let (entry, _package_id) = app_registry::build_entry(&tapp_bytes, &publisher, arch, None)
            .map_err(|e| anyhow!("build entry: {e}"))?;
        tilo_rt::block_on(app_registry::publish::emit_entry_https(registry, &entry))?
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

    tilo_rt::block_on(async move {
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
    })??;
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

/// Write `meta` (full when `payload` is given, thin otherwise) to `path`
/// atomically, enforcing the application size ceiling on the result.
fn write_tapp(path: &Path, meta: &PackageMeta, payload: Option<&dyn BlobSource>) -> Result<()> {
    let temp = path.with_extension(format!("tapp.tmp.{}", std::process::id()));
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("create temporary package {}", temp.display()))?;
    let mut pending = PendingTapp(temp.clone());
    let mut file = tilo_app_package::tapp::write(std::io::BufWriter::new(file), meta, payload)
        .context("write package archive")?;
    file.flush().context("flush package archive")?;
    drop(file);
    let archive_size = std::fs::metadata(&temp)
        .with_context(|| format!("stat temporary package {}", temp.display()))?
        .len();
    if let Some(limit) = tilo_app_package::app_size_limit(&meta.manifest)
        && archive_size > limit
    {
        bail!("application tapp is {archive_size} bytes; limit is {limit} bytes");
    }
    std::fs::rename(&temp, path).with_context(|| format!("install package {}", path.display()))?;
    pending.0.clear();
    Ok(())
}

/// Decode the archive at `path`. With `keep_blobs` the carried payload is
/// returned in memory (applications are capped at a few MiB); otherwise it
/// is verified and dropped.
fn read_tapp(path: &Path, keep_blobs: bool) -> Result<(Tapp, BTreeMap<BlobHash, Vec<u8>>)> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut blobs = BTreeMap::new();
    let tapp = if keep_blobs {
        tilo_app_package::tapp::read(reader, &mut blobs)
    } else {
        tilo_app_package::tapp::read(reader, &mut DiscardBlobs)
    }
    .with_context(|| format!("decode {}", path.display()))?;
    Ok((tapp, blobs))
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
    platform: TargetPlatform,
    arch: Option<&str>,
    previous: Option<&registry_types::RegistryEntry>,
    metadata: Option<&EntryMetadata>,
) -> Result<registry_types::RegistryEntry> {
    let publisher = SigningIdentity::from_signing_key(signer.clone());
    let binding = match platform {
        TargetPlatform::TiloOs => native_binding(tapp_bytes, &publisher, arch)?,
        TargetPlatform::Web | TargetPlatform::Windows | TargetPlatform::Macos => {
            signed_target_binding(tapp_bytes, &publisher, platform, arch)?
        }
    };
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
        tapp_root_hash_hex: binding.package_id.to_hex(),
        tapp_size_bytes: tapp_bytes.len() as u64,
        released_at_unix: now,
        tapp_blake3_hex: binding.tapp_blake3_hex,
        target: binding.target,
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
                    "version {} for target {}/{} already exists with different package bytes or metadata",
                    version.version,
                    version.target.platform,
                    version.target.architecture,
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
            || match platform {
                TargetPlatform::TiloOs => {
                    format!("{display_name} is a native Tilo OS application.").into()
                }
                TargetPlatform::Web | TargetPlatform::Windows | TargetPlatform::Macos => {
                    format!("{display_name} is a Tilo application.").into()
                }
            },
            |entry| entry.description.clone(),
        ),
        tagline: previous.map_or_else(
            || match platform {
                TargetPlatform::TiloOs => format!("{display_name} for Tilo OS").into(),
                TargetPlatform::Web => format!("{display_name} on Tilo").into(),
                TargetPlatform::Windows => format!("{display_name} for Windows").into(),
                TargetPlatform::Macos => format!("{display_name} for macOS").into(),
            },
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

/// What a registry version binds from the final tapp bytes.
struct ArtifactBinding {
    package_id: tilo_app_package::PackageId,
    tapp_blake3_hex: String,
    target: registry_types::RegistryTarget,
}

fn native_binding(
    tapp_bytes: &[u8],
    publisher: &SigningIdentity,
    arch: Option<&str>,
) -> Result<ArtifactBinding> {
    let arch = match arch {
        None => None,
        Some("x86_64") => Some(tilo_app_package::manifest::Arch::X86_64),
        Some("aarch64") => Some(tilo_app_package::manifest::Arch::AArch64),
        Some(other) => bail!("unknown --arch {other:?} for tilo-os (expected x86_64 or aarch64)"),
    };
    // Use the same trust-boundary path as `publish`: this verifies every
    // signed blob, materialized payload limits and payload shape before any
    // registry metadata is signed.
    let (built, package_id) = app_registry::build_entry(tapp_bytes, publisher, arch, None)
        .map_err(|error| anyhow!("verify tapp for registry entry: {error}"))?;
    Ok(ArtifactBinding {
        package_id,
        tapp_blake3_hex: built.tapp_hash,
        target: registry_types::RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: app_registry::index::arch_wire(built.arch).to_string(),
            isa: built.isa,
            runtime: built.runtime,
        },
    })
}

/// A web, Windows or macOS build binds the `[target]` its signed manifest
/// records; the tapp passes the same verification a native one does before
/// anything is signed.
fn signed_target_binding(
    tapp_bytes: &[u8],
    publisher: &SigningIdentity,
    platform: TargetPlatform,
    arch: Option<&str>,
) -> Result<ArtifactBinding> {
    if platform == TargetPlatform::Web
        && arch.is_some_and(|arch| arch != registry_types::WASM32_ARCHITECTURE)
    {
        bail!("web builds target --arch wasm32");
    }
    let meta = app_registry::tapp::load(tapp_bytes).map_err(|e| anyhow!("load tapp: {e}"))?;
    app_registry::tapp::validate_payload_shape(&meta)
        .map_err(|error| anyhow!("verify tapp for registry entry: {error}"))?;
    app_registry::tapp::verify_bytes(tapp_bytes, meta.package_id, publisher.public_key_bytes())
        .map_err(|error| anyhow!("verify tapp for registry entry: {error}"))?;
    if let Some(limit) = tilo_app_package::app_size_limit(&meta.manifest)
        && tapp_bytes.len() as u64 > limit
    {
        bail!(
            "application .tapp is {} bytes; limit is {limit} bytes",
            tapp_bytes.len()
        );
    }
    let target = meta.manifest.target.as_ref().ok_or_else(|| {
        anyhow!(
            "the tapp has no [target]; it is a Tilo OS build, not a {} build",
            platform.wire()
        )
    })?;
    let target = registry_types::RegistryTarget {
        platform: target.platform.clone(),
        architecture: target.architecture.clone(),
        isa: target.isa.clone(),
        runtime: Some(target.runtime.clone()),
    };
    if target.platform != platform.wire() {
        bail!(
            "the tapp's target is {}, not {}",
            target.platform,
            platform.wire()
        );
    }
    if let Some(arch) = arch
        && arch != target.architecture
    {
        bail!(
            "the tapp was built for {}, not --arch {arch}",
            target.architecture
        );
    }
    Ok(ArtifactBinding {
        package_id: meta.package_id,
        tapp_blake3_hex: blake3::hash(tapp_bytes).to_hex().to_string(),
        target,
    })
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

#[allow(
    clippy::too_many_arguments,
    reason = "the entry subcommand's explicit CLI options are kept visible at the command boundary"
)]
fn cmd_entry(
    file: &Path,
    url: &str,
    identity: &Path,
    platform: TargetPlatform,
    arch: Option<&str>,
    metadata: Option<&Path>,
    action: Option<RegistryAction>,
    previous_entry: Option<&Path>,
) -> Result<()> {
    let tapp_bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let signer = load_signing_key(identity)?;
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
        platform,
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
    fn version_identity_is_unique_per_precedence_and_target() {
        // phase: source_hash_cannot_create_a_second_artifact_at_the_same_precedence
        {
            assert!(same_version_identity(
                &version("0.1.1-dev.10+g111"),
                &version("0.1.1-dev.10+g222")
            ));
            assert!(!same_version_identity(
                &version("0.1.1-dev.10+g111"),
                &version("0.1.1-dev.11+g000")
            ));
            assert!(
                compare_registry_versions(
                    &version("0.1.1-dev.10+g111"),
                    &version("0.1.1-dev.9+gzzz")
                )
                .is_lt()
            );
        }
        // phase: slower_release_is_stale_only_for_the_same_target
        {
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
}
