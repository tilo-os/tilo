//! `qos-runtime-bundle` — CLI over the [`runtime_bundle`] crate.
//!
//! ```text
//! qos-runtime-bundle build   --version <ver> --out <dir> [--from <host-path>]
//! qos-runtime-bundle verify  <dir>
//! qos-runtime-bundle path    --version <ver>            # print resolved default
//! ```
//!
//! The default `path` lookup order matches the launcher:
//! `$XDG_DATA_HOME/qos-runtime/<v>/` if it exists, otherwise
//! `/usr/lib/qos-runtime/<v>/`, otherwise print the per-user path
//! (which is where `qos-install runtime install` would write to).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use runtime_bundle::{
    build_bundle, default_bundle_path, resolve_bundle_path, system_bundle_path, verify_bundle,
};

#[derive(Debug, Parser)]
#[command(
    name = "qos-runtime-bundle",
    about = "Produce and verify qOS runtime bundles."
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
        /// the launcher to match `runtime = "qos-runtime/<version>"`
        /// in package manifests.
        #[arg(long)]
        version: String,
        /// Output directory. Created if missing.
        #[arg(long)]
        out: PathBuf,
        /// Host root to resolve source paths against. Defaults to `/`.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Verify an existing bundle by recomputing hashes.
    Verify {
        /// Path to the bundle root (contains `bundle.toml`).
        dir: PathBuf,
    },
    /// Print the resolved bundle path for a given version.
    Path {
        /// Runtime version tag.
        #[arg(long)]
        version: String,
        /// Print the system fallback (`/usr/lib/qos-runtime/<v>/`)
        /// even if no bundle is installed there.
        #[arg(long)]
        system: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build { version, out, from } => {
            match build_bundle(&out, &version, from.as_deref()) {
                Ok(manifest) => {
                    println!("built bundle at {}", out.display());
                    println!("  version  : {}", manifest.version);
                    println!("  files    : {}", manifest.files.len());
                    println!("  root-hash: blake3:{}", manifest.root_hash);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("qos-runtime-bundle build: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Cmd::Verify { dir } => match verify_bundle(&dir) {
            Ok(manifest) => {
                println!("bundle {} verified", dir.display());
                println!("  version  : {}", manifest.version);
                println!("  files    : {}", manifest.files.len());
                println!("  root-hash: blake3:{}", manifest.root_hash);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("qos-runtime-bundle verify: {e}");
                ExitCode::from(1)
            }
        },
        Cmd::Path { version, system } => {
            if system {
                println!("{}", system_bundle_path(&version).display());
                return ExitCode::SUCCESS;
            }
            if let Some(p) = resolve_bundle_path(&version) {
                println!("{}", p.display());
                return ExitCode::SUCCESS;
            }
            // Nothing installed. Print the per-user default so callers
            // know where an install *would* go.
            match default_bundle_path(&version) {
                Ok(p) => {
                    println!("{}", p.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("qos-runtime-bundle path: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
