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

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
mod attest;

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
    /// Self-contained executable for other Linux desktops (`x86_64` or `aarch64`).
    Linux,
    /// Self-contained Windows executable (`x86_64` or `aarch64`).
    Windows,
    /// Self-contained macOS executable (`x86_64` or `aarch64`).
    Macos,
    /// Android package, an APK or AAB (`aarch64` or `x86_64`).
    Android,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RegistryAction {
    Add,
    Update,
    Delete,
}

impl From<TargetPlatform> for registry_client::Platform {
    fn from(value: TargetPlatform) -> Self {
        match value {
            TargetPlatform::TiloOs => Self::TiloOs,
            TargetPlatform::Linux => Self::Linux,
            TargetPlatform::Windows => Self::Windows,
            TargetPlatform::Macos => Self::Macos,
            TargetPlatform::Android => Self::Android,
        }
    }
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
    /// Pack the Linux, Windows or macOS build of an app: one self-contained
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
        /// `linux`, `windows` or `macos`.
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
        /// Validate and print an unsigned plan without selecting a signing key
        /// or writing an archive.
        #[arg(long, conflicts_with = "identity")]
        dry_run: bool,
    },
    /// Write the thin form of an existing .tapp (metadata only) to `output`.
    Thin {
        /// Output path for the thin .tapp.
        #[arg(short = 'o', long)]
        output: PathBuf,
        /// The .tapp to thin.
        file: PathBuf,
    },
    /// Split a full .tapp for hosting: write its thin form under the same
    /// file name into `--out`, plus every payload blob as `<blake3>.blob`,
    /// or, past 16 MiB, as `<blake3>.chunks` naming 16 MiB `<blake3>.blob`
    /// chunks. Upload every file in `--out` into one directory, then build
    /// the entry from the thin .tapp with `entry --objects <out>`.
    Split {
        /// Directory for the thin .tapp and its payload objects.
        #[arg(long)]
        out: PathBuf,
        /// The full .tapp to split.
        file: PathBuf,
    },
    /// Write the compressed twin of a .tapp, `<file>.zst` unless `-o` names
    /// another path: the form clients download first when it sits beside the
    /// package at its signed URL. The twin is checked to decompress to the
    /// exact package before it is written; signatures and the registry entry
    /// still cover only the package.
    Compress {
        /// Output path for the twin; `<file>.zst` by default.
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// The .tapp to compress.
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
    /// Publish a signed .tapp to a registry directory: sign an index entry,
    /// upload the blob to iroh-blobs, and re-sign the directory's index.
    ///
    /// `--registry` is a local directory or `file://` URL; publish to an HTTP
    /// registry with `tilo-app publish`. `--dry-run` prints the signed entry +
    /// content hash without uploading.
    Publish {
        /// Target registry: a local dir or `file://…` (self-hosted).
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
    /// Co-sign an existing complete package with an attestor and its original publisher.
    /// Payload bytes remain unchanged; the attested manifest gets a new signed root.
    Attest {
        /// Original publisher identity JSON; a different publisher is refused.
        #[arg(long)]
        publisher_identity: PathBuf,
        /// Attestor signing identity JSON generated by keygen.
        #[arg(long)]
        attestor_identity: PathBuf,
        /// Attestor ID matching the user's separately enrolled trust record.
        #[arg(long)]
        attestor: String,
        /// Intended recipient trust scope (all, prefix:... or mime:...).
        /// This verifies scope before signing; recipient trust remains authoritative.
        #[arg(long)]
        scope: String,
        /// Complete input package; incomplete thin packages are refused.
        file: PathBuf,
        /// Fresh output; existing files and the input are never overwritten.
        #[arg(short = 'o', long)]
        output: PathBuf,
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
    Entry(EntryArgs),
    /// Print a signed registry entry for a bot program (the Shortcut's
    /// canonical JSON); with `--action`, wrapped as a request for POST /entries.
    BotEntry(BotEntryArgs),
    /// Print the registry entry this publisher has waiting for review for a
    /// package id, as `--previous-entry` accepts it; prints nothing when none
    /// is waiting. A newer request replaces the waiting one and must keep its
    /// versions, so it builds on this entry rather than the published one.
    Pending(PendingArgs),
    /// Seal a directory tree into a signed environment image: an
    /// `env-runtime` (a minimal userland), or a `tool` holding what some
    /// packages add on top of the exact env-runtime `--runtime` names.
    /// Execute bits, symbolic links and empty directories are kept; the
    /// package id is printed on stdout.
    SealImage(SealImageArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ImageKind {
    EnvRuntime,
    Tool,
}

#[derive(clap::Args)]
struct SealImageArgs {
    /// What the image is.
    #[arg(long, value_enum)]
    kind: ImageKind,
    /// The directory to seal, holding the image's `usr/` and `etc/`.
    #[arg(long)]
    tree: PathBuf,
    /// Reverse-DNS package id, such as `so.tilo.env-runtime`.
    #[arg(long)]
    id: String,
    /// Semver of this build.
    #[arg(long)]
    version: String,
    /// Display name.
    #[arg(long)]
    name: Option<String>,
    /// Architecture of the image's binaries (`x86_64` or `aarch64`);
    /// defaults to this machine's.
    #[arg(long)]
    arch: Option<String>,
    /// For a tool: the env-runtime package id (`tapp:<hex>`) it was built on.
    #[arg(long)]
    runtime: Option<String>,
    /// A file listing the distribution packages the image holds, one
    /// `name-epoch:version-release.arch` per line.
    #[arg(long)]
    packages: Option<PathBuf>,
    /// Publisher identity JSON; a fresh key is generated when absent.
    #[arg(long)]
    identity: Option<PathBuf>,
    /// Output .tapp path.
    #[arg(short = 'o', long)]
    output: PathBuf,
    /// Write the thin form (metadata only).
    #[arg(long)]
    thin: bool,
}

#[derive(clap::Args)]
struct BotEntryArgs {
    /// Publisher identity JSON that signs this entry.
    #[arg(long)]
    identity: PathBuf,
    /// Reverse-DNS id of the bot, such as `so.tilo.bots.discord-bridge`.
    #[arg(long)]
    package_id: String,
    /// Version of this release.
    #[arg(long)]
    version: String,
    /// Registry that serves the program and receives `--upload`.
    #[arg(long, default_value = "https://apps.tilo.so")]
    registry: String,
    /// A connector the program's triggers use, as `<package id>@<min version>`.
    #[arg(long = "connector")]
    connectors: Vec<String>,
    /// JSON presentation metadata, as for `entry`.
    #[arg(long)]
    metadata: Option<PathBuf>,
    /// Wrap the entry in the signed mutation shape accepted by POST /entries.
    #[arg(long)]
    action: Option<RegistryAction>,
    /// Current signed registry entry JSON; required for updates.
    #[arg(long, requires = "action")]
    previous_entry: Option<PathBuf>,
    /// Upload the program to `--registry` before printing.
    #[arg(long)]
    upload: bool,
    /// The program file: a Shortcut as JSON, in any formatting.
    file: PathBuf,
}

#[derive(clap::Args)]
struct EntryArgs {
    /// Public URL the .tapp will be downloadable from.
    #[arg(long)]
    url: String,
    /// Publisher identity JSON that signed the tapp and signs this entry.
    #[arg(long)]
    identity: PathBuf,
    /// Platform family of the tapp: a native Tilo OS build, or a desktop
    /// or Android build made by `pack-desktop` or `tilo-app package`.
    #[arg(long, value_enum, default_value_t)]
    platform: TargetPlatform,
    /// Target architecture: `x86_64` or `aarch64` (inferred when the
    /// package manifest declares exactly one).
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
    /// Newest releases kept per target; older ones leave the entry.
    #[arg(long, default_value_t = registry_types::retention::DEFAULT_RETAINED_PER_CHANNEL)]
    keep_releases: usize,
    /// Newest pre-releases kept per target; older ones leave the entry.
    #[arg(long, default_value_t = registry_types::retention::DEFAULT_RETAINED_PER_CHANNEL)]
    keep_prereleases: usize,
    /// Owner-verified catalogue used to reconcile never-published native
    /// history after four newer releases with offered exact runtimes remain.
    #[arg(long, requires = "registry_owner_pubkey")]
    runtime_catalogue: Option<PathBuf>,
    /// Existing registry owner pin used with --runtime-catalogue.
    #[arg(long, requires = "runtime_catalogue")]
    registry_owner_pubkey: Option<String>,
    /// Write a machine-readable exact-runtime refusal for a publisher's
    /// bounded signed-catalogue readiness wait. No file is written for any
    /// other verification or packaging failure.
    #[arg(long, requires = "runtime_catalogue")]
    missing_runtime_output: Option<PathBuf>,
    /// Payload objects of a thin .tapp, as `split` wrote them; they are
    /// hosted beside the .tapp at `--url`.
    #[arg(long)]
    objects: Option<PathBuf>,
    /// The .tapp to describe.
    file: PathBuf,
}

#[derive(clap::Args)]
struct PendingArgs {
    /// Registry origin, e.g. `https://apps.tilo.so`.
    #[arg(long)]
    registry: String,
    /// Publisher identity JSON that signs the time-bound read.
    #[arg(long)]
    identity: PathBuf,
    /// Package id, e.g. `so.tilo.notes`.
    package_id: String,
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
        Command::PackDesktop {
            manifest,
            executable,
            platform,
            arch,
            isa,
            output,
            identity,
            dry_run,
        } => cmd_pack_desktop(&PackDesktopArgs {
            manifest: &manifest,
            executable: &executable,
            platform,
            isa: isa.as_deref().unwrap_or(&arch),
            arch: &arch,
            output: &output,
            identity: identity.as_deref(),
            dry_run,
        }),
        Command::SealImage(args) => cmd_seal_image(&args),
        Command::Thin { output, file } => cmd_thin(&file, &output),
        Command::Split { out, file } => cmd_split(&file, &out),
        Command::Compress { output, file } => cmd_compress(&file, output.as_deref()),
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
        Command::Attest {
            publisher_identity,
            attestor_identity,
            attestor,
            scope,
            file,
            output,
        } => attest::run(
            &publisher_identity,
            &attestor_identity,
            &attestor,
            &scope,
            &file,
            &output,
        ),
        Command::Keygen { out } => cmd_keygen(&out),
        Command::Entry(args) => cmd_entry(&args),
        Command::BotEntry(args) => cmd_bot_entry(&args),
        Command::Pending(args) => cmd_pending(&args),
    }
}

/// Fill `[package.keywords]` from the app's `search/*.ftl`, which are the
/// only place the words are written; a manifest that also declares some by
/// hand is refused rather than merged.
fn add_search_keywords(manifest: &mut Manifest, manifest_path: &Path) -> Result<()> {
    let app_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let keywords = tilo_app_package::search_keywords::read(app_dir).map_err(|e| anyhow!(e))?;
    if !keywords.is_empty() && !manifest.package.keywords.is_empty() {
        bail!(
            "{} declares [package.keywords] and has search/*.ftl; keep the words in search/*.ftl only",
            manifest_path.display()
        );
    }
    if !keywords.is_empty() {
        manifest.package.keywords = keywords;
    }
    Ok(())
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
    add_search_keywords(&mut manifest, manifest_path)?;

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

fn cmd_seal_image(args: &SealImageArgs) -> Result<()> {
    use tilo_app_package::manifest::Arch;
    let arch = match args.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
        "x86_64" => Arch::X86_64,
        "aarch64" => Arch::AArch64,
        other => bail!("--arch {other:?}: images are built for x86_64 or aarch64"),
    };
    let runtime = args
        .runtime
        .as_deref()
        .map(|raw| {
            let text = if raw.starts_with("tapp:") {
                raw.to_owned()
            } else {
                format!("tapp:{raw}")
            };
            text.parse::<tilo_app_package::PackageId>()
                .map_err(|error| anyhow!("--runtime {raw:?}: {error}"))
        })
        .transpose()?;
    let packages = match &args.packages {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        None => Vec::new(),
    };
    let spec = tilo_app_package::ImageSpec {
        kind: match args.kind {
            ImageKind::EnvRuntime => tilo_app_package::Kind::EnvRuntime,
            ImageKind::Tool => tilo_app_package::Kind::Tool,
        },
        id: args.id.clone(),
        version: args.version.clone(),
        name: args.name.clone(),
        arch,
        runtime,
        packages,
    };
    let identity = match &args.identity {
        Some(path) => load_identity(path)?,
        None => SigningIdentity::generate(),
    };
    let packed = tilo_app_package::seal_image(&args.tree, &spec, &identity)
        .map_err(|error| anyhow!("seal {}: {error}", args.tree.display()))?;
    let payload: Option<&dyn BlobSource> = if args.thin { None } else { Some(&packed) };
    write_tapp(&args.output, &packed.meta, payload)?;
    eprintln!(
        "sealed {} {} {} ({} files, {} distinct blobs)",
        spec.kind,
        spec.id,
        spec.version,
        packed.meta.collection.entries.len(),
        packed.meta.payload_hashes().len()
    );
    println!("{}", packed.id());
    Ok(())
}

/// The desktop systems `pack-desktop` builds for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DesktopPlatform {
    Linux,
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
    dry_run: bool,
}

fn cmd_pack_desktop(args: &PackDesktopArgs<'_>) -> Result<()> {
    let manifest_src = std::fs::read_to_string(args.manifest)
        .with_context(|| format!("read manifest {}", args.manifest.display()))?;
    let mut authored = Manifest::from_toml_str(&manifest_src).context("decode manifest TOML")?;
    add_search_keywords(&mut authored, args.manifest)?;
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
        DesktopPlatform::Linux => registry_types::LINUX_PLATFORM,
        DesktopPlatform::Windows => registry_types::WINDOWS_PLATFORM,
        DesktopPlatform::Macos => registry_types::MACOS_PLATFORM,
    };
    let mut manifest = tilo_app_package::desktop::desktop_manifest(
        &authored, platform, args.arch, args.isa, &module,
    )?;

    let executable_bytes = std::fs::metadata(args.executable)?.len();
    let limit = tilo_app_package::app_size_limit(&manifest)
        .ok_or_else(|| anyhow!("desktop package has no application size contract"))?;
    if executable_bytes > limit {
        bail!("executable is {executable_bytes} bytes; desktop payload limit is {limit} bytes");
    }
    if args.dry_run {
        if args.identity.is_some() {
            bail!("dry-run cannot select a signing identity");
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "signed": false,
                "publisher_selected": false,
                "archive_size_verified": false,
                "executable": args.executable,
                "executable_bytes": executable_bytes,
                "maximum_archive_bytes": limit,
                "output": args.output,
                "manifest_toml": manifest.to_toml_string()?,
            }))?
        );
        return Ok(());
    }
    let identity = match args.identity {
        Some(path) => load_identity(path)?,
        None => SigningIdentity::generate(),
    };
    manifest.publisher.identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));

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

fn cmd_thin(file: &Path, output: &Path) -> Result<()> {
    let (tapp, _) = read_tapp(file, false)?;
    write_tapp(output, &tapp.meta, None)?;
    eprintln!("wrote thin {} -> {}", tapp.meta.id, output.display());
    Ok(())
}

fn cmd_compress(file: &Path, output: Option<&Path>) -> Result<()> {
    use tilo_app_package::compressed;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    tilo_app_package::tapp::read(bytes.as_slice(), &mut tilo_app_package::DiscardBlobs)
        .with_context(|| format!("{} is not a .tapp", file.display()))?;
    let twin = compressed::compress(&bytes).context("compress")?;
    let mut round_trip = Vec::with_capacity(bytes.len());
    compressed::decompress_bounded(twin.as_slice(), bytes.len() as u64, &mut round_trip)
        .context("decompress the twin")?;
    if round_trip != bytes {
        bail!("the twin of {} does not decompress to it", file.display());
    }
    let output = output.map_or_else(
        || {
            let mut name = file.as_os_str().to_owned();
            name.push(compressed::SUFFIX);
            PathBuf::from(name)
        },
        Path::to_path_buf,
    );
    tilo_files::native::atomic::write(&output, &twin)
        .with_context(|| format!("write {}", output.display()))?;
    eprintln!(
        "compressed {} -> {}: {} -> {} bytes",
        file.display(),
        output.display(),
        bytes.len(),
        twin.len()
    );
    Ok(())
}

fn cmd_split(file: &Path, out: &Path) -> Result<()> {
    let name = file
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", file.display()))?;
    std::fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;
    let thin_path = out.join(name);
    if thin_path.exists() && std::fs::canonicalize(&thin_path)? == std::fs::canonicalize(file)? {
        bail!("--out must not be the directory holding {}", file.display());
    }
    let reader = std::io::BufReader::new(
        File::open(file).with_context(|| format!("open {}", file.display()))?,
    );
    let split = tilo_app_package::chunks::split(reader, out)
        .with_context(|| format!("split {}", file.display()))?;
    write_tapp(&thin_path, &split.meta, None)?;
    eprintln!(
        "split {} -> {}: {} whole blob(s), {} chunked blob(s) in {} chunk(s), {} payload bytes",
        split.meta.id,
        out.display(),
        split.whole_blobs,
        split.chunked_blobs,
        split.chunks,
        split.payload_bytes,
    );
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
    // An image's manifest carries what it may be stacked on; one that
    // breaks the schema (a tool naming no runtime) does not verify.
    if tapp.meta.manifest.package.kind.is_image()
        && let Err(error) = tilo_app_package::validate_manifest(&tapp.meta.manifest)
    {
        println!("FAILED: {id}: {error}");
        std::process::exit(1);
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

    if registry.starts_with("http://") || registry.starts_with("https://") {
        bail!(
            "an HTTP registry takes uploads and reviewed entries; publish to it with `tilo-app publish --registry {registry}`"
        );
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

fn cmd_pending(args: &PendingArgs) -> Result<()> {
    let package_id = args.package_id.as_str();
    let signer = load_signing_key(&args.identity)?;
    let client = registry_client::RegistryClient::new(&args.registry)?;
    let Some(entry) = client.pending(&signer, package_id)? else {
        return Ok(());
    };
    if !registry_types::verify_entry_signature(&entry) {
        bail!("pending entry for {package_id} has an invalid publisher signature");
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&entry).context("encode pending entry")?
    );
    Ok(())
}

fn reconcile_entry_history(
    args: &EntryArgs,
    entry: &mut registry_types::RegistryEntry,
    signer: &SigningKey,
) -> Result<()> {
    if let Some(catalogue_path) = &args.runtime_catalogue {
        let bytes = std::fs::read(catalogue_path).context("read runtime catalogue")?;
        let catalogue: registry_types::SignedRegistrySnapshot =
            serde_json::from_slice(&bytes).context("decode runtime catalogue")?;
        let owner = args
            .registry_owner_pubkey
            .as_deref()
            .context("runtime catalogue requires an existing registry owner pin")?;
        let now = tilo_clock::try_unix_now().context("clock precedes Unix epoch")?;
        registry_types::verify_snapshot(&catalogue, owner, now)
            .context("verify runtime catalogue against registry owner")?;
        let (tapp, _) = read_tapp(&args.file, false)?;
        require_optional_runtime_offers(entry, &args.url, &tapp.meta.manifest, &catalogue.entries)?;
        let dropped = registry_client::runtime_history::reconcile(
            entry,
            &catalogue.entries,
            &args.url,
            signer,
        )?;
        for version in dropped {
            eprintln!(
                "reconciled unpublished {}@{} ({}) after four newer offered releases; exact runtime {} is no longer offered",
                entry.package_id,
                version.version,
                version
                    .target()
                    .map_or("", |target| target.architecture.as_str()),
                version
                    .target()
                    .and_then(|target| target.runtime.as_deref())
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// A newly published app must not become visible before its exact engine.
/// The caller verifies the owner-signed snapshot and the app before this check.
fn require_optional_runtime_offers(
    entry: &registry_types::RegistryEntry,
    current_url: &str,
    manifest: &Manifest,
    catalogue: &[registry_types::RegistryEntry],
) -> Result<()> {
    let current = entry
        .versions
        .iter()
        .find(|version| {
            version
                .tapp()
                .is_some_and(|tapp| tapp.tapp_url == current_url)
        })
        .context("current package is missing from its registry entry")?;
    let target = current
        .target()
        .context("current package has no native target")?;
    for runtime in tilo_app_package::desktop::optional_runtime_dependencies(manifest) {
        let offered = catalogue.iter().any(|provider| {
            registry_types::is_runtime_package_id(&provider.package_id)
                && provider
                    .versions
                    .iter()
                    .filter_map(registry_types::RegistryVersion::target)
                    .any(|provided| {
                        provided.platform == target.platform
                            && provided.architecture == target.architecture
                            && provided.runtime.as_deref() == Some(runtime.as_str())
                    })
        });
        if !offered {
            return Err(registry_client::runtime_history::MissingRuntime {
                package_id: entry.package_id.clone(),
                version: current.version.clone(),
                architecture: target.architecture.clone(),
                runtime: runtime.clone(),
            }
            .into());
        }
    }
    Ok(())
}

fn record_missing_runtime(path: Option<&Path>, error: &anyhow::Error) -> Result<()> {
    if let (Some(path), Some(missing)) = (
        path,
        error.downcast_ref::<registry_client::runtime_history::MissingRuntime>(),
    ) {
        let record = serde_json::json!({
            "package_id": missing.package_id,
            "version": missing.version,
            "architecture": missing.architecture,
            "runtime": missing.runtime,
        });
        std::fs::write(path, serde_json::to_vec(&record)?)
            .with_context(|| format!("write exact-runtime refusal to {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod runtime_readiness_tests {
    use super::{record_missing_runtime, require_optional_runtime_offers};

    #[test]
    fn publication_waits_for_the_exact_engine_and_native_architecture() {
        use registry_types::{
            RegistryEntry, RegistryTarget, RegistryVersion, TappArtifact, VersionArtifact,
        };

        let manifest: super::Manifest =
            super::Manifest::from_toml_str(include_str!("../../../../apps/browser/tilo-app.toml"))
                .unwrap();
        let runtime = &manifest.package.optional_runtimes[0];
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let url = "https://apps.tilo.so/artifacts/browser.tapp";
        let mut app = RegistryEntry {
            package_id: manifest.package.id.to_string(),
            kind: registry_types::EntryKind::App,
            name: "Browser".into(),
            description: "Browse the web".into(),
            tagline: "Web browser".into(),
            images: registry_types::LocalizedImages::default(),
            supported_languages: vec!["en".into()],
            categories: vec!["Network".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(signer.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: 1_790_000_000,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: url.into(),
                    tapp_root_hash_hex: "aa".repeat(32),
                    tapp_size_bytes: 12_288,
                    tapp_blake3_hex: "bb".repeat(32),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: manifest.package.runtime.clone(),
                        interfaces: vec![],
                    },
                }),
            }],
            updated_at_unix: 1_790_000_001,
            signature_hex: String::new(),
        };
        registry_types::sign_entry(&mut app, &signer);
        let missing = require_optional_runtime_offers(&app, url, &manifest, &[]).unwrap_err();
        assert_eq!(
            missing
                .downcast_ref::<registry_client::runtime_history::MissingRuntime>()
                .unwrap()
                .runtime,
            *runtime
        );
        let mut provider = app.clone();
        provider.package_id = "so.tilo.runtime.chromium".into();
        for (provided_runtime, architecture, accepted) in [
            ("tilo-runtime/older-engine", "x86_64", false),
            (runtime.as_str(), "aarch64", false),
            (runtime.as_str(), "x86_64", true),
        ] {
            let VersionArtifact::Tapp(artifact) = &mut provider.versions[0].artifact else {
                panic!("native package")
            };
            artifact.target.runtime = Some(provided_runtime.into());
            artifact.target.architecture = architecture.into();
            registry_types::sign_entry(&mut provider, &signer);
            assert_eq!(
                require_optional_runtime_offers(&app, url, &manifest, &[provider.clone()]).is_ok(),
                accepted
            );
        }
        assert!(registry_types::verify_entry_signature(&app));
    }

    #[test]
    fn writes_only_typed_exact_runtime_refusals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.json");
        let missing = registry_client::runtime_history::MissingRuntime {
            package_id: "so.tilo.agent".into(),
            version: "1.0.8".into(),
            architecture: "aarch64".into(),
            runtime: "tilo-runtime/exact".into(),
        };
        record_missing_runtime(Some(&path), &missing.into()).unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["runtime"], "tilo-runtime/exact");
        assert_eq!(record["architecture"], "aarch64");

        std::fs::remove_file(&path).unwrap();
        record_missing_runtime(Some(&path), &anyhow::anyhow!("invalid signature")).unwrap();
        assert!(!path.exists());
    }
}

fn load_previous_entry(path: &Path) -> Result<registry_types::RegistryEntry> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read previous entry {}", path.display()))?;
    let entry: registry_types::RegistryEntry =
        serde_json::from_slice(&bytes).context("decode previous registry entry")?;
    if !registry_types::verify_entry_signature(&entry) {
        bail!("previous registry entry has an invalid publisher signature");
    }
    Ok(entry)
}

fn load_entry_metadata(path: &Path) -> Result<registry_client::EntryMetadata> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read entry metadata {}", path.display()))?;
    serde_json::from_slice(&bytes).context("decode entry metadata JSON")
}

fn print_entry(
    entry: &registry_types::RegistryEntry,
    action: Option<RegistryAction>,
    signer: &SigningKey,
) -> Result<()> {
    let output = action.map_or_else(
        || serde_json::to_value(entry),
        |action| {
            serde_json::to_value(registry_types::sign_change_request(
                action.into(),
                entry.clone(),
                signer,
            ))
        },
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("encode entry")?
    );
    Ok(())
}

fn release_time() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(tilo_clock::unix_now)
}

/// Hand-written program files are accepted in any formatting; what gets
/// hashed, uploaded and bound is always the canonical encoding.
fn cmd_bot_entry(args: &BotEntryArgs) -> Result<()> {
    let file = &args.file;
    let raw = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let shortcut: tilo_automation::Shortcut =
        serde_json::from_slice(&raw).with_context(|| format!("decode {}", file.display()))?;
    let canonical = tilo_automation::program::canonical_bytes(&shortcut)?;
    let connectors = args
        .connectors
        .iter()
        .map(|raw| {
            let (package_id, min_version) = raw.split_once('@').ok_or_else(|| {
                anyhow::anyhow!("--connector takes <package id>@<min version>, got {raw}")
            })?;
            Ok(registry_types::PackageRequirement {
                package_id: package_id.to_owned(),
                min_version: min_version.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let signer = load_signing_key(&args.identity)?;
    if matches!(args.action, Some(RegistryAction::Update)) && args.previous_entry.is_none() {
        bail!("--previous-entry is required with --action update");
    }
    let previous = args
        .previous_entry
        .as_deref()
        .map(load_previous_entry)
        .transpose()?;
    let metadata = args
        .metadata
        .as_deref()
        .map(load_entry_metadata)
        .transpose()?;
    let release = registry_client::BotRelease {
        program: &canonical,
        package_id: &args.package_id,
        version: &args.version,
        registry_origin: &args.registry,
        connectors: &connectors,
    };
    let entry = registry_client::build_bot_entry(
        &release,
        &signer,
        previous.as_ref(),
        metadata.as_ref(),
        release_time(),
    )?;
    if args.upload {
        registry_client::RegistryClient::new(&args.registry)?
            .upload_program(&signer, &canonical)?;
    }
    print_entry(&entry, args.action, &signer)?;
    let url = entry
        .versions
        .iter()
        .find(|v| v.version == args.version)
        .and_then(registry_types::RegistryVersion::bot)
        .map_or("?", |bot| bot.program_url.as_str());
    eprintln!("entry: {}@{} -> {url}", entry.package_id, args.version);
    Ok(())
}

fn cmd_entry(args: &EntryArgs) -> Result<()> {
    let file = &args.file;
    let url = args.url.as_str();
    let identity = &args.identity;
    let platform = args.platform;
    let arch = args.arch.as_deref();
    let metadata = args.metadata.as_deref();
    let action = args.action;
    let previous_entry = args.previous_entry.as_deref();
    let retention = registry_types::retention::RetentionPolicy {
        releases: args.keep_releases,
        prereleases: args.keep_prereleases,
    };
    let tapp_bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let signer = load_signing_key(identity)?;
    if matches!(action, Some(RegistryAction::Update)) && previous_entry.is_none() {
        bail!("--previous-entry is required with --action update");
    }
    let previous = previous_entry.map(load_previous_entry).transpose()?;
    let metadata = metadata.map(load_entry_metadata).transpose()?;
    let mut entry = if let Some(objects) = &args.objects {
        registry_client::build_entry_with_objects(
            registry_client::ThinPackage {
                tapp: &tapp_bytes,
                objects: &tilo_app_package::chunks::ObjectDir::new(objects),
            },
            url,
            &signer,
            platform.into(),
            arch,
            previous.as_ref(),
            metadata.as_ref(),
            &retention,
        )?
    } else {
        registry_client::build_entry(
            &tapp_bytes,
            url,
            &signer,
            platform.into(),
            arch,
            previous.as_ref(),
            metadata.as_ref(),
            &retention,
        )?
    };
    if let Err(error) = reconcile_entry_history(args, &mut entry, &signer) {
        record_missing_runtime(args.missing_runtime_output.as_deref(), &error)?;
        return Err(error);
    }
    print_entry(&entry, action, &signer)?;
    // `build_entry` always builds exactly one version; `.first()`
    // avoids indexing so a future refactor there can't turn this into a panic.
    let version = entry.versions.first().map_or("?", |v| v.version.as_str());
    eprintln!("entry: {}@{version} -> {url}", entry.package_id);
    Ok(())
}

#[cfg(test)]
mod desktop_plan_tests {
    use super::*;

    #[test]
    fn desktop_dry_run_refuses_identity_selection() {
        assert!(
            Cli::try_parse_from([
                "tilo-app-package",
                "pack-desktop",
                "--manifest",
                "app.toml",
                "--executable",
                "app",
                "--platform",
                "linux",
                "--arch",
                "x86_64",
                "--output",
                "app.tapp",
                "--dry-run",
                "--identity",
                "private.json",
            ])
            .is_err()
        );
    }

    #[test]
    fn desktop_dry_run_validates_without_writing_or_signing() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("app.toml");
        std::fs::write(
            &manifest,
            format!(
                r#"
[package]
id = "so.tilo.plan"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2026.07"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
"#,
                "00".repeat(32)
            ),
        )
        .unwrap();
        let executable = dir.path().join("app");
        std::fs::write(&executable, b"not executed").unwrap();
        let output = dir.path().join("missing/app.tapp");
        let mut args = PackDesktopArgs {
            manifest: &manifest,
            executable: &executable,
            platform: DesktopPlatform::Linux,
            arch: "x86_64",
            isa: "x86-64-v3",
            output: &output,
            identity: None,
            dry_run: true,
        };
        cmd_pack_desktop(&args).unwrap();
        assert!(!output.parent().unwrap().exists());
        args.arch = "wasm32";
        assert!(cmd_pack_desktop(&args).is_err());
        assert!(!output.exists());
    }
}
