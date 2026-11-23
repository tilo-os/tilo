//! `tilo-runtime-bundle` — CLI over the [`runtime_bundle`] crate; `--help` lists the commands.
//!
//! The default `path` lookup order matches the launcher: `$XDG_DATA_HOME/tilo-runtime/<v>/` if it
//! exists, then `/usr/lib/tilo-runtime/<v>/`, then the per-user path, which is where
//! `tilo-install runtime install` writes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use runtime_bundle::{
    BuildOptions, Identity, VerifyOptions, build_abi_bundle, build_bundle_with,
    default_bundle_path, resolve_bundle_path, system_bundle_path, verify_bundle_recorded_with,
    verify_bundle_with,
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
        /// Build a bundle of the `--include-bin` binaries, and any `--include`, without the host
        /// loader, libc and `ld.so.cache`: a runtime apps name as their entrypoint runtime (the
        /// tilolang runtime). The launcher mounts it beside the app's own runtime, whose loader
        /// and libraries its binaries run on, so a loader set of its own would never be read; a
        /// Mac runtime brings the libraries its binaries load relative to themselves as a
        /// directory `--include`, and names each architecture of its universal binaries in
        /// `--arch` (`aarch64+x86_64`).
        #[arg(long, requires = "include_bin", conflicts_with_all = ["release", "from"])]
        binaries_only: bool,
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
        /// Leave the record of this verification where a launch reads it, as an install does:
        /// the first app started on the bundle then reads no payload byte this proved.
        #[arg(long)]
        record: bool,
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
            binaries_only,
        } => {
            if binaries_only {
                let (Some(version), Some(out)) = (version, out) else {
                    eprintln!(
                        "tilo-runtime-bundle build: --binaries-only takes --version and --out"
                    );
                    return ExitCode::from(2);
                };
                let built = runtime_bundle::build_binaries_bundle(
                    &out,
                    &version,
                    arch.as_deref(),
                    identity.as_deref(),
                    &include_bin,
                    &include,
                );
                return cmd_build(
                    built.map(|manifest| (manifest, out)),
                    version_out.as_deref(),
                );
            }
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
            record,
        } => cmd_verify(
            &dir,
            &VerifyOptions {
                require_publisher,
                require_signature,
                require_arch,
            },
            record,
        ),
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
            print_manifest(&manifest);
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

/// What a built or verified bundle is.
fn print_manifest(manifest: &runtime_bundle::BundleManifest) {
    println!("  version  : {}", manifest.version);
    println!("  arch     : {}", manifest.arch);
    println!("  files    : {}", manifest.files.len());
    println!("  root-hash: blake3:{}", manifest.root_hash);
    if let Some(pk) = &manifest.publisher_hex {
        println!("  publisher: ed25519:{pk}");
    }
}

fn cmd_verify(dir: &Path, opts: &VerifyOptions, record: bool) -> ExitCode {
    let verified = if record {
        let records = runtime_bundle::record::directory();
        verify_bundle_recorded_with(dir, opts, records.as_deref()).map(|(manifest, _)| manifest)
    } else {
        verify_bundle_with(dir, opts)
    };
    match verified {
        Ok(manifest) => {
            println!("bundle {} verified", dir.display());
            print_manifest(&manifest);
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
    if let Err(e) = tilo_files::native::atomic::write_creating_parent_with_mode(
        out,
        &identity.to_json_bytes(),
        0o600,
    ) {
        eprintln!("tilo-runtime-bundle key generate: {e}");
        return ExitCode::from(1);
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure, with_observer};
    use tilo_test_env::file_mode::{on_a_thread_without_chmod, with_zero_umask};

    #[test]
    fn key_generate_is_private_under_zero_umask_and_keeps_old_key_on_failed_replace() {
        tilo_test_env::in_own_process(
            module_path!(),
            "key_generate_is_private_under_zero_umask_and_keeps_old_key_on_failed_replace",
            || {
                with_zero_umask(|| {
                    let dir = tempfile::tempdir().unwrap();
                    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                    let path = dir.path().join("publisher.json");

                    // With umask 0 and chmod/fchmod neutralized, neither the writer nor the
                    // atomic backend's post-open fchmod can narrow the inode: the stage can
                    // only be 0600 if openat created it with 0600.
                    let observed = Arc::new(Mutex::new(None));
                    let seen = Arc::clone(&observed);
                    let existed_at_stage = Arc::new(AtomicBool::new(false));
                    let flag = Arc::clone(&existed_at_stage);
                    let target = path.clone();
                    let code = on_a_thread_without_chmod(|| {
                        with_observer(
                            move |event| {
                                if event.checkpoint == Checkpoint::StageCreated {
                                    let mode =
                                        event.stage.as_ref().unwrap().metadata.permissions().mode()
                                            & 0o777;
                                    *seen.lock().unwrap() = Some(mode);
                                    flag.store(target.exists(), Ordering::SeqCst);
                                }
                            },
                            || key_generate(&path),
                        )
                    });
                    assert_eq!(code, ExitCode::SUCCESS);
                    assert_eq!(*observed.lock().unwrap(), Some(0o600));
                    assert!(!existed_at_stage.load(Ordering::SeqCst));
                    assert_eq!(
                        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    assert_eq!(
                        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                        0o755
                    );
                    Identity::load(&path).unwrap();
                    let before = std::fs::read(&path).unwrap();

                    let failed = with_failure(Checkpoint::BeforePublish, || key_generate(&path));
                    assert_ne!(failed, ExitCode::SUCCESS);
                    assert_eq!(std::fs::read(&path).unwrap(), before);
                    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

                    // Missing parent directories are created without tightening the shared parent.
                    let nested = dir.path().join("nested/publisher.json");
                    assert_eq!(
                        on_a_thread_without_chmod(|| key_generate(&nested)),
                        ExitCode::SUCCESS
                    );
                    assert_eq!(
                        std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    assert_eq!(
                        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                        0o755
                    );
                });
            },
        );
    }
}
