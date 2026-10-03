//! `tilo-env-tools` — build tool images without a session.
//!
//! CI uses it to publish the tool catalog for each env-runtime; a relay's
//! compute worker uses it to build a member's tool where their session runs.
//! Output is one JSON document on stdout; logs go to stderr.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use env_tools::builder::{self, Timeouts};
use env_tools::{CacheKey, seal::SealReport};
use tilo_app_package::PackageId;

#[derive(Parser)]
#[command(name = "tilo-env-tools", about = "Build tool images for environments")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct RuntimeArgs {
    /// The env-runtime image id (`tapp:<hex>`) the tool is for.
    #[arg(long)]
    runtime: PackageId,
    /// Its materialised tree.
    #[arg(long)]
    runtime_tree: PathBuf,
    /// Index, dnf metadata cache and scratch. Must be on a filesystem
    /// overlayfs accepts as an upper (ordinary disk, not tmpfs).
    #[arg(long)]
    state: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve packages against the runtime and print the cache key.
    Resolve {
        #[command(flatten)]
        runtime: RuntimeArgs,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Resolve, install in a build sandbox and seal the result into `--out`.
    BuildTree {
        #[command(flatten)]
        runtime: RuntimeArgs,
        /// Where the sealed tree goes; must not exist.
        #[arg(long)]
        out: PathBuf,
        #[arg(required = true)]
        packages: Vec<String>,
    },
}

#[derive(serde::Serialize)]
struct Resolved {
    key: CacheKey,
    digest: String,
    already_installed: Vec<String>,
}

#[derive(serde::Serialize)]
struct TreeBuilt {
    key: CacheKey,
    digest: String,
    requested: Vec<String>,
    files: u64,
    symlinks: u64,
    bytes: u64,
    seconds: f64,
    dropped: Vec<String>,
    whiteouts: Vec<String>,
    shadows: Vec<String>,
    skipped: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let timeouts = Timeouts::default();
    match cli.command {
        Command::Resolve { runtime, packages } => {
            let (key, resolution) = resolve(&runtime, &packages, timeouts)?;
            print(&Resolved {
                digest: key.digest().to_hex(),
                key,
                already_installed: resolution.already_installed,
            })
        }
        Command::BuildTree {
            runtime,
            out,
            packages,
        } => {
            let started = std::time::Instant::now();
            let (key, _) = resolve(&runtime, &packages, timeouts)?;
            let scratch = scratch_dir(&runtime.state, "build")?;
            let built = builder::build_tree(
                &runtime.runtime_tree,
                &key.nevras,
                &scratch,
                &out,
                Some(dnf_cache(&runtime)),
                None,
                timeouts.install,
            );
            let _ = env_tools::sandbox::remove_tree(&scratch);
            let built = built?;
            let key = CacheKey::new(runtime.runtime, &built.nevras);
            let SealReport {
                files,
                symlinks,
                bytes,
                dropped,
                whiteouts,
                shadows,
                skipped,
            } = built.report;
            print(&TreeBuilt {
                digest: key.digest().to_hex(),
                key,
                requested: packages,
                files,
                symlinks,
                bytes,
                seconds: started.elapsed().as_secs_f64(),
                dropped,
                whiteouts,
                shadows,
                skipped,
            })
        }
    }
}

fn resolve(
    runtime: &RuntimeArgs,
    packages: &[String],
    timeouts: Timeouts,
) -> anyhow::Result<(CacheKey, env_tools::resolve::Resolution)> {
    let scratch = scratch_dir(&runtime.state, "resolve")?;
    let resolution = builder::resolve_in(
        &runtime.runtime_tree,
        &scratch,
        Some(dnf_cache(runtime)),
        None,
        packages,
        timeouts.resolve,
    );
    let _ = env_tools::sandbox::remove_tree(&scratch);
    let resolution = resolution?;
    Ok((
        CacheKey::new(runtime.runtime, &resolution.install),
        resolution,
    ))
}

fn dnf_cache(runtime: &RuntimeArgs) -> PathBuf {
    runtime
        .state
        .join("dnf-cache")
        .join(runtime.runtime.to_hex())
}

fn scratch_dir(state: &Path, what: &str) -> anyhow::Result<PathBuf> {
    let dir = state
        .join("scratch")
        .join(format!("{what}-{}", std::process::id()));
    let _ = env_tools::sandbox::remove_tree(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn print(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
