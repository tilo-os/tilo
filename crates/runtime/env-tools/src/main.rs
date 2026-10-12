//! `tilo-env-tools` — build tool images without a session.
//!
//! CI uses it to publish the tool catalog for each env-runtime; a relay's
//! compute worker uses it to build a member's tool where their session runs.
//! Output is one JSON document on stdout; progress and logs go to stderr.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use env_tools::network::NetworkChoice;
use env_tools::{Builder, CacheKey, Catalog, Entry, NoCatalog, Origin, ToolStore, image, store};
use package_store::{Admission, ImageTrust, PackageStore, Store};
use tilo_app_package::{PackageId, SigningIdentity};

#[derive(Parser)]
#[command(name = "tilo-env-tools", about = "Build tool images for environments")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct RuntimeArgs {
    /// The env-runtime image as a `.tapp`; it is checked and materialised
    /// under `--state`.
    #[arg(long, conflicts_with_all = ["runtime", "runtime_tree"])]
    runtime_tapp: Option<PathBuf>,
    /// The env-runtime image id (`tapp:<hex>`), with `--runtime-tree`.
    #[arg(long, requires = "runtime_tree")]
    runtime: Option<PackageId>,
    /// The runtime's materialised tree, with `--runtime`.
    #[arg(long, requires = "runtime")]
    runtime_tree: Option<PathBuf>,
    /// Index, dnf metadata cache, scratch and materialised runtimes. Must be
    /// on a filesystem overlayfs accepts as an upper (ordinary disk).
    #[arg(long)]
    state: PathBuf,
    /// What builds may reach: the runtime's repositories (`gated`, the
    /// default), nothing (`offline`), or this machine's whole network
    /// (`host`, for a CI builder without `/dev/net/tun`).
    #[arg(long, value_enum, default_value_t = NetworkArg::Gated)]
    network: NetworkArg,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum NetworkArg {
    Gated,
    Offline,
    Host,
}

impl From<NetworkArg> for NetworkChoice {
    fn from(arg: NetworkArg) -> Self {
        match arg {
            NetworkArg::Gated => Self::Gated,
            NetworkArg::Offline => Self::Offline,
            NetworkArg::Host => Self::Host,
        }
    }
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
    /// Build the tool image for packages and write it as a `.tapp`.
    Build {
        #[command(flatten)]
        runtime: RuntimeArgs,
        /// Publisher identity file (`{"secret_key_hex": …}`); a throwaway
        /// key when absent.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Where the `.tapp` goes.
        #[arg(long)]
        out_dir: PathBuf,
        /// Add the tool to this catalog document (created if missing).
        #[arg(long)]
        catalog: Option<PathBuf>,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Build (or find) a tool with this device's builder, key and store —
    /// what `pkg install` does, without a session.
    DeviceBuild {
        #[arg(long)]
        runtime: PackageId,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Build a tool for `publisher`, whose key is elsewhere: the image is
    /// written to `--out` with a placeholder signature for the key holder
    /// to sign. What a relay's worker runs as a member, in their quota.
    BuildFor {
        /// The key holder's public key (hex).
        #[arg(long)]
        publisher: String,
        #[arg(long)]
        runtime: PackageId,
        #[arg(long)]
        out: PathBuf,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Merge catalog documents into one.
    MergeCatalogs {
        #[arg(long)]
        out: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
}

#[derive(serde::Serialize)]
struct Resolved {
    key: CacheKey,
    digest: String,
    already_installed: Vec<String>,
}

#[derive(serde::Serialize)]
struct BuiltTool {
    tool: PackageId,
    key: CacheKey,
    digest: String,
    requested: Vec<String>,
    origin: &'static str,
    tapp: Option<PathBuf>,
    tapp_bytes: Option<u64>,
    files: Option<u64>,
    bytes: Option<u64>,
    build_seconds: Option<f64>,
    shadows: Vec<String>,
    whiteouts: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match Cli::parse().command {
        Command::Resolve { runtime, packages } => {
            let (id, tree) = runtime.materialise()?;
            let builder = Builder::new(
                DirStore::new(id, tree.clone(), None, None)?,
                NoCatalog,
                &runtime.state,
            )
            .with_network(runtime.network.into());
            let (key, resolution) = builder.resolve(&id, &tree, &packages)?;
            print(&Resolved {
                digest: key.digest().to_hex(),
                key,
                already_installed: resolution.already_installed,
            })
        }
        Command::Build {
            runtime,
            identity,
            out_dir,
            catalog,
            packages,
        } => {
            let (id, tree) = runtime.materialise()?;
            let identity = match identity {
                Some(path) => env_tools::signing::identity_file(&path)?,
                None => SigningIdentity::generate(),
            };
            std::fs::create_dir_all(&out_dir)?;
            let builder = Builder::new(
                DirStore::new(id, tree, Some(identity), Some(out_dir))?,
                NoCatalog,
                &runtime.state,
            )
            .with_network(runtime.network.into());
            let built = builder.build_reporting(&id, &packages, &mut std::io::stderr())?;
            let tapp = builder
                .store()
                .written
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .clone();
            if let Some(path) = catalog {
                add_to_catalog(&path, &built, &packages)?;
            }
            print(&report(built, packages, tapp))
        }
        Command::DeviceBuild { runtime, packages } => {
            let builder = store::device_builder()?;
            let built = builder.build_reporting(&runtime, &packages, &mut std::io::stderr())?;
            print(&report(built, packages, None))
        }
        Command::BuildFor {
            publisher,
            runtime,
            out,
            packages,
        } => {
            let publisher: [u8; 32] = hex::decode(&publisher)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .context("--publisher is not 32 hex bytes")?;
            let state = store::default_state_dir()
                .ok_or_else(|| anyhow::anyhow!("no cache directory for the tool builder"))?;
            let builder = Builder::new(
                env_tools::delegated::UnsignedStore::new(Store::from_env()?, publisher, out)?,
                NoCatalog,
                state,
            );
            let built = builder.build_reporting(&runtime, &packages, &mut std::io::stderr())?;
            print(&serde_json::json!({
                "unsigned": built.tool.to_string(),
                "key": built.key,
            }))
        }
        Command::MergeCatalogs { out, inputs } => {
            let mut merged = Catalog::default();
            for input in &inputs {
                merged.merge(Catalog::load(input).with_context(|| input.display().to_string())?);
            }
            merged.save(&out)?;
            print(&serde_json::json!({ "tools": merged.tools.len() }))
        }
    }
}

/// Add `built` to the catalog document at `path`, creating it if missing.
fn add_to_catalog(
    path: &Path,
    built: &env_tools::Built,
    requested: &[String],
) -> anyhow::Result<()> {
    let mut doc = if path.exists() {
        Catalog::load(path)?
    } else {
        Catalog::default()
    };
    doc.insert(Entry {
        tool: built.tool,
        key: built.key.clone(),
        requested: requested.to_vec(),
    });
    doc.save(path)
}

fn report(built: env_tools::Built, requested: Vec<String>, tapp: Option<PathBuf>) -> BuiltTool {
    let tapp_bytes = tapp
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len());
    let (origin, files, bytes, build_seconds, shadows, whiteouts) = match built.origin {
        Origin::Local => ("local", None, None, None, Vec::new(), Vec::new()),
        Origin::Catalog => ("catalog", None, None, None, Vec::new(), Vec::new()),
        Origin::Built { report, took } => (
            "built",
            Some(report.files),
            Some(report.bytes),
            Some(took.as_secs_f64()),
            report.shadows,
            report.whiteouts,
        ),
    };
    BuiltTool {
        tool: built.tool,
        digest: built.key.digest().to_hex(),
        key: built.key,
        requested,
        origin,
        tapp,
        tapp_bytes,
        files,
        bytes,
        build_seconds,
        shadows,
        whiteouts,
    }
}

impl RuntimeArgs {
    /// The runtime's id and read-only tree.
    fn materialise(&self) -> anyhow::Result<(PackageId, PathBuf)> {
        if let (Some(id), Some(tree)) = (self.runtime, &self.runtime_tree) {
            return Ok((id, tree.clone()));
        }
        let tapp = self.runtime_tapp.as_deref().ok_or_else(|| {
            anyhow::anyhow!("name the runtime: --runtime-tapp, or --runtime with --runtime-tree")
        })?;
        let user = PackageStore::at(self.state.join("runtimes"));
        // The caller vouches for the file (CI checks the published image's
        // signature and checksums first); the package's own signature is
        // still verified on the way in.
        let meta = user.install_image(
            std::io::BufReader::new(std::fs::File::open(tapp)?),
            |_| {
                Ok(Admission {
                    pinned: true,
                    attested_by: String::new(),
                    source: tapp.display().to_string(),
                    held: false,
                })
            },
            None,
        )?;
        anyhow::ensure!(
            meta.manifest.package.kind == tilo_app_package::Kind::EnvRuntime,
            "{} is a {} image, not an env-runtime",
            tapp.display(),
            meta.manifest.package.kind
        );
        let store = Store::new(user).with_trust(ImageTrust::new().with_publisher(meta.publisher()));
        let tree = package_store::materialized_tree(&store, &meta.id)?;
        Ok((meta.id, tree))
    }
}

/// A store over one runtime that writes each tool it is given to a
/// directory as a `.tapp`, for publishing.
struct DirStore {
    runtime: PackageId,
    tree: PathBuf,
    identity: Option<SigningIdentity>,
    out_dir: Option<PathBuf>,
    arch: tilo_app_package::manifest::Arch,
    written: Mutex<Option<PathBuf>>,
}

impl DirStore {
    fn new(
        runtime: PackageId,
        tree: PathBuf,
        identity: Option<SigningIdentity>,
        out_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            runtime,
            tree,
            identity,
            out_dir,
            arch: image::host_arch()?,
            written: Mutex::new(None),
        })
    }
}

impl ToolStore for DirStore {
    fn runtime_tree(&self, runtime: &PackageId) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(*runtime == self.runtime, "no runtime {runtime} here");
        Ok(self.tree.clone())
    }

    fn has(&self, _: &PackageId) -> bool {
        // Every build is written out afresh.
        false
    }

    fn store_tool(
        &self,
        tree: &Path,
        key: &CacheKey,
        requested: &[String],
    ) -> anyhow::Result<PackageId> {
        let (Some(identity), Some(out_dir)) = (&self.identity, &self.out_dir) else {
            anyhow::bail!("this command only resolves");
        };
        let packed = image::seal_tool(tree, key, requested, self.arch, identity)?;
        let name = image::tool_id(requested);
        let digest = key.digest().to_hex();
        let path = out_dir.join(format!(
            "{name}-{}.tapp",
            digest.get(..16).unwrap_or(&digest)
        ));
        image::write_tapp(&path, &packed)?;
        *self
            .written
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned"))? = Some(path);
        Ok(packed.id())
    }
}

fn print(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
