//! `--target linux|macos|web`: one app source, three artifacts.
//!
//! [`build`] produces the runnable artifact under
//! `<target-dir>/tilo-app/<target>/<package>/`: a native binary on Linux, an
//! ad-hoc-signed `.app` on macOS, a served bundle on the web. [`package`]
//! turns it into the distributable: a signed `.tapp`, a signed (and, when
//! configured, notarized) `.app` zip, or an immutable content-addressed web
//! version. Before anything compiles, [`check_target`] refuses a target the
//! manifest does not declare, a required capability the target cannot
//! provide, and a capability the code uses that the manifest does not
//! declare, with the fix.

mod macos;
mod serve;
mod web;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context as _, Result, bail};
pub use tilo_caps::Target;
use tilo_caps::{Capability, Finding};

pub use serve::ServeOptions;

/// What to build, for which target.
#[derive(Clone, Debug)]
pub struct TargetRequest {
    pub target: Target,
    /// Optimized build (`release` natively, the web release profile on the web).
    pub release: bool,
    /// Accept required capabilities whose backend is only planned.
    pub allow_planned: bool,
    /// Web only: which bundle to build.
    pub bundle: WebBundle,
    pub features: Vec<String>,
    pub no_default_features: bool,
}

impl TargetRequest {
    #[must_use]
    pub fn new(target: Target) -> Self {
        Self {
            target,
            release: false,
            allow_planned: false,
            bundle: WebBundle::Auto,
            features: Vec::new(),
            no_default_features: false,
        }
    }
}

/// The web bundle a web build produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum WebBundle {
    /// The desktop bundle for Tilo's own web apps, otherwise the app alone.
    Auto,
    /// The page runtime with only this app.
    App,
    /// Every web-capable Tilo app (inside a Tilo checkout).
    Desktop,
}

/// A built, runnable artifact.
#[derive(Clone, Debug)]
pub struct BuildOutcome {
    pub target: Target,
    /// The binary, the `.app`, or the web bundle directory.
    pub artifact: PathBuf,
    /// What to open: the binary (also inside a `.app`) or the page path
    /// (`app/#/me/<id>`) relative to the bundle.
    pub entry: String,
}

/// An app project as the target commands see it.
#[derive(Clone, Debug)]
pub struct Project {
    pub app_dir: PathBuf,
    pub cargo_manifest: PathBuf,
    pub package: String,
    pub lib_name: String,
    pub bin: String,
    pub manifest_path: PathBuf,
    pub manifest: tilo_app_package::Manifest,
    /// The Cargo workspace the app builds in (the app itself outside Tilo).
    pub workspace_root: PathBuf,
    /// Cargo's target directory for that workspace; never redirected.
    pub target_dir: PathBuf,
    /// The Tilo checkout that provides the SDK (a path or a Cargo git
    /// checkout), for the browser runtime's page host and fonts.
    pub sdk_root: PathBuf,
}

impl Project {
    /// Resolve the app containing `start`.
    ///
    /// # Errors
    ///
    /// When there is no app, its manifests do not parse, or Cargo cannot
    /// describe the workspace.
    pub fn load(start: &Path) -> Result<Self> {
        let app_dir = crate::resolve_app_dir(start)?;
        let cargo_manifest = app_dir.join("Cargo.toml");
        let cargo: toml::Table = read_toml(&cargo_manifest)?;
        let package = cargo
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} has no [package].name", cargo_manifest.display()))?
            .to_owned();
        let lib_name = cargo
            .get("lib")
            .and_then(|lib| lib.get("name"))
            .and_then(toml::Value::as_str)
            .map_or_else(|| package.replace('-', "_"), str::to_owned);
        let bin = cargo
            .get("package")
            .and_then(|package| package.get("default-run"))
            .and_then(toml::Value::as_str)
            .unwrap_or(&package)
            .to_owned();
        let manifest_path = app_dir.join("tilo-app.toml");
        let manifest_source = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        let metadata = cargo_metadata(&cargo_manifest, true)?;
        let workspace_root = json_path(&metadata, "workspace_root")?;
        let target_dir = json_path(&metadata, "target_directory")?;
        let sdk_root = if is_sdk_checkout(&workspace_root) {
            workspace_root.clone()
        } else {
            sdk_root_from_dependencies(&cargo_manifest)?
        };
        Ok(Self {
            app_dir,
            cargo_manifest,
            package,
            lib_name,
            bin,
            manifest_path,
            manifest,
            workspace_root,
            target_dir,
            sdk_root,
        })
    }

    /// The app's display name.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.manifest
            .package
            .name
            .clone()
            .unwrap_or_else(|| self.package.clone())
    }

    /// `<target-dir>/tilo-app/<target>/<package>`.
    #[must_use]
    pub fn output_dir(&self, target: Target) -> PathBuf {
        self.target_dir
            .join("tilo-app")
            .join(target.as_str())
            .join(&self.package)
    }

    fn in_sdk_checkout(&self) -> bool {
        self.workspace_root == self.sdk_root
    }
}

fn is_sdk_checkout(root: &Path) -> bool {
    root.join("crates/sdk/tilo-app/Cargo.toml").is_file()
        && root
            .join("crates/browser/tilo-browser-runtime/Cargo.toml")
            .is_file()
}

fn sdk_root_from_dependencies(cargo_manifest: &Path) -> Result<PathBuf> {
    let metadata = cargo_metadata(cargo_manifest, false)?;
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata lists no packages")?;
    for package in packages {
        if package.get("name").and_then(serde_json::Value::as_str) != Some("tilo-app") {
            continue;
        }
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .context("tilo-app has no manifest path")?;
        if let Some(root) = manifest.ancestors().nth(4)
            && is_sdk_checkout(root)
        {
            return Ok(root.to_path_buf());
        }
    }
    bail!(
        "cannot find the Tilo SDK checkout this app builds against\n  help: the app must depend on `ui = {{ package = \"tilo-app\", … }}` from a Tilo checkout or git source"
    )
}

fn cargo_metadata(manifest: &Path, no_deps: bool) -> Result<serde_json::Value> {
    let mut command = ProcessCommand::new(cargo_program());
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--manifest-path")
        .arg(manifest)
        .stderr(Stdio::inherit());
    if no_deps {
        command.arg("--no-deps");
    }
    let output = command.output().context("running cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed for {}", manifest.display());
    }
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata")
}

fn json_path(value: &serde_json::Value, key: &str) -> Result<PathBuf> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .with_context(|| format!("cargo metadata has no {key}"))
}

pub(crate) fn read_toml(path: &Path) -> Result<toml::Table> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .parse()
        .with_context(|| format!("parsing {}", path.display()))
}

pub(crate) fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// Check that `target` is declared, every required capability can be
/// provided there, and every capability the code uses is declared. Returns the non-fatal notes (planned backends allowed by
/// `allow_planned`).
///
/// # Errors
///
/// With every problem and its fix, when the build must not start.
pub fn check_target(project: &Project, target: Target, allow_planned: bool) -> Result<Vec<String>> {
    let platforms = project.manifest.package.platforms();
    let declared = platforms
        .iter()
        .any(|platform| Target::from_platform(platform.as_str()) == Some(target));
    let manifest = display_relative(&project.app_dir, &project.manifest_path);
    let mut errors = String::new();
    if !declared {
        let listed = platforms
            .iter()
            .map(|platform| format!("\"{}\"", platform.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        let reason = project
            .manifest
            .package
            .platform_reason
            .as_deref()
            .map_or_else(String::new, |reason| format!("\n  note: {reason}"));
        let _ = write!(
            errors,
            "error[platform]: `{target}` is not one of this app's platforms\n  --> {manifest}  platforms = [{listed}]{reason}\n  help: add \"{target}\" to `package.platforms` once the app works there, or build for {listed}\n"
        );
    }
    let capabilities = &project.manifest.capabilities;
    let mut notes = Vec::new();
    for finding in tilo_caps::check(&capabilities.required, &capabilities.optional, target) {
        let message = render_finding(&finding, &manifest);
        if finding.is_error(allow_planned) {
            errors.push_str(&message);
        } else {
            notes.push(message);
        }
    }
    let declared: Vec<&str> = capabilities
        .required
        .iter()
        .chain(&capabilities.optional)
        .map(String::as_str)
        .collect();
    for (capability, sites) in crate::capability_usage::undeclared(&project.app_dir, &declared)? {
        errors.push_str(&crate::capability_usage::render(
            &capability,
            &sites,
            &manifest,
        ));
    }
    if errors.is_empty() {
        Ok(notes)
    } else {
        bail!("{}", errors.trim_end())
    }
}

fn render_finding(finding: &Finding, manifest: &str) -> String {
    match finding {
        Finding::Unknown { name, suggestion } => {
            let help = suggestion.map_or_else(
                || "  help: `tilo-app add capability --list` shows every capability\n".to_owned(),
                |capability| format!("  help: did you mean `{capability}`?\n"),
            );
            format!(
                "error[capability]: `{name}` is not a Tilo capability\n  --> {manifest}\n{help}"
            )
        }
        Finding::RequiredUnsupported {
            capability,
            target,
            reason,
        } => {
            let others = Target::ALL
                .iter()
                .filter(|other| capability.availability(**other).is_usable())
                .map(|other| format!("\"{other}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "error[capability]: `{capability}` is required but not available on target `{target}`: {reason}\n  --> {manifest}  required = [\"{capability}\"]\n  help: move it to `optional` and degrade where it is missing,\n        or declare `platforms = [{others}]` with a `platform-reason`\n"
            )
        }
        Finding::RequiredPlanned { capability, target } => format!(
            "error[capability]: `{capability}` is required but Tilo has no `{target}` backend for it yet\n  --> {manifest}  required = [\"{capability}\"]\n  help: move it to `optional` so the app degrades, or pass `--allow-planned` to build anyway\n"
        ),
    }
}

fn display_relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `tilo-app check --target …`: the platform and capability checks for each
/// target, then `cargo check` wherever this host can compile the target
/// (the web everywhere, Linux and macOS on their own hosts).
///
/// # Errors
///
/// With the first target's problems, or when a compile check fails.
pub fn check_targets(start: &Path, targets: &[Target], allow_planned: bool) -> Result<()> {
    let project = Project::load(start)?;
    for &target in targets {
        for note in check_target(&project, target, allow_planned)? {
            eprintln!("{note}");
        }
        let triple = match target {
            Target::Web => Some("wasm32-unknown-unknown"),
            _ if check_host(target).is_ok() => None,
            _ => {
                println!(
                    "[check] {target}: manifest and capabilities OK; compiling needs a {target} host"
                );
                continue;
            }
        };
        let mut command = ProcessCommand::new(cargo_program());
        command
            .arg("check")
            .arg("--manifest-path")
            .arg(&project.cargo_manifest)
            .current_dir(&project.app_dir);
        if let Some(triple) = triple {
            command.args(["--lib", "--target", triple]);
        }
        let status = command.status().context("running cargo check")?;
        if !status.success() {
            bail!("cargo check for {target} failed with {status}");
        }
        println!("[check] {target}: OK");
    }
    Ok(())
}

/// Whether this machine can build `target`, and the fix when it cannot.
///
/// # Errors
///
/// When the host lacks the target's toolchain.
pub fn check_host(target: Target) -> Result<()> {
    match target {
        Target::Macos if !cfg!(target_os = "macos") => bail!(
            "building for macOS needs a Mac (Apple SDK, codesign)\n  help: `tilo-app check --target macos` runs the portability and capability checks here"
        ),
        Target::Linux if !cfg!(target_os = "linux") => bail!(
            "building for Linux needs a Linux host (the Linux lane in CI builds it)\n  help: `tilo-app check --target linux` runs the portability and capability checks here"
        ),
        _ => Ok(()),
    }
}

/// Build the runnable artifact for `request.target`.
///
/// # Errors
///
/// When the target or capability checks fail, or the build does.
pub fn build(start: &Path, request: &TargetRequest) -> Result<BuildOutcome> {
    let project = Project::load(start)?;
    build_project(&project, request)
}

/// Assemble a macOS bundle from existing inputs, without compiling, signing,
/// executing, or notarizing them. The caller validates the supplied executable
/// architecture, linkage and eventual signatures before distribution.
///
/// # Errors
///
/// Refuses failed platform/capability checks, missing inputs, unsafe bundle
/// names, and existing output directories. Uses the normal SDK assembler.
pub fn assemble_macos(
    start: &Path,
    binary: &Path,
    runtime_host: &Path,
    output: &Path,
) -> Result<PathBuf> {
    let project = Project::load(start)?;
    for note in check_target(&project, Target::Macos, false)? {
        eprintln!("{note}");
    }
    assemble_macos_project(&project, binary, runtime_host, output)
}

fn assemble_macos_project(
    project: &Project,
    binary: &Path,
    runtime_host: &Path,
    output: &Path,
) -> Result<PathBuf> {
    // Assembly follows the app catalog's declared entrypoint. A Cargo package
    // may have a differently named binary without setting `default-run`.
    let mut project = project.clone();
    project.bin = project
        .manifest
        .entrypoint
        .as_ref()
        .context("manifest has no entrypoint to assemble")?
        .bin
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("manifest entrypoint has no executable filename")?
        .to_owned();
    if project.bin.eq_ignore_ascii_case(macos::RUNTIME_HOST) {
        bail!("app executable collides with reserved macOS runtime helper name");
    }
    for name in [project.display_name(), project.bin.clone()] {
        if name.is_empty()
            || matches!(name.as_str(), "." | "..")
            || name.contains(['/', '\\', '\0'])
        {
            bail!("invalid macOS bundle/executable name: {name:?}");
        }
    }
    for input in [binary, runtime_host] {
        if !std::fs::symlink_metadata(input)?.file_type().is_file() {
            bail!("bundle input must be a regular file: {}", input.display());
        }
    }
    if output.try_exists()? || std::fs::symlink_metadata(output).is_ok() {
        bail!("output already exists: {}", output.display());
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let work = tempfile::Builder::new()
        .prefix(".macos-assemble-")
        .tempdir_in(parent)?;
    let app = macos::assemble(&project, binary, runtime_host, work.path())?;
    let name = app
        .file_name()
        .context("assembled bundle has no filename")?
        .to_owned();
    std::fs::rename(work.path(), output).context("publishing unsigned bundle directory")?;
    Ok(output.join(name))
}

fn build_project(project: &Project, request: &TargetRequest) -> Result<BuildOutcome> {
    for note in check_target(project, request.target, request.allow_planned)? {
        eprintln!("{note}");
    }
    check_host(request.target)?;
    match request.target {
        Target::Linux => {
            let binary = build_native(project, request, None)?;
            let output = project.output_dir(Target::Linux);
            std::fs::create_dir_all(&output)
                .with_context(|| format!("creating {}", output.display()))?;
            let artifact = output.join(&project.bin);
            link_or_copy(&binary, &artifact)?;
            Ok(BuildOutcome {
                target: Target::Linux,
                entry: artifact.display().to_string(),
                artifact,
            })
        }
        Target::Macos => {
            let binary = build_native(project, request, None)?;
            let runtime_host = build_runtime_host(project, request)?;
            let app = macos::assemble(
                project,
                &binary,
                &runtime_host,
                &project.output_dir(Target::Macos),
            )?;
            macos::sign_ad_hoc(&app, &macos::entitlements_path(&app))?;
            Ok(BuildOutcome {
                target: Target::Macos,
                entry: macos::executable(&app, project).display().to_string(),
                artifact: app,
            })
        }
        Target::Web => web::build(project, request),
    }
}

/// Build and start the app: the binary natively, a local server with
/// rebuild-on-change and page reload on the web.
///
/// # Errors
///
/// When the build fails or the app cannot start.
pub fn run(
    start: &Path,
    request: &TargetRequest,
    forwarded: &[OsString],
    serve: &ServeOptions,
) -> Result<()> {
    let project = Project::load(start)?;
    if request.target == Target::Web {
        return serve::run(&project, request, serve);
    }
    let outcome = build_project(&project, request)?;
    eprintln!("[tilo-app] running {}", outcome.entry);
    let status = ProcessCommand::new(&outcome.entry)
        .args(forwarded)
        .current_dir(&project.app_dir)
        .status()
        .with_context(|| format!("starting {}", outcome.entry))?;
    if !status.success() {
        bail!("{} exited with {status}", project.bin);
    }
    Ok(())
}

/// What `tilo-app package --target …` should produce.
#[derive(Clone, Debug)]
pub struct TargetPackageRequest {
    pub build: TargetRequest,
    /// Output path (file for Linux/macOS, directory for the web).
    pub output: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub force: bool,
}

/// Produce the distributable for `request.build.target`.
///
/// # Errors
///
/// When a check, the build, signing or verification fails.
pub fn package(start: &Path, request: &TargetPackageRequest) -> Result<PathBuf> {
    let project = Project::load(start)?;
    let mut build_request = request.build.clone();
    build_request.release = true;
    // A package is this app's artifact, never the shared desktop bundle.
    if build_request.bundle == WebBundle::Auto {
        build_request.bundle = WebBundle::App;
    }
    match build_request.target {
        Target::Linux => {
            for note in check_target(&project, Target::Linux, build_request.allow_planned)? {
                eprintln!("{note}");
            }
            check_host(Target::Linux)?;
            let outcome = crate::package_app(
                start,
                &crate::PackageRequest {
                    sdk: None,
                    output: request.output.clone(),
                    identity: request.identity.clone(),
                    profile: None,
                    static_link: false,
                    target: None,
                    platform: crate::PackagePlatform::Tilo,
                    arch: None,
                    features: build_request.features.clone(),
                    no_default_features: build_request.no_default_features,
                    force: request.force,
                },
            )?;
            Ok(outcome.output)
        }
        Target::Macos => {
            let outcome = build_project(&project, &build_request)?;
            macos::package(&project, &outcome.artifact, request)
        }
        Target::Web => {
            let outcome = build_project(&project, &build_request)?;
            web::package(&project, &outcome.artifact, request)
        }
    }
}

/// Build the app's binary for this host and return its path.
fn build_native(
    project: &Project,
    request: &TargetRequest,
    triple: Option<&str>,
) -> Result<PathBuf> {
    let mut command = ProcessCommand::new(cargo_program());
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&project.cargo_manifest)
        .arg("--bin")
        .arg(&project.bin)
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&project.app_dir)
        .stdout(Stdio::piped());
    if request.release {
        command.arg("--release");
    }
    if let Some(triple) = triple {
        command.arg("--target").arg(triple);
    }
    if request.no_default_features {
        command.arg("--no-default-features");
    }
    if !request.features.is_empty() {
        command.arg("--features").arg(request.features.join(","));
    }
    let expected = project
        .cargo_manifest
        .canonicalize()
        .unwrap_or_else(|_| project.cargo_manifest.clone());
    let artifacts = run_cargo_build(&mut command, &expected)?;
    artifacts
        .get(&project.bin)
        .cloned()
        .with_context(|| format!("Cargo did not report binary {}", project.bin))
}

/// The shared runtime daemon (`tilo-runtime-mcp`, a binary of the SDK's
/// `shell` package) in the app's profile, without the hosted test-control
/// socket. Outside the SDK checkout it builds under the app's target
/// directory, never inside a Cargo git checkout.
fn build_runtime_host(project: &Project, request: &TargetRequest) -> Result<PathBuf> {
    let manifest = project.sdk_root.join("crates/session/shell/Cargo.toml");
    let mut command = ProcessCommand::new(cargo_program());
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .args(["--bin", macos::RUNTIME_HOST, "--no-default-features"])
        .arg("--message-format=json-render-diagnostics")
        .current_dir(&project.sdk_root)
        .stdout(Stdio::piped());
    if request.release {
        command.arg("--release");
    }
    if project.in_sdk_checkout() {
        command.arg("--locked");
    } else {
        command
            .arg("--target-dir")
            .arg(project.target_dir.join("tilo-app/runtime-host"));
    }
    let expected = manifest.canonicalize().unwrap_or(manifest);
    run_cargo_build(&mut command, &expected)?
        .remove(macos::RUNTIME_HOST)
        .with_context(|| format!("Cargo did not report binary {}", macos::RUNTIME_HOST))
}

/// Run a `cargo build --message-format=json…` command, forwarding
/// diagnostics, and collect `name → executable` for `manifest`'s targets.
pub(crate) fn run_cargo_build(
    command: &mut ProcessCommand,
    manifest: &Path,
) -> Result<HashMap<String, PathBuf>> {
    let mut child = command.spawn().context("starting cargo build")?;
    let stdout = child
        .stdout
        .take()
        .context("cargo build has no artifact stream")?;
    let mut artifacts = HashMap::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading cargo artifact stream")?;
        crate::collect_cargo_message(&line, manifest, &mut artifacts);
    }
    let status = child.wait().context("waiting for cargo build")?;
    if !status.success() {
        bail!("cargo build failed with {status}");
    }
    Ok(artifacts)
}

/// Replace `destination` with `source`, by hard link when both are on one
/// file system so a rebuild writes no second copy.
pub(crate) fn link_or_copy(source: &Path, destination: &Path) -> Result<()> {
    // Relinking an unchanged file is not free: it changes the source's
    // metadata, which a watcher on the source tree reports as an edit.
    if let (Ok(from), Ok(to)) = (std::fs::metadata(source), std::fs::metadata(destination))
        && from.len() == to.len()
        && from
            .modified()
            .ok()
            .is_some_and(|time| to.modified().ok() == Some(time))
    {
        return Ok(());
    }
    if destination.exists() {
        std::fs::remove_file(destination)
            .with_context(|| format!("replacing {}", destination.display()))?;
    }
    if std::fs::hard_link(source, destination).is_err() {
        std::fs::copy(source, destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
    }
    Ok(())
}

/// Result of `tilo-app add capability`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityAdded {
    pub manifest: PathBuf,
    pub capability: Capability,
    pub required: bool,
    /// Targets among the app's platforms that cannot provide it, with why.
    pub unavailable: Vec<(Target, String)>,
}

/// Declare `name` in the app's `[capabilities]`, as optional unless
/// `required`. Moves it between the lists if it is already declared.
///
/// # Errors
///
/// When the name is unknown (with a suggestion), or a required capability
/// cannot be provided on one of the app's platforms.
pub fn add_capability(start: &Path, name: &str, required: bool) -> Result<CapabilityAdded> {
    let app_dir = crate::resolve_app_dir(start)?;
    let manifest_path = app_dir.join("tilo-app.toml");
    let capability = Capability::from_name(name).ok_or_else(|| {
        Capability::suggest(name).map_or_else(
            || anyhow::anyhow!("`{name}` is not a Tilo capability\n  help: `tilo-app add capability --list` shows every capability"),
            |suggestion| anyhow::anyhow!("`{name}` is not a Tilo capability\n  help: did you mean `{suggestion}`?"),
        )
    })?;
    let source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&source)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let mut unavailable = Vec::new();
    for platform in manifest.package.platforms() {
        let Some(target) = Target::from_platform(platform.as_str()) else {
            continue;
        };
        match capability.availability(target) {
            tilo_caps::Availability::Available | tilo_caps::Availability::Prompt => {}
            tilo_caps::Availability::Planned => {
                unavailable.push((target, "no backend yet".to_owned()));
            }
            tilo_caps::Availability::Unsupported(reason) => {
                unavailable.push((target, reason.to_owned()));
            }
        }
    }
    if required && !unavailable.is_empty() {
        let targets = unavailable
            .iter()
            .map(|(target, reason)| format!("  {target}: {reason}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "`{capability}` cannot be required: this app's platforms include targets without it\n{targets}\n  help: add it as optional (without --required) and degrade where it is missing, or narrow `package.platforms`"
        );
    }
    let updated = with_capability(&source, capability.name(), required)?;
    tilo_app_package::Manifest::from_toml_str(&updated)
        .context("the updated manifest does not validate")?;
    crate::write_atomic(&manifest_path, updated.as_bytes(), "app manifest")?;
    Ok(CapabilityAdded {
        manifest: manifest_path,
        capability,
        required,
        unavailable,
    })
}

/// `source` with `name` in `[capabilities].required` or `.optional`, and
/// out of the other list. Everything else in the file is kept verbatim.
fn with_capability(source: &str, name: &str, required: bool) -> Result<String> {
    let mut table: toml::Table = source.parse().context("parsing tilo-app.toml")?;
    let capabilities = table
        .entry("capabilities")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let capabilities = capabilities
        .as_table_mut()
        .context("[capabilities] is not a table")?;
    let mut lists = [Vec::new(), Vec::new()];
    for (slot, key) in lists.iter_mut().zip(["required", "optional"]) {
        if let Some(values) = capabilities.get(key).and_then(toml::Value::as_array) {
            *slot = values
                .iter()
                .filter_map(toml::Value::as_str)
                .filter(|value| *value != name)
                .map(str::to_owned)
                .collect();
        }
    }
    let [mut required_list, mut optional_list] = lists;
    if required {
        required_list.push(name.to_owned());
    } else {
        optional_list.push(name.to_owned());
    }
    let block = render_capabilities_block(&required_list, &optional_list);
    Ok(replace_capabilities_block(source, &block))
}

fn render_capabilities_block(required: &[String], optional: &[String]) -> String {
    let list = |values: &[String]| {
        values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut block = String::from("[capabilities]\n");
    if !required.is_empty() {
        let _ = writeln!(block, "required = [{}]", list(required));
    }
    if !optional.is_empty() {
        let _ = writeln!(block, "optional = [{}]", list(optional));
    }
    block
}

/// Replace the `[capabilities]` table (header to the next header) with
/// `block`, or append it.
fn replace_capabilities_block(source: &str, block: &str) -> String {
    let mut output = String::with_capacity(source.len().saturating_add(block.len()));
    let mut in_block = false;
    let mut replaced = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if trimmed == "[capabilities]" {
                in_block = true;
                replaced = true;
                output.push_str(block);
                continue;
            }
            if in_block {
                output.push('\n');
            }
            in_block = false;
        }
        if !in_block {
            output.push_str(line);
        }
    }
    if !replaced {
        if !output.ends_with('\n') && !output.is_empty() {
            output.push('\n');
        }
        output.push('\n');
        output.push_str(block);
    }
    output
}

/// The capability reference: every capability and its availability per
/// target, as `tilo-app add capability --list` prints it.
#[must_use]
pub fn capability_table() -> String {
    let mut table = String::from("capability        linux      macos      web        \n");
    for capability in Capability::ALL {
        let _ = write!(table, "{:<18}", capability.name());
        for target in Target::ALL {
            let cell = match capability.availability(target) {
                tilo_caps::Availability::Available => "yes",
                tilo_caps::Availability::Prompt => "prompt",
                tilo_caps::Availability::Planned => "planned",
                tilo_caps::Availability::Unsupported(_) => "no",
            };
            let _ = write!(table, "{cell:<11}");
        }
        let _ = writeln!(table, "{}", capability.summary());
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[package]\nid = \"so.tilo.demo\"\nversion = \"0.1.0\"\nkind = \"app\"\n\n[entrypoint]\nbin = \"bin/demo\"\ntype = \"gui\"\n";

    #[test]
    fn adding_a_capability_appends_the_table_once() {
        let once = with_capability(MANIFEST, "files.save", false).unwrap();
        assert!(
            once.ends_with("[capabilities]\noptional = [\"files.save\"]\n"),
            "{once}"
        );
        let twice = with_capability(&once, "network.http", true).unwrap();
        assert_eq!(twice.matches("[capabilities]").count(), 1, "{twice}");
        assert!(twice.contains("required = [\"network.http\"]\noptional = [\"files.save\"]\n"));
        assert!(twice.starts_with(MANIFEST));
    }

    #[test]
    fn a_capability_moves_between_the_lists_and_later_tables_survive() {
        let source = format!(
            "{MANIFEST}\n[capabilities]\noptional = [\"network.http\"]\n\n[providers]\nmime = []\n"
        );
        let moved = with_capability(&source, "network.http", true).unwrap();
        assert!(
            moved.contains(
                "[capabilities]\nrequired = [\"network.http\"]\n\n[providers]\nmime = []\n"
            ),
            "{moved}"
        );
        assert!(!moved.contains("optional"));
    }

    #[test]
    fn the_capability_table_lists_every_capability() {
        let table = capability_table();
        for capability in Capability::ALL {
            assert!(table.contains(capability.name()));
        }
    }

    #[test]
    fn unsupported_required_capabilities_name_the_fix() {
        let message = render_finding(
            &Finding::RequiredUnsupported {
                capability: Capability::Processes,
                target: Target::Web,
                reason: "A browser page cannot start programs.",
            },
            "tilo-app.toml",
        );
        assert!(message.starts_with(
            "error[capability]: `processes` is required but not available on target `web`"
        ));
        assert!(
            message.contains("platforms = [\"linux\", \"macos\"]"),
            "{message}"
        );
        assert!(message.contains("move it to `optional`"));
    }
}
