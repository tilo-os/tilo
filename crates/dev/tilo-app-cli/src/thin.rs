//! Thin packaging: build an app exactly as the release pipeline does, and bake
//! the runtime bundle it links against.
//!
//! A thin app is compiled with `-C prefer-dynamic` against `runtime-dylib`,
//! the shared UI/runtime SDK dylib. Rust dylib symbols carry crate hashes, so a
//! thin app only loads against a `libruntime_dylib.so` built in the same Cargo
//! graph with the same toolchain. That is why nothing here drives Cargo
//! directly: the build goes through `os/scripts/build-thin-apps.sh` (the same
//! script the image and `dist/apps/package-apps.sh` run, with the same
//! `thin-app` profile, `<app>/thin` feature and
//! `tools/ci/rustc-thin-app-wrapper.sh`), resolved against the default app
//! collection from `tools/apps/app_release.py`, and the runtime is baked with
//! `os/scripts/bake-runtime-bundle.sh`.
//!
//! The one deliberate difference from a release: the runtime this produces is
//! not the published `tilo-runtime/<version>` the manifest names, so the
//! packaged manifest names it by content instead ([`local_runtime_version`]).
//! Installing it can then never shadow the system runtime the other installed
//! apps were linked against.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};

/// Where thin builds are driven from, relative to the Tilo source root.
pub(crate) const THIN_BUILD_SCRIPT: &str = "os/scripts/build-thin-apps.sh";
/// The rustc wrapper that adds `-C prefer-dynamic` to the app and runtime.
pub(crate) const THIN_WRAPPER: &str = "tools/ci/rustc-thin-app-wrapper.sh";
/// Bakes and signs a runtime bundle.
pub(crate) const BAKE_RUNTIME_SCRIPT: &str = "os/scripts/bake-runtime-bundle.sh";
/// Names the default collection's packages and features.
pub(crate) const APP_RELEASE_PLANNER: &str = "tools/apps/app_release.py";
/// The Cargo profile release thin apps are built with.
pub(crate) const THIN_PROFILE: &str = "thin-app";

/// The Tilo source tree containing `app_dir` as `apps/<name>`, the only place
/// the thin build script looks for apps.
///
/// # Errors
///
/// Returns an error naming `--fat` when `app_dir` is not inside a Tilo source
/// tree, or not directly under its `apps/`.
pub(crate) fn tilo_source_root(app_dir: &Path) -> Result<PathBuf> {
    let app_dir = app_dir
        .canonicalize()
        .with_context(|| format!("resolving {}", app_dir.display()))?;
    let root = app_dir
        .ancestors()
        .skip(1)
        .find(|dir| dir.join(THIN_BUILD_SCRIPT).is_file() && dir.join(THIN_WRAPPER).is_file());
    let Some(root) = root else {
        bail!(
            "{} is not inside a Tilo source tree ({THIN_BUILD_SCRIPT} not found above it).\n\
             A thin package links the shared runtime-dylib, which has to be built in the same \
             Cargo graph as the app, so it is built from the Tilo tree's apps/ directory. \
             Move the app to apps/<name> in a Tilo checkout, or pass --fat for a self-contained \
             binary (usually over the 5 MiB app limit).",
            app_dir.display()
        );
    };
    if app_dir.parent() != Some(root.join("apps").as_path()) {
        bail!(
            "{} is inside the Tilo tree at {} but not at apps/<name>, where {THIN_BUILD_SCRIPT} \
             finds apps; move it there, or pass --fat",
            app_dir.display(),
            root.display()
        );
    }
    Ok(root.to_path_buf())
}

/// The Cargo graph the shared runtime is resolved in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeGraph {
    /// Every package resolved with the runtime.
    pub packages: Vec<String>,
    /// Package-qualified features enabled across that graph.
    pub features: Vec<String>,
}

impl RuntimeGraph {
    /// Parse `app_release.py --collection <name>` output.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan is not JSON or lacks its package or
    /// feature lists.
    pub(crate) fn from_plan(plan: &str) -> Result<Self> {
        let plan: serde_json::Value =
            serde_json::from_str(plan).context("parsing the app collection plan")?;
        let strings = |key: &str| -> Result<Vec<String>> {
            plan.get(key)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("app collection plan has no {key:?} list"))?
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        anyhow::anyhow!("app collection plan {key:?} holds a non-string")
                    })
                })
                .collect()
        };
        Ok(Self {
            packages: strings("packages")?,
            features: strings("features")?,
        })
    }

    /// The default app collection, the graph `dist/apps/package-apps.sh`
    /// resolves the released runtime in.
    ///
    /// # Errors
    ///
    /// Returns an error when `python3` or the planner fails.
    pub(crate) fn default_collection(root: &Path) -> Result<Self> {
        let output = ProcessCommand::new("python3")
            .arg(root.join(APP_RELEASE_PLANNER))
            .args(["--collection", "default"])
            .current_dir(root)
            .output()
            .context("starting python3 to plan the default app collection")?;
        if !output.status.success() {
            bail!(
                "{APP_RELEASE_PLANNER} --collection default failed with {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Self::from_plan(&String::from_utf8_lossy(&output.stdout))
    }
}

/// One run of the thin build script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThinBuildPlan {
    /// The Tilo source root, also the working directory.
    pub root: PathBuf,
    /// The Cargo profile, which is also the output directory under `target/`.
    pub profile: String,
    /// Cargo's target directory, when it is not `<root>/target`.
    pub target_dir: Option<PathBuf>,
    /// Environment the script reads its selection from.
    pub env: Vec<(String, String)>,
}

impl ThinBuildPlan {
    /// Build only `package`, resolved in `graph` so its runtime is the one a
    /// collection build would produce. `features` are extra features of the
    /// app itself.
    pub(crate) fn new(
        root: &Path,
        package: &str,
        graph: &RuntimeGraph,
        profile: &str,
        features: &[String],
        target_dir: Option<PathBuf>,
    ) -> Self {
        let mut packages = graph.packages.clone();
        if !packages.iter().any(|p| p == package) {
            // An app outside the default collection (the browser, a new app)
            // joins the graph; its runtime then differs from the released
            // one, which the content-derived runtime version accounts for.
            packages.push(package.to_owned());
        }
        let mut app_features = graph.features.clone();
        for feature in features {
            let qualified = if feature.contains('/') {
                feature.clone()
            } else {
                format!("{package}/{feature}")
            };
            if !app_features.contains(&qualified) {
                app_features.push(qualified);
            }
        }
        let mut env = vec![
            ("REPO".to_owned(), root.display().to_string()),
            ("PROFILE".to_owned(), profile.to_owned()),
            ("INCLUDE_PACKAGES".to_owned(), packages.join(" ")),
            ("BUILD_PACKAGES".to_owned(), package.to_owned()),
            ("EXCLUDE_PACKAGES".to_owned(), String::new()),
            ("APP_FEATURES".to_owned(), app_features.join(",")),
        ];
        if let Some(dir) = &target_dir {
            env.push(("CARGO_TARGET_DIR".to_owned(), dir.display().to_string()));
        }
        Self {
            root: root.to_path_buf(),
            profile: profile.to_owned(),
            target_dir,
            env,
        }
    }

    /// The command that runs the build. `CARGO` is inherited, so a drop-in
    /// such as a remote builder is used by the script as well.
    pub(crate) fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new("bash");
        command
            .arg(self.root.join(THIN_BUILD_SCRIPT))
            .current_dir(&self.root)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        if self.target_dir.is_none() {
            command.env_remove("CARGO_TARGET_DIR");
        }
        command
    }

    /// Where the script leaves binaries and `libruntime_dylib.so`.
    pub(crate) fn output_dir(&self) -> PathBuf {
        self.target_dir
            .clone()
            .unwrap_or_else(|| self.root.join("target"))
            .join(&self.profile)
    }

    /// Each of `binaries` in [`Self::output_dir`].
    ///
    /// # Errors
    ///
    /// Returns an error naming the first binary the build did not produce.
    pub(crate) fn binaries(&self, binaries: &[String]) -> Result<HashMap<String, PathBuf>> {
        let out = self.output_dir();
        binaries
            .iter()
            .map(|binary| {
                let path = out.join(binary);
                if path.is_file() {
                    Ok((binary.clone(), path))
                } else {
                    bail!(
                        "{THIN_BUILD_SCRIPT} did not produce {}; a thin package holds the \
                         manifest's `entrypoint.bin` only",
                        path.display()
                    )
                }
            })
            .collect()
    }
}

/// Cargo's target directory override from the environment, which the thin
/// build script honours too.
pub(crate) fn target_dir_override() -> Option<PathBuf> {
    std::env::var_os("CARGO_TARGET_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Run the thin build and return each requested binary's path.
///
/// # Errors
///
/// Returns an error when the script fails or leaves a binary missing.
pub(crate) fn build(plan: &ThinBuildPlan, binaries: &[String]) -> Result<HashMap<String, PathBuf>> {
    let status = plan
        .command()
        .status()
        .with_context(|| format!("starting {THIN_BUILD_SCRIPT}"))?;
    if !status.success() {
        bail!("{THIN_BUILD_SCRIPT} failed with {status}");
    }
    plan.binaries(binaries)
}

/// The Rust standard library shared object thin binaries load, from the
/// toolchain's target libdir.
///
/// # Errors
///
/// Returns an error when `rustc` fails or the libdir holds no `libstd-*.so`.
pub(crate) fn rust_libstd() -> Result<PathBuf> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output = ProcessCommand::new(&rustc)
        .args(["--print", "target-libdir"])
        .output()
        .with_context(|| format!("starting {}", rustc.to_string_lossy()))?;
    if !output.status.success() {
        bail!("rustc --print target-libdir failed with {}", output.status);
    }
    let libdir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    std::fs::read_dir(&libdir)
        .with_context(|| format!("reading {}", libdir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libstd-"))
                && path.extension().is_some_and(|ext| ext == "so")
        })
        .ok_or_else(|| anyhow::anyhow!("no libstd-*.so in {}", libdir.display()))
}

/// The runtime version a locally built runtime is published under:
/// `0.0.0-local+<hash>`, where the hash covers the runtime dylib and the name
/// of the Rust standard library it links (which encodes the toolchain).
///
/// It is valid semver, and never equal to a released `tilo-runtime/<version>`,
/// so installing it beside the system runtime cannot change what any other
/// app loads.
///
/// # Errors
///
/// Returns an error when the dylib cannot be read.
pub(crate) fn local_runtime_version(dylib: &Path, libstd: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&std::fs::read(dylib).with_context(|| format!("reading {}", dylib.display()))?);
    hasher.update(b"\0");
    hasher.update(
        libstd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_bytes(),
    );
    let hash = hasher.finalize().to_hex();
    Ok(format!("0.0.0-local+{}", hash.get(..16).unwrap_or(&hash)))
}

/// Rewrite `[package].runtime` in a manifest's source to `<name>/<version>`,
/// keeping the declared runtime name.
///
/// # Errors
///
/// Returns an error when the manifest declares no `[package].runtime`: a thin
/// app cannot start without one.
pub(crate) fn retarget_runtime(source: &str, version: &str) -> Result<(String, String)> {
    let mut section = String::new();
    let mut reference = None;
    let mut lines = Vec::new();
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            trimmed
                .trim_end()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .clone_into(&mut section);
        }
        let key = trimmed.split('=').next().map(str::trim);
        if section == "package" && key == Some("runtime") && reference.is_none() {
            let declared = trimmed
                .split_once('=')
                .map(|(_, value)| value.trim().trim_matches('"'))
                .unwrap_or_default();
            let name = declared.split_once('/').map_or(declared, |(name, _)| name);
            let name = if name.is_empty() {
                "tilo-runtime"
            } else {
                name
            };
            let retargeted = format!("{name}/{version}");
            let ending = if line.ends_with('\n') { "\n" } else { "" };
            lines.push(format!("runtime = \"{retargeted}\"{ending}"));
            reference = Some(retargeted);
            continue;
        }
        lines.push(line.to_owned());
    }
    let reference = reference.ok_or_else(|| {
        anyhow::anyhow!(
            "tilo-app.toml declares no [package].runtime; a thin app needs one, for example \
             runtime = \"tilo-runtime/2026.07\""
        )
    })?;
    Ok((lines.concat(), reference))
}

/// A baked runtime bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedRuntime {
    /// `<name>/<version>`, as the packaged manifest names it.
    pub reference: String,
    /// The bundle directory (holding `bundle.toml`).
    pub bundle: PathBuf,
}

/// Bake the runtime bundle for `version` at `out/tilo-runtime/<version>`
/// from the host root plus the runtime dylib and libstd, the way
/// `dist/apps/package-apps.sh` does.
///
/// Without `identity` the bake script creates (or reuses)
/// `out/identity.json` and signs with it.
///
/// # Errors
///
/// Returns an error when the bake script fails.
pub(crate) fn bake_runtime(
    root: &Path,
    out: &Path,
    version: &str,
    libs: &[&Path],
    identity: Option<&Path>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let out = out
        .canonicalize()
        .with_context(|| format!("resolving {}", out.display()))?;
    let extra_libs = libs
        .iter()
        .map(|lib| lib.display().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = ProcessCommand::new("bash");
    command
        .arg(root.join(BAKE_RUNTIME_SCRIPT))
        .current_dir(root)
        .env("OUT_DIR", &out)
        .env("RUNTIME_VERSION", version)
        .env("FROM", "/")
        .env("EXTRA_LIBS", extra_libs)
        .env_remove("TRUST_LIST");
    if let Some(identity) = identity {
        command.env("IDENTITY_JSON", identity);
    } else {
        command.env_remove("IDENTITY_JSON");
    }
    let status = command
        .status()
        .with_context(|| format!("starting {BAKE_RUNTIME_SCRIPT}"))?;
    if !status.success() {
        bail!("{BAKE_RUNTIME_SCRIPT} failed with {status}");
    }
    Ok(out.join("tilo-runtime").join(version))
}
