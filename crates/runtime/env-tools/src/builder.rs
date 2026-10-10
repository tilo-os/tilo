//! The tool builder: request → key → existing image, or a fresh build.
//!
//! 1. **Resolve** the request against the runtime (`dnf --assumeno` in a
//!    throwaway sandbox) and form the [`CacheKey`].
//! 2. **Look up** the key: the local index, then the catalog. A hit is the
//!    answer.
//! 3. **Build** on a miss: install exactly the resolved NEVRAs in a build
//!    sandbox, check the rpm database agrees, seal the upper, and hand the
//!    tree to the store, which packs it into a `tool` image bound to the
//!    runtime, signs and keeps it.
//!
//! Nothing here needs a session or a display; a relay's compute worker
//! drives the same [`Builder`] as a device does.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tilo_app_package::PackageId;

use crate::index::{Entry, LocalIndex, ToolLookup};
use crate::key::CacheKey;
use crate::network::{Network, NetworkChoice};
use crate::resolve::{self, Resolution, ResolveError};
use crate::sandbox::{BuildSandbox, SandboxError};
use crate::seal::{self, SealError, SealReport};

/// dnf options every resolve and build uses; part of [`crate::key::RECIPE`].
pub const DNF_OPTIONS: &[&str] = &[
    "--setopt=install_weak_deps=False",
    "--setopt=tsflags=nodocs",
    "--setopt=keepcache=False",
];

/// Where images live: the runtime trees to build on, and the tool images
/// built. Implemented over the package store; a fake in tests.
pub trait ToolStore {
    /// The materialised, read-only root of an env-runtime image.
    ///
    /// # Errors
    ///
    /// The runtime can't be fetched, verified or materialised.
    fn runtime_tree(&self, runtime: &PackageId) -> anyhow::Result<PathBuf>;

    /// Whether `tool` is in the store (so an index entry naming it is good).
    fn has(&self, tool: &PackageId) -> bool;

    /// Pack `tree` into a `tool` image bound to `key.runtime`, sign it and
    /// keep it. `requested` is what the user asked for.
    ///
    /// # Errors
    ///
    /// Packing, signing or storing fails.
    fn store_tool(
        &self,
        tree: &Path,
        key: &CacheKey,
        requested: &[String],
    ) -> anyhow::Result<PackageId>;
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("no packages requested")]
    Empty,
    #[error("{0:?} {verb} already part of the runtime", verb = if .0.len() == 1 { "is" } else { "are" })]
    AlreadyInRuntime(Vec<String>),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("dnf failed (exit {code:?}):\n{output}")]
    Dnf { code: Option<i32>, output: String },
    #[error("the build installed {installed:?}, not the resolved {resolved:?}")]
    Drifted {
        resolved: Vec<String>,
        installed: Vec<String>,
    },
    #[error("{0}")]
    Store(#[source] anyhow::Error),
    #[error("build state {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Where a tool id came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// This builder's index named an image the store has.
    Local,
    /// The published catalog.
    Catalog,
    /// Built just now.
    Built { report: SealReport, took: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub tool: PackageId,
    pub key: CacheKey,
    pub origin: Origin,
}

/// Deadlines for the two dnf runs.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub resolve: Duration,
    pub install: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            resolve: Duration::from_secs(10 * 60),
            install: Duration::from_secs(60 * 60),
        }
    }
}

/// A tree built and sealed, not yet stored.
#[derive(Debug)]
pub struct SealedTree {
    pub nevras: Vec<String>,
    pub report: SealReport,
    /// The package manager's output.
    pub log: String,
}

/// Resolves, caches and builds tool images.
pub struct Builder<S, C> {
    store: S,
    catalog: C,
    state: PathBuf,
    index: LocalIndex,
    timeouts: Timeouts,
    sandbox: SandboxOptions,
    network: NetworkChoice,
    /// Each runtime's egress policy, worked out once.
    policies: Mutex<HashMap<PackageId, tilo_egress::EgressPolicy>>,
}

impl<S: ToolStore, C: ToolLookup> Builder<S, C> {
    /// A builder keeping its index, dnf caches and scratch under `state`
    /// (which must be on a filesystem overlayfs accepts as an upper).
    pub fn new(store: S, catalog: C, state: impl Into<PathBuf>) -> Self {
        let state = state.into();
        Self {
            index: LocalIndex::at(state.join("index")),
            store,
            catalog,
            state,
            timeouts: Timeouts::default(),
            sandbox: SandboxOptions::default(),
            network: NetworkChoice::default(),
            policies: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_bwrap(mut self, bwrap: PathBuf) -> Self {
        self.sandbox.bwrap = Some(bwrap);
        self
    }

    /// What the builds may reach: their runtime's repositories (the
    /// default), nothing, or the caller's whole network.
    #[must_use]
    pub fn with_network(mut self, network: NetworkChoice) -> Self {
        self.network = network;
        self
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn index(&self) -> &LocalIndex {
        &self.index
    }

    /// The tool image for `packages` on `runtime`: an existing one when the
    /// resolved key has been built before, else a new build.
    ///
    /// # Errors
    ///
    /// [`BuildError`] for an empty or unresolvable request, a request the
    /// runtime already satisfies, and any build or store failure.
    pub fn build(&self, runtime: &PackageId, packages: &[String]) -> Result<Built, BuildError> {
        self.build_reporting(runtime, packages, &mut std::io::sink())
    }

    /// [`Self::build`], telling `progress` what it is doing and passing on
    /// the package manager's output.
    ///
    /// # Errors
    ///
    /// As [`Self::build`].
    pub fn build_reporting(
        &self,
        runtime: &PackageId,
        packages: &[String],
        progress: &mut dyn Write,
    ) -> Result<Built, BuildError> {
        let _ = writeln!(progress, "resolving {}", packages.join(" "));
        let tree = self
            .store
            .runtime_tree(runtime)
            .map_err(BuildError::Store)?;
        let (key, _) = self.resolve(runtime, &tree, packages)?;
        let digest = key.digest();
        if let Some(entry) = self.index.get(&digest)
            && self.store.has(&entry.tool)
        {
            tracing::info!(tool = %entry.tool, %digest, "tool cache hit");
            let _ = writeln!(progress, "already built: {}", key.nevras.join(" "));
            return Ok(Built {
                tool: entry.tool,
                key,
                origin: Origin::Local,
            });
        }
        if let Some(tool) = self.catalog.lookup(&digest) {
            tracing::info!(%tool, %digest, "tool catalog hit");
            let _ = writeln!(progress, "in the tool catalog: {}", key.nevras.join(" "));
            self.remember(&key, tool, packages);
            return Ok(Built {
                tool,
                key,
                origin: Origin::Catalog,
            });
        }

        let started = Instant::now();
        let _ = writeln!(progress, "installing {}", key.nevras.join(" "));
        let scratch = self.scratch("build")?;
        let out = scratch.join("tool");
        let sealed = self.build_tree(runtime, &tree, &key.nevras, &scratch.join("sandbox"), &out);
        let result = sealed.and_then(|sealed| {
            let _ = progress.write_all(sealed.log.as_bytes());
            let _ = writeln!(progress, "sealing {} files", sealed.report.files);
            let key = CacheKey::new(*runtime, &sealed.nevras);
            let tool = self
                .store
                .store_tool(&out, &key, packages)
                .map_err(BuildError::Store)?;
            Ok((key, tool, sealed.report))
        });
        let _ = crate::sandbox::remove_tree(&scratch);
        let (key, tool, report) = result?;
        self.remember(&key, tool, packages);
        let elapsed = started.elapsed();
        tracing::info!(%tool, digest = %key.digest(), files = report.files, bytes = report.bytes, ?elapsed, "tool built");
        Ok(Built {
            tool,
            key,
            origin: Origin::Built {
                report,
                took: elapsed,
            },
        })
    }

    /// Resolve `packages` against the runtime tree, returning the key.
    ///
    /// # Errors
    ///
    /// As [`resolve_in`].
    pub fn resolve(
        &self,
        runtime: &PackageId,
        tree: &Path,
        packages: &[String],
    ) -> Result<(CacheKey, Resolution), BuildError> {
        let scratch = self.scratch("resolve")?;
        let result = self.options(runtime, tree).and_then(|options| {
            resolve_in(tree, &scratch, &options, packages, self.timeouts.resolve)
        });
        let _ = crate::sandbox::remove_tree(&scratch);
        let resolution = result?;
        Ok((CacheKey::new(*runtime, &resolution.install), resolution))
    }

    /// Install exactly `nevras` on `runtime` (materialised at `tree`) and
    /// seal the result into `out`. The returned NEVRAs are what the rpm
    /// database says was installed.
    ///
    /// # Errors
    ///
    /// As [`build_tree`].
    pub fn build_tree(
        &self,
        runtime: &PackageId,
        tree: &Path,
        nevras: &[String],
        scratch: &Path,
        out: &Path,
    ) -> Result<SealedTree, BuildError> {
        let options = self.options(runtime, tree)?;
        build_tree(tree, nevras, scratch, out, &options, self.timeouts.install)
    }

    fn remember(&self, key: &CacheKey, tool: PackageId, requested: &[String]) {
        let entry = Entry {
            tool,
            key: key.clone(),
            requested: requested.to_vec(),
        };
        if let Err(e) = self.index.put(&entry) {
            tracing::warn!(error = %e, "could not record the tool in the local index");
        }
    }

    /// The sandbox options for a build on `runtime` (at `tree`): its own
    /// metadata cache, and the network the builder's choice gives it.
    fn options(&self, runtime: &PackageId, tree: &Path) -> Result<SandboxOptions, BuildError> {
        let network = match self.network {
            NetworkChoice::Offline => Network::Offline,
            NetworkChoice::Host => Network::Host,
            NetworkChoice::Gated => crate::network::gated(self.policy(runtime, tree)?),
        };
        Ok(SandboxOptions {
            dnf_cache: Some(self.state.join("dnf-cache").join(runtime.to_hex())),
            network,
            ..self.sandbox.clone()
        })
    }

    fn policy(
        &self,
        runtime: &PackageId,
        tree: &Path,
    ) -> Result<tilo_egress::EgressPolicy, BuildError> {
        if let Some(policy) = self
            .policies
            .lock()
            .ok()
            .and_then(|p| p.get(runtime).cloned())
        {
            return Ok(policy);
        }
        let scratch = self.scratch("mirrors")?;
        let policy = crate::network::repo_policy(tree, &scratch);
        let _ = crate::sandbox::remove_tree(&scratch);
        let policy = policy?;
        if let Ok(mut policies) = self.policies.lock() {
            policies.insert(*runtime, policy.clone());
        }
        Ok(policy)
    }

    fn scratch(&self, what: &str) -> Result<PathBuf, BuildError> {
        let base = self.state.join("scratch");
        std::fs::create_dir_all(&base).map_err(|source| BuildError::Io {
            path: base.clone(),
            source,
        })?;
        for n in 0u32.. {
            let dir = base.join(format!("{what}-{}-{n}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(BuildError::Io { path: dir, source }),
            }
        }
        Err(BuildError::Io {
            path: base,
            source: std::io::Error::other("no free scratch name"),
        })
    }
}

/// How a build's sandbox is set up beyond its runtime and scratch.
#[derive(Clone, Debug)]
pub struct SandboxOptions {
    /// dnf's metadata cache, kept across builds on one runtime.
    pub dnf_cache: Option<PathBuf>,
    /// A bubblewrap other than `bwrap` on `PATH`.
    pub bwrap: Option<PathBuf>,
    /// What the sandbox may reach.
    pub network: Network,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            dnf_cache: None,
            bwrap: None,
            network: Network::Offline,
        }
    }
}

fn sandbox(
    tree: &Path,
    scratch: &Path,
    options: &SandboxOptions,
) -> Result<BuildSandbox, SandboxError> {
    let mut sandbox = BuildSandbox::create(tree, scratch)?;
    if let Some(cache) = &options.dnf_cache {
        sandbox = sandbox.with_dnf_cache(cache.clone());
    }
    if let Some(bwrap) = &options.bwrap {
        sandbox = sandbox.with_bwrap(bwrap.clone());
    }
    Ok(sandbox.with_network(options.network.clone()))
}

fn dnf_argv<'a>(head: &[&'a str], packages: &'a [String]) -> Vec<&'a str> {
    let mut argv: Vec<&str> = vec!["dnf"];
    argv.extend(DNF_OPTIONS);
    argv.extend(head);
    argv.push("--");
    argv.extend(packages.iter().map(String::as_str));
    argv
}

/// Resolve `packages` on the runtime `tree` in a sandbox under `scratch`.
///
/// # Errors
///
/// [`BuildError::Empty`], [`BuildError::AlreadyInRuntime`] when nothing new
/// would be installed, [`BuildError::Resolve`] for dnf's verdicts, or a
/// sandbox failure.
pub fn resolve_in(
    tree: &Path,
    scratch: &Path,
    options: &SandboxOptions,
    packages: &[String],
    timeout: Duration,
) -> Result<Resolution, BuildError> {
    if packages.is_empty() {
        return Err(BuildError::Empty);
    }
    let sandbox = sandbox(tree, scratch, options)?;
    let ran = sandbox.run(&dnf_argv(&["--assumeno", "install"], packages), timeout)?;
    let output = format!("{}\n{}", ran.stdout, ran.stderr);
    let resolution = resolve::parse_assumeno(&output)?;
    if resolution.install.is_empty() {
        if resolution.already_installed.is_empty() {
            return Err(BuildError::Dnf {
                code: ran.code,
                output,
            });
        }
        return Err(BuildError::AlreadyInRuntime(resolution.already_installed));
    }
    Ok(resolution)
}

/// Install exactly `nevras` on the runtime `tree` in a sandbox under
/// `scratch`, then seal the upper into `out`.
///
/// # Errors
///
/// [`BuildError::Dnf`] when the transaction fails, [`BuildError::Drifted`]
/// when the rpm database disagrees with `nevras`, or a sandbox or seal
/// failure.
pub fn build_tree(
    tree: &Path,
    nevras: &[String],
    scratch: &Path,
    out: &Path,
    options: &SandboxOptions,
    timeout: Duration,
) -> Result<SealedTree, BuildError> {
    if nevras.is_empty() {
        return Err(BuildError::Empty);
    }
    let sandbox = sandbox(tree, scratch, options)?;
    let before = installed(&sandbox)?;
    let ran = sandbox.run(&dnf_argv(&["-y", "install"], nevras), timeout)?;
    if !ran.success {
        return Err(BuildError::Dnf {
            code: ran.code,
            output: format!("{}\n{}", ran.stdout, ran.stderr),
        });
    }
    let after = installed(&sandbox)?;
    let added: Vec<String> = after
        .iter()
        .filter(|n| before.binary_search(n).is_err())
        .cloned()
        .collect();
    let mut resolved: Vec<String> = nevras
        .iter()
        .map(|n| crate::key::normalize_nevra(n))
        .collect();
    resolved.sort();
    resolved.dedup();
    let removed = before.iter().any(|n| after.binary_search(n).is_err());
    if removed || added != resolved {
        return Err(BuildError::Drifted {
            resolved,
            installed: added,
        });
    }
    let report = seal::seal(&sandbox.upper(), tree, out)?;
    let _ = sandbox.discard();
    Ok(SealedTree {
        nevras: added,
        report,
        log: ran.stdout,
    })
}

fn installed(sandbox: &BuildSandbox) -> Result<Vec<String>, BuildError> {
    let ran = sandbox.run_offline(
        &["rpm", "-qa", "--qf", resolve::RPM_QUERY_FORMAT],
        Duration::from_secs(120),
    )?;
    if !ran.success {
        return Err(BuildError::Dnf {
            code: ran.code,
            output: ran.stderr,
        });
    }
    Ok(resolve::parse_rpm_list(&ran.stdout))
}
