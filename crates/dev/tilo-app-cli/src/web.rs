//! The web build of an app: `tilo-app build --platform web`, `package
//! --platform web`, `dev --platform web` and `run --platform web`.
//!
//! `wasm32` has no dynamic linking and the browser runtime is one module, so
//! an app's web build is the runtime bundle with that app registered: the
//! `app` bundle of [`crate::targets`] (`build --target web --bundle app`),
//! which `dev` and `run` serve and rebuild. A registry web package was a
//! side module for a separately published web core; this runtime has no such
//! core, so `package --platform web` names `package --target web`, which
//! writes the bundle in the layout Cloud serves.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest as _, Sha256};

use crate::targets::{ServeOptions, Target, TargetRequest, WebBundle};
use crate::{DevRequest, PackageOutcome, PackageRequest, resolve_app_dir};

/// The runtime module inside a bundle's `pkg/`.
const BUNDLE_MODULE: &str = "pkg/tilo_browser_runtime_bg.wasm";

/// What to build.
#[derive(Debug, Default)]
pub struct WebBuildRequest {
    /// A directory to hold a copy of the bundle; the build itself stays in
    /// `<target>/tilo-app/web/<package>/bundle`.
    pub output: Option<PathBuf>,
    /// Kept for the command line: Cargo rebuilds only what changed, so an
    /// edit to the app relinks the bundle without rebuilding the runtime.
    pub app_only: bool,
    /// Optimized (`--release`): the workspace's `wasm-release` profile and
    /// `wasm-opt`, the module a page should stream.
    pub release: bool,
    /// With `release`: keep `debug_assertions` and overflow checks (the
    /// workspace's `wasm-assert` profile), so a measured or qualified bundle
    /// is never faster for having stopped checking.
    pub assertions: bool,
}

/// A finished web build: the bundle directory and its one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebBuild {
    pub app_dir: PathBuf,
    /// The bundle directory (`--output`'s copy when given).
    pub output: PathBuf,
    pub app_id: String,
    /// The page to open inside `output`.
    pub entry: String,
    /// The runtime and the app, linked into one module.
    pub module: PathBuf,
    pub module_bytes: u64,
    pub module_sha256: String,
    pub elapsed: Duration,
}

impl WebBuild {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_dir": self.app_dir,
            "output": self.output,
            "app_id": self.app_id,
            "entry": self.entry,
            "module": self.module,
            "module_bytes": self.module_bytes,
            "module_sha256": self.module_sha256,
            "elapsed_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// Build the app at `start` for the web: the browser runtime bundle with the
/// app registered.
pub fn build_web(start: &Path, request: &WebBuildRequest) -> Result<WebBuild> {
    let app_dir = resolve_app_dir(start)?;
    let portability = crate::portability::lint_app(&app_dir)?;
    if !portability.web_capable() {
        bail!(
            "{} cannot run on the web:\n{}",
            app_dir.display(),
            portability.render_web()
        );
    }
    let started = Instant::now();
    let outcome =
        crate::targets::build(&app_dir, &app_bundle(request.release, request.assertions))?;
    let output = match &request.output {
        Some(path) => {
            let output = crate::absolute_from(&app_dir, path);
            replace_tree(&outcome.artifact, &output)?;
            output
        }
        None => outcome.artifact.clone(),
    };
    let module = output.join(BUNDLE_MODULE);
    let bytes = std::fs::read(&module).with_context(|| format!("reading {}", module.display()))?;
    let project = crate::targets::Project::load(&app_dir)?;
    Ok(WebBuild {
        app_dir,
        output,
        app_id: project.manifest.package.id,
        entry: outcome.entry,
        module,
        module_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        module_sha256: hex::encode(Sha256::digest(&bytes)),
        elapsed: started.elapsed(),
    })
}

fn app_bundle(release: bool, assertions: bool) -> TargetRequest {
    let mut request = TargetRequest::new(Target::Web);
    request.bundle = WebBundle::App;
    request.release = release;
    request.assertions = assertions;
    request
}

/// Replace `to` with a copy of the directory `from`.
fn replace_tree(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to).with_context(|| format!("removing {}", to.display()))?;
    }
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    crate::copy_payload_tree(from, to)
}

/// `tilo-app package --platform web`: the registry side-module package,
/// which needs a web core this runtime does not have.
///
/// # Errors
///
/// Always, naming the package the web target writes instead.
pub(crate) fn package_web(start: &Path, _request: &PackageRequest) -> Result<PackageOutcome> {
    let app_dir = resolve_app_dir(start)?;
    bail!(
        "a web package of {} would be a side module for a separately published web core, which \
         the browser runtime no longer has: it links an app into its own bundle\n  help: \
         `tilo-app package --target web` writes that bundle in the layout Cloud serves",
        app_dir.display()
    )
}

/// How `dev --platform web` serves the page.
#[derive(Clone, Debug)]
pub struct WebDevOptions {
    /// The address the page is served on.
    pub bind: String,
    /// Open the app in a room other people join from their own browser
    /// (`--share`).
    pub share: bool,
    /// With `share`, start an empty room (`--fresh`).
    pub fresh: bool,
}

/// `tilo-app dev --platform web`: build, serve the bundle, and rebuild and
/// reload the page on every change.
///
/// # Errors
///
/// When the address is invalid, the first build fails or serving cannot
/// start.
pub fn dev_web(start: &Path, request: &DevRequest, options: &WebDevOptions) -> Result<()> {
    serve_web(start, options, true, request.debounce)
}

/// `tilo-app run --platform web`: build once and serve the bundle until the
/// process stops.
///
/// # Errors
///
/// When the address is invalid, the build fails or serving cannot start.
pub fn run_web(start: &Path, options: &WebDevOptions) -> Result<()> {
    serve_web(start, options, false, ServeOptions::default().debounce)
}

fn serve_web(start: &Path, options: &WebDevOptions, watch: bool, debounce: Duration) -> Result<()> {
    let address = options
        .bind
        .parse()
        .with_context(|| format!("--bind {} is not an address and port", options.bind))?;
    crate::targets::run(
        start,
        &app_bundle(false, false),
        &[],
        &ServeOptions {
            address,
            watch,
            debounce,
            share: options.share,
            fresh: options.fresh,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_build_is_the_app_bundle() {
        let request = app_bundle(true, false);
        assert_eq!(request.target, Target::Web);
        assert_eq!(request.bundle, WebBundle::App);
        assert!(request.release);
        assert!(!request.assertions);
        // A qualification build asks for the asserting release profile; the
        // request carries it rather than the bundle deciding for itself.
        assert!(app_bundle(true, true).assertions);
    }

    #[test]
    fn an_output_copy_replaces_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("bundle");
        std::fs::create_dir_all(bundle.join("pkg")).unwrap();
        std::fs::write(bundle.join("pkg/module.wasm"), "wasm").unwrap();
        let output = dir.path().join("out");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("stale.js"), "old").unwrap();
        replace_tree(&bundle, &output).unwrap();
        assert!(!output.join("stale.js").exists());
        assert_eq!(
            std::fs::read_to_string(output.join("pkg/module.wasm")).unwrap(),
            "wasm"
        );
    }
}
