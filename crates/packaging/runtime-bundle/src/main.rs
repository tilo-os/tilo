//! `tilo-runtime-bundle` — CLI over the [`runtime_bundle`] crate.
//!
//! ```text
//! tilo-runtime-bundle build   --version <ver> --out <dir> [--from <host-path>] \
//!                            [--arch <arch>] [--identity <path>]
//! tilo-runtime-bundle build   --release <rel> --out-root <dir> [--version-out <file>] ...
//!                            # names the bundle <rel>+native.<abi digest>
//! tilo-runtime-bundle verify  <dir> [--require-publisher <hex>]
//!                                  [--require-signature]
//!                                  [--require-arch <arch>]
//! tilo-runtime-bundle path    --version <ver>            # print resolved default
//! tilo-runtime-bundle key generate --out <path>          # write a fresh identity
//! ```
//!
//! The default `path` lookup order matches the launcher:
//! `$XDG_DATA_HOME/tilo-runtime/<v>/` if it exists, otherwise
//! `/usr/lib/tilo-runtime/<v>/`, otherwise print the per-user path
//! (which is where `tilo-install runtime install` would write to).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use runtime_bundle::{
    BuildOptions, Identity, VerifyOptions, build_abi_bundle, build_bundle_with,
    default_bundle_path, resolve_bundle_path, system_bundle_path, verify_bundle_with,
};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-runtime-bundle",
    about = "Produce and verify Tilo runtime bundles."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Copy the minimal loader + libc set from the host into a
    /// content-addressed bundle directory.
    Build {
        /// Bundle version tag. Written into `bundle.toml` and used by
        /// the launcher to match `runtime = "tilo-runtime/<version>"`
        /// in package manifests.
        #[arg(long, required_unless_present = "release", conflicts_with = "release")]
        version: Option<String>,
        /// Output directory. Created if missing.
        #[arg(long, required_unless_present = "release", conflicts_with = "release")]
        out: Option<PathBuf>,
        /// Build the runtime that thin apps link: the bundle's version
        /// becomes `<release>+native.<abi digest>`, the digest of its
        /// payload, so an app built against it names exactly this build.
        #[arg(long, requires = "out_root")]
        release: Option<String>,
        /// With --release: the directory the bundle is created in, under
        /// its version.
        #[arg(long, requires = "release")]
        out_root: Option<PathBuf>,
        /// Write the built bundle's version to this file.
        #[arg(long)]
        version_out: Option<PathBuf>,
        /// Host root to resolve source paths against. Defaults to `/`.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Target architecture. Defaults to the host arch
        /// (`std::env::consts::ARCH`). Explicit override lets you
        /// build a cross-arch bundle from a fabricated rootfs (e.g.
        /// an aarch64 sysroot on an `x86_64` CI runner).
        #[arg(long)]
        arch: Option<String>,
        /// Path to an identity JSON file for signing the bundle.
        /// Format: `{ "secret_key_hex": "<64-hex>" }`. When set, the
        /// finished `bundle.toml` gains `publisher_hex` +
        /// `signature_hex`. Same shape as `tilo-app-package`'s identity files.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Extra shared object to place at the top of the bundle's
        /// `lib/` (repeatable). Use it to fold the Rust
        /// `libstd-<hash>.so` and the Tilo `libruntime_dylib.so` into
        /// the bundle so thin (`-C prefer-dynamic`) apps resolve them
        /// via the launcher's `LD_LIBRARY_PATH=<mount>/lib`. Each is
        /// hashed + signed like every other bundle file. A directory keeps
        /// its tree under `lib/<name>/`, as the browser engine does.
        #[arg(long = "include", value_name = "PATH")]
        include: Vec<PathBuf>,
        /// Native service executable included in the signed runtime payload.
        #[arg(long = "include-bin", value_name = "PATH")]
        include_bin: Vec<PathBuf>,
    },
    /// Verify an existing bundle by recomputing hashes (and, when
    /// signed, checking the signature).
    Verify {
        /// Path to the bundle root (contains `bundle.toml`).
        dir: PathBuf,
        /// If set, refuse the bundle unless it is signed by this
        /// exact publisher (Ed25519 public key, hex-encoded).
        #[arg(long)]
        require_publisher: Option<String>,
        /// If set, refuse an unsigned bundle.
        #[arg(long)]
        require_signature: bool,
        /// If set, refuse a bundle whose declared `arch` differs.
        /// Defaults to no arch enforcement.
        #[arg(long)]
        require_arch: Option<String>,
    },
    /// Print the resolved bundle path for a given version.
    Path {
        /// Runtime version tag.
        #[arg(long)]
        version: String,
        /// Print the system fallback (`/usr/lib/tilo-runtime/<v>/`)
        /// even if no bundle is installed there.
        #[arg(long)]
        system: bool,
    },
    /// Signing-key utilities.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCmd {
    /// Write a fresh signing identity JSON file to `--out`.
    Generate {
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the public key of an identity JSON file (hex).
    Pubkey {
        #[arg(long)]
        identity: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build {
            version,
            out,
            release,
            out_root,
            version_out,
            from,
            arch,
            identity,
            include,
            include_bin,
        } => {
            let opts = BuildOptions {
                from: from.as_deref(),
                arch: arch.as_deref(),
                identity: identity.as_deref(),
                extra_libs: &include,
                extra_bins: &include_bin,
            };
            let built = match (release, out_root, version, out) {
                (Some(release), Some(root), _, _) => build_abi_bundle(&root, &release, &opts),
                (_, _, Some(version), Some(out)) => {
                    build_bundle_with(&out, &version, &opts).map(|manifest| (manifest, out))
                }
                _ => {
                    eprintln!(
                        "tilo-runtime-bundle build: pass --version and --out, or --release and --out-root"
                    );
                    return ExitCode::from(2);
                }
            };
            cmd_build(built, version_out.as_deref())
        }
        Cmd::Verify {
            dir,
            require_publisher,
            require_signature,
            require_arch,
        } => cmd_verify(&dir, require_publisher, require_signature, require_arch),
        Cmd::Path { version, system } => cmd_path(&version, system),
        Cmd::Key { cmd } => cmd_key(cmd),
    }
}

fn cmd_build(
    built: Result<(runtime_bundle::BundleManifest, PathBuf), runtime_bundle::BundleError>,
    version_out: Option<&Path>,
) -> ExitCode {
    match built {
        Ok((manifest, out)) => {
            if let Some(path) = version_out
                && let Err(e) = std::fs::write(path, format!("{}\n", manifest.version))
            {
                eprintln!("tilo-runtime-bundle build: writing {}: {e}", path.display());
                return ExitCode::from(1);
            }
            println!("built bundle at {}", out.display());
            println!("  version  : {}", manifest.version);
            println!("  arch     : {}", manifest.arch);
            println!("  files    : {}", manifest.files.len());
            println!("  root-hash: blake3:{}", manifest.root_hash);
            if let Some(pk) = &manifest.publisher_hex {
                println!("  publisher: ed25519:{pk}");
            }
            if manifest.signature_hex.is_some() {
                println!("  signed   : yes");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tilo-runtime-bundle build: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_verify(
    dir: &Path,
    require_publisher: Option<String>,
    require_signature: bool,
    require_arch: Option<String>,
) -> ExitCode {
    match verify_bundle_with(
        dir,
        &VerifyOptions {
            require_publisher,
            require_signature,
            require_arch,
        },
    ) {
        Ok(manifest) => {
            println!("bundle {} verified", dir.display());
            println!("  version  : {}", manifest.version);
            println!("  arch     : {}", manifest.arch);
            println!("  files    : {}", manifest.files.len());
            println!("  root-hash: blake3:{}", manifest.root_hash);
            if let Some(pk) = &manifest.publisher_hex {
                println!("  publisher: ed25519:{pk}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tilo-runtime-bundle verify: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_path(version: &str, system: bool) -> ExitCode {
    if system {
        println!("{}", system_bundle_path(version).display());
        return ExitCode::SUCCESS;
    }
    if let Some(p) = resolve_bundle_path(version) {
        println!("{}", p.display());
        return ExitCode::SUCCESS;
    }
    match default_bundle_path(version) {
        Ok(p) => {
            println!("{}", p.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tilo-runtime-bundle path: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_key(cmd: KeyCmd) -> ExitCode {
    match cmd {
        KeyCmd::Generate { out } => key_generate(&out),
        KeyCmd::Pubkey { identity } => key_pubkey(&identity),
    }
}

fn key_generate(out: &Path) -> ExitCode {
    let identity = Identity::generate();
    if let Some(parent) = out.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("tilo-runtime-bundle key generate: {e}");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::write(out, identity.to_json_bytes()) {
        eprintln!("tilo-runtime-bundle key generate: {e}");
        return ExitCode::from(1);
    }
    // Best-effort restrict permissions on the secret file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600));
    }
    println!("wrote identity to {}", out.display());
    println!("public-key: ed25519:{}", identity.public_key_hex());
    ExitCode::SUCCESS
}

fn key_pubkey(identity: &Path) -> ExitCode {
    match Identity::load(identity) {
        Ok(id) => {
            println!("{}", id.public_key_hex());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tilo-runtime-bundle key pubkey: {e}");
            ExitCode::from(1)
        }
    }
}
