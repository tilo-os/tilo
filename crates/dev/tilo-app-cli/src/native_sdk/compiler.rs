//! Compile authored code against a signed, runtime-bound compiler interface.
//! No toolkit dependency is rebuilt or substituted by this path.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::SdkSelection;
use crate::{AppPackageSpec, BuildPlan, PackageRequest};

/// Independent content selection of the developer compiler bundle.
/// Its signer must be the independently selected runtime publisher.
#[derive(Debug, Clone)]
pub struct CompilerSelection {
    /// Signed compiler bundle directory.
    pub bundle: PathBuf,
    /// Independently obtained content root.
    pub root: String,
}

#[derive(Debug, Deserialize)]
struct Interface {
    schema: u32,
    runtime: RuntimeBinding,
    compiler: String,
    host: String,
    architecture: String,
    profile: String,
    ui: String,
    units: Vec<Unit>,
    resources: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimeBinding {
    version: String,
    root: String,
    publisher: String,
    toolkit_sha256: String,
}

#[derive(Debug, Deserialize)]
struct Unit {
    target: String,
    kind: Vec<String>,
    features: Vec<String>,
    artifacts: Vec<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    name: String,
    size: u64,
    sha256: String,
}

struct Inputs {
    runtime: PathBuf,
    compiler: PathBuf,
    interface: Interface,
}

/// The source SDK's canonical declarations, captured once by its producer.
/// Consumers read the signed result and never resolve these dependencies.
///
/// # Errors
/// Fails if the locked SDK graph or its source declarations cannot be read.
pub fn capture_resources(repo: &Path) -> Result<Vec<String>> {
    let app = repo.join("crates/sdk/tilo-app");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(app.join("Cargo.toml"))
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        bail!("cannot observe the locked SDK resource graph");
    }
    crate::resource_kinds::declared_with_metadata(&app, &serde_json::from_slice(&output.stdout)?)
}

pub(crate) fn build(
    spec: &AppPackageSpec,
    plan: &BuildPlan<'_>,
    request: &PackageRequest,
) -> Result<HashMap<String, PathBuf>> {
    if plan.profile != "thin-app" {
        bail!("SDK compiler consumers must use the declared thin-app profile");
    }
    let selected = request.sdk.as_ref().context("SDK selection is required")?;
    let root = crate::find_repo_root(&spec.app_dir).context("SDK checkout is required")?;
    let inputs = checked_inputs(selected, &root, &spec.app_dir, plan.triple)?;
    let stage = tempfile::Builder::new()
        .prefix(".tilo-sdk-build-")
        .tempdir_in(&spec.app_dir)?;
    stage_manifest(spec, &inputs.interface, stage.path())?;
    let manifest = stage.path().join("Cargo.toml");
    let mut command = consumer_command(spec, plan, request, &inputs, &manifest)?;
    let mut child = command.spawn().context("starting SDK consumer compiler")?;
    let mut artifacts = HashMap::new();
    for line in BufReader::new(
        child
            .stdout
            .take()
            .context("missing Cargo artifact stream")?,
    )
    .lines()
    {
        let line = line?;
        validate_artifact_message(&line, &manifest)?;
        crate::collect_cargo_message(&line, &manifest, &mut artifacts);
    }
    if !child.wait()?.success() {
        bail!("authored app compilation against the signed SDK failed");
    }
    checked_inputs(selected, &root, &spec.app_dir, plan.triple)?;
    artifacts.insert("sdk:authored-source".into(), stage.path().to_owned());
    artifacts.insert("sdk:compiler".into(), inputs.compiler.clone());
    let _retained = stage.keep();
    artifacts.insert(
        "sdk:runtime_dylib".into(),
        inputs.runtime.join("lib/libruntime_dylib.so"),
    );
    Ok(artifacts)
}

fn consumer_command(
    spec: &AppPackageSpec,
    plan: &BuildPlan<'_>,
    request: &PackageRequest,
    inputs: &Inputs,
    manifest: &Path,
) -> Result<Command> {
    let root = crate::find_repo_root(&spec.app_dir).context("SDK checkout is required")?;
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .args([
            "build",
            "--locked",
            "--profile",
            "thin-app",
            "--message-format=json-render-diagnostics",
        ])
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--features")
        .arg(
            request
                .features
                .iter()
                .cloned()
                .chain(["thin".into()])
                .collect::<Vec<_>>()
                .join(","),
        )
        .arg("--config")
        .arg(cargo_path_option(
            "build.rustc-workspace-wrapper",
            &root.join("tools/ci/rustc-sdk-consumer.sh"),
        )?)
        .arg("--config")
        .arg(cargo_path_option(
            "env.TILO_SDK_COMPILER",
            &inputs.compiler,
        )?)
        .arg("--config")
        .arg(cargo_path_option("env.TILO_SDK_RUNTIME", &inputs.runtime)?)
        .current_dir(&spec.app_dir)
        .stdout(Stdio::piped());
    let cargo: toml::Value =
        toml::from_str(&std::fs::read_to_string(spec.app_dir.join("Cargo.toml"))?)?;
    let mut authored = vec![
        cargo
            .get("lib")
            .and_then(|lib| lib.get("name"))
            .and_then(toml::Value::as_str)
            .unwrap_or(&spec.cargo_package)
            .replace('-', "_"),
    ];
    authored.extend(plan.bins.iter().map(|(bin, _)| bin.replace('-', "_")));
    command.arg("--config").arg(format!(
        "env.TILO_SDK_AUTHORED_CRATES={:?}",
        authored.join(":")
    ));
    if request.no_default_features {
        command.arg("--no-default-features");
    }
    for (bin, _) in plan.bins {
        command.arg("--bin").arg(bin);
    }
    for option in [
        "CODEGEN_UNITS",
        "LTO",
        "OPT_LEVEL",
        "PANIC",
        "STRIP",
        "DEBUG",
    ] {
        command.env_remove(format!("CARGO_PROFILE_RELEASE_{option}"));
    }
    Ok(command)
}

fn cargo_path_option(key: &str, path: &Path) -> Result<String> {
    let value = toml::Value::String(
        path.to_str()
            .context("compiler path is not UTF-8")?
            .to_owned(),
    );
    Ok(format!("{key}={value}"))
}

fn validate_artifact_message(line: &str, manifest: &Path) -> Result<()> {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(());
    };
    if message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-artifact")
        && message
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .map(Path::new)
            != Some(manifest)
    {
        bail!(
            "SDK consumer attempted to rebuild a dependency instead of using its signed interface"
        );
    }
    Ok(())
}

fn checked_inputs(
    selected: &SdkSelection,
    root: &Path,
    app: &Path,
    triple: Option<&str>,
) -> Result<Inputs> {
    let arch = std::env::consts::ARCH;
    let version = Command::new("rustc")
        .args(["--version", "--verbose"])
        .current_dir(app)
        .output()?;
    if !version.status.success() {
        bail!("cannot observe the native compiler identity");
    }
    let compiler_version = String::from_utf8(version.stdout)?.trim().to_owned();
    let host = compiler_version
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .context("compiler host is unavailable")?;
    if triple.is_some_and(|t| t != host) {
        bail!("cross-host SDK compiler interfaces are not supported");
    }
    let runtime = cache(selected, root, app, arch)?;
    let compiler_selected = selected
        .compiler
        .as_ref()
        .context("signed SDK compiler interface is required")?;
    let compiler_selection = SdkSelection {
        bundle: compiler_selected.bundle.clone(),
        publisher: selected.publisher.clone(),
        root: compiler_selected.root.clone(),
        compiler: None,
    };
    let compiler = cache(&compiler_selection, root, app, arch)?;
    let runtime_manifest: runtime_bundle::BundleManifest =
        toml::from_str(&std::fs::read_to_string(runtime.join("bundle.toml"))?)?;
    let interface = read_interface(&compiler.join("lib/compiler-interface/manifest.json"))?;
    validate_binding(
        &interface,
        selected,
        &runtime_manifest,
        &compiler_version,
        host,
    )?;
    let toolkit = runtime.join("lib/libruntime_dylib.so");
    if sha256(&toolkit)? != interface.runtime.toolkit_sha256 {
        bail!("compiler interface names a different compiled toolkit");
    }
    validate_inventory(&interface, &compiler, arch)?;
    Ok(Inputs {
        runtime,
        compiler,
        interface,
    })
}

fn read_interface(path: &Path) -> Result<Interface> {
    const MAX_DESCRIPTOR: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_DESCRIPTOR + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR {
        bail!("SDK compiler descriptor exceeds its bound");
    }
    let interface: Interface = serde_json::from_slice(&bytes)?;
    if interface.units.len() > 4096 || interface.resources.len() > 4096 {
        bail!("SDK compiler descriptor exceeds its bound");
    }
    Ok(interface)
}

fn validate_binding(
    interface: &Interface,
    selected: &SdkSelection,
    manifest: &runtime_bundle::BundleManifest,
    compiler: &str,
    host: &str,
) -> Result<()> {
    if interface.schema != 1
        || interface.profile != "thin-app"
        || interface.runtime.root != selected.root
        || interface.runtime.publisher != selected.publisher
        || interface.runtime.version != manifest.version
        || interface.architecture != manifest.arch
        || interface.compiler.trim() != compiler
        || interface.host != host
        || !host.starts_with(&format!("{}-", manifest.arch))
    {
        bail!("signed compiler interface differs from the selected runtime, target or compiler");
    }
    Ok(())
}

fn validate_inventory(interface: &Interface, bundle: &Path, arch: &str) -> Result<()> {
    let directory = bundle.join("lib/compiler-interface/artifacts");
    let mut names = std::collections::BTreeSet::new();
    let mut plugins = Vec::new();
    for unit in &interface.units {
        for artifact in &unit.artifacts {
            if Path::new(&artifact.name)
                .file_name()
                .and_then(|n| n.to_str())
                != Some(&artifact.name)
                || !names.insert(artifact.name.clone())
            {
                bail!("unsafe or duplicate SDK compiler artifact");
            }
            let path = directory.join(&artifact.name);
            if std::fs::metadata(&path)?.len() != artifact.size || sha256(&path)? != artifact.sha256
            {
                bail!("SDK compiler artifact differs from its actual unit inventory");
            }
            if unit.kind.iter().any(|k| k == "proc-macro") {
                plugins.push(path);
            }
        }
    }
    if !names.contains(&interface.ui) || names.len() > 4096 {
        bail!("SDK compiler interface is incomplete or exceeds its bound");
    }
    if !interface.units.iter().any(|unit| {
        unit.target == "tilo_app" && unit.artifacts.iter().any(|a| a.name == interface.ui)
    }) {
        bail!("compiler public unit differs from its declared Cargo identity");
    }
    let actual = std::fs::read_dir(&directory)?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    if actual != names {
        bail!("SDK compiler inventory is not closed");
    }
    super::verify_linkage(bundle, plugins, arch)
}

fn cache(selection: &SdkSelection, root: &Path, app: &Path, arch: &str) -> Result<PathBuf> {
    let original = if selection.bundle.is_absolute() {
        selection.bundle.clone()
    } else {
        app.join(&selection.bundle)
    };
    let parent = root.join(".tilo-native-sdk");
    std::fs::create_dir_all(&parent)?;
    private_directory(&parent)?;
    let destination = parent.join(&selection.root);
    if destination.exists() {
        verify_selected(&original, selection, arch)?;
        private_directory(&destination)?;
    } else {
        let (snapshot, manifest) = super::verified_snapshot(selection, &original, arch)?;
        let staged = tempfile::Builder::new()
            .prefix(".sdk-")
            .tempdir_in(&parent)?;
        std::fs::copy(
            snapshot.path().join("bundle.toml"),
            staged.path().join("bundle.toml"),
        )?;
        for file in manifest.files {
            let to = staged.path().join(&file.path);
            std::fs::create_dir_all(to.parent().context("invalid SDK payload")?)?;
            std::fs::copy(snapshot.path().join(file.path), to)?;
        }
        if let Err(error) = std::fs::rename(staged.path(), &destination)
            && !destination.exists()
        {
            return Err(error.into());
        }
        private_directory(&destination)?;
    }
    verify_selected(&destination, selection, arch)?;
    Ok(destination)
}

fn verify_selected(path: &Path, selected: &SdkSelection, arch: &str) -> Result<()> {
    let verified = runtime_bundle::verify_bundle_with(
        path,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(selected.publisher.clone()),
            require_signature: true,
            require_arch: Some(arch.to_owned()),
        },
    )?;
    if verified.root_hash != selected.root {
        bail!("SDK cache differs from independent selection");
    }
    Ok(())
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: observing this process's effective UID has no side effects.
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("SDK compiler cache must be an owned directory, never a link");
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<()> {
    bail!("native SDK compiler consumption requires a Unix host");
}

pub(crate) fn declared_resources(
    selection: &SdkSelection,
    artifacts: &HashMap<String, PathBuf>,
    app: &Path,
    triple: Option<&str>,
) -> Result<Vec<String>> {
    let root = crate::find_repo_root(app).context("SDK checkout is required")?;
    let inputs = checked_inputs(selection, &root, app, triple)?;
    let source = artifacts
        .get("sdk:authored-source")
        .context("compiled authored source is unavailable")?;
    let mut kinds = crate::resource_kinds::declared_in(source)?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for kind in inputs.interface.resources {
        if kind.is_empty() || kind.len() > 256 || kind.chars().any(char::is_control) {
            bail!("invalid signed SDK resource declaration");
        }
        kinds.insert(kind);
    }
    if kinds.len() > 4096 {
        bail!("signed SDK resource declarations exceed their bound");
    }
    Ok(kinds.into_iter().collect())
}

fn sha256(path: &Path) -> Result<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hash = sha2::Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let length = std::io::Read::read(&mut file, &mut buffer)?;
        if length == 0 {
            break;
        }
        hash.update(buffer.get(..length).context("invalid hash read length")?);
    }
    Ok(hex::encode(hash.finalize()))
}

fn stage_manifest(spec: &AppPackageSpec, interface: &Interface, directory: &Path) -> Result<()> {
    let mut cargo: toml::Value =
        toml::from_str(&std::fs::read_to_string(spec.app_dir.join("Cargo.toml"))?)?;
    validate_dependencies(&cargo, interface)?;
    let root = crate::find_repo_root(&spec.app_dir).context("SDK checkout is required")?;
    let workspace: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let table = cargo
        .as_table_mut()
        .context("invalid authored Cargo manifest")?;
    table.remove("dependencies");
    table.remove("dev-dependencies");
    table.remove("build-dependencies");
    table.remove("target");
    table.insert(
        "workspace".into(),
        workspace
            .get("workspace")
            .context("SDK workspace is unavailable")?
            .clone(),
    );
    let staging_workspace = table
        .get_mut("workspace")
        .context("staged workspace is unavailable")?
        .as_table_mut()
        .context("invalid workspace")?;
    staging_workspace.remove("members");
    staging_workspace.remove("default-members");
    staging_workspace.remove("dependencies");
    let mut profiles = workspace
        .get("profile")
        .context("SDK build profiles are unavailable")?
        .clone();
    for (_, profile) in profiles
        .as_table_mut()
        .context("invalid SDK profiles")?
        .iter_mut()
    {
        if let Some(profile) = profile.as_table_mut() {
            profile.remove("package");
        }
    }
    table.insert("profile".into(), profiles);
    let features = table
        .entry("features")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::default()))
        .as_table_mut()
        .context("invalid app features")?;
    for (_, values) in features.iter_mut() {
        let entries = values.as_array_mut().context("invalid app feature")?;
        entries.retain(|v| !v.as_str().is_some_and(|s| s.starts_with("dep:")));
        if entries
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.contains('/')))
        {
            bail!("SDK consumer features cannot reconfigure compiled dependencies");
        }
    }
    features.insert("thin".into(), toml::Value::Array(Vec::new()));
    copy_source(&spec.app_dir, directory, directory)?;
    std::fs::write(directory.join("Cargo.toml"), toml::to_string(&cargo)?)?;
    let name = &spec.cargo_package;
    let version = cargo
        .get("package")
        .and_then(|package| package.get("version"))
        .context("app package version is missing")?
        .as_str()
        .context("app version must be explicit")?;
    std::fs::write(
        directory.join("Cargo.lock"),
        format!("version = 4\n\n[[package]]\nname = {name:?}\nversion = {version:?}\n"),
    )?;
    Ok(())
}

fn validate_dependencies(cargo: &toml::Value, interface: &Interface) -> Result<()> {
    validate_source_paths(cargo)?;
    let mut dependencies = Vec::new();
    if let Some(table) = cargo.get("dependencies").and_then(toml::Value::as_table) {
        dependencies.extend(table.iter());
    }
    if let Some(targets) = cargo.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            if let Some(table) = target.get("dependencies").and_then(toml::Value::as_table) {
                dependencies.extend(table.iter());
            }
        }
    }
    for (alias, value) in dependencies {
        let package = value
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(alias);
        if !["tilo-app", "runtime-dylib", "workspace-hack"].contains(&package) {
            bail!("dependency {alias} is outside this signed SDK compiler interface");
        }
        if let Some(features) = value.get("features").and_then(toml::Value::as_array) {
            let unit = interface
                .units
                .iter()
                .find(|u| u.target == "tilo_app")
                .context("public SDK unit is unavailable")?;
            if package != "tilo-app"
                || features.iter().any(|f| {
                    !f.as_str()
                        .is_some_and(|f| unit.features.iter().any(|have| have == f))
                })
            {
                bail!("requested dependency features differ from compiled SDK");
            }
        }
    }
    if cargo
        .get("package")
        .and_then(|p| p.get("build"))
        .is_some_and(|build| build.as_bool() != Some(false))
    {
        bail!("SDK-only authoring does not execute application build scripts");
    }
    if cargo.get("build-dependencies").is_some() {
        bail!("SDK-only authoring does not compile build-script dependencies");
    }
    Ok(())
}

fn validate_source_paths(cargo: &toml::Value) -> Result<()> {
    let lib = cargo.get("lib").into_iter();
    let targets = ["bin", "example", "test", "bench"]
        .into_iter()
        .flat_map(|kind| {
            cargo
                .get(kind)
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
        });
    for target in lib.chain(targets) {
        if let Some(path) = target.get("path").and_then(toml::Value::as_str) {
            let path = Path::new(path);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                bail!("authored compiler targets must stay inside the verified source snapshot");
            }
        }
    }
    Ok(())
}

fn copy_source(source: &Path, destination: &Path, stage: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if path == stage
            || ["target", ".git", "Cargo.toml", "Cargo.lock"]
                .iter()
                .any(|n| name == *n)
        {
            continue;
        }
        if name.to_string_lossy().starts_with(".tilo-sdk-build-") {
            continue;
        }
        let to = destination.join(&name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir(&to)?;
            copy_source(&path, &to, stage)?;
        } else if entry.file_type()?.is_file() {
            if name == "build.rs" {
                bail!("SDK-only authoring does not execute application build scripts");
            }
            std::fs::copy(path, to)?;
        } else {
            bail!("SDK authoring source must contain regular files and directories");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
