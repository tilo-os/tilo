//! Bind native packages to an independently selected, fully verified SDK.
//! A signed SDK name is never a substitute for compiler or ELF evidence.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub mod compiler;
use launcher::loader_preflight::{read_dynamic_section, read_interpreter};

/// The authenticated SDK identity selected by the developer, not by its manifest.
#[derive(Debug, Clone)]
pub struct SdkSelection {
    /// Optional separately signed compiler interface for exact artifact reuse.
    pub compiler: Option<compiler::CompilerSelection>,
    /// Directory containing the canonical signed bundle and its complete payload.
    pub bundle: PathBuf,
    /// Independently obtained expected publisher public key.
    pub publisher: String,
    /// Independently obtained expected content root.
    pub root: String,
}

pub(crate) fn validate_request(request: &super::PackageRequest) -> Result<()> {
    if request.sdk.is_some()
        && (request.platform != super::PackagePlatform::Tilo || request.static_link)
    {
        bail!("--sdk-bundle is only for native thin Tilo packages");
    }
    Ok(())
}

/// Resolve all checkout apps in the canonical SDK graph, while compiling only
/// the selected app binaries. Reuse the production selective compiler wrapper.
pub(crate) fn configure_graph(
    command: &mut Command,
    root: &Path,
    profile: &str,
    selected: &str,
    requested: &[String],
) -> Result<Vec<String>> {
    let mut features = Vec::new();
    let mut bins = Vec::new();
    let mut found = false;
    let mut directories =
        std::fs::read_dir(root.join("apps"))?.collect::<std::io::Result<Vec<_>>>()?;
    directories.sort_by_key(std::fs::DirEntry::file_name);
    for directory in directories {
        let path = directory.path();
        if !path.join("tilo-app.toml").is_file() {
            continue;
        }
        let cargo: toml::Value =
            toml::from_str(&std::fs::read_to_string(path.join("Cargo.toml"))?)?;
        let name = cargo
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .context("SDK app has no Cargo package name")?;
        let manifest = tilo_app_package::Manifest::from_toml_str(&std::fs::read_to_string(
            path.join("tilo-app.toml"),
        )?)?;
        let entry = manifest
            .entrypoint
            .context("SDK app has no native entrypoint")?;
        let bin = Path::new(&entry.bin)
            .file_name()
            .and_then(|s| s.to_str())
            .context("SDK app entrypoint has no binary name")?;
        found |= name == selected;
        command.arg("-p").arg(name);
        features.push(format!("{name}/thin"));
        bins.push(bin.replace('-', "_"));
        command.arg("--config").arg(format!(
            "profile.{profile}.package.\"{name}\".opt-level=\"z\""
        ));
        command.arg("--config").arg(format!(
            "profile.{profile}.package.\"{name}\".codegen-units=1"
        ));
        command.arg("--config").arg(format!(
            "profile.{profile}.package.\"{name}\".strip=\"symbols\""
        ));
    }
    if !found {
        bail!("the selected app is outside the canonical checkout SDK graph");
    }
    command
        .arg("--locked")
        .arg("-p")
        .arg("runtime-dylib")
        .arg("--config")
        .arg(format!(
            "build.rustc-workspace-wrapper={:?}",
            root.join("tools/ci/rustc-thin-app-wrapper.sh")
                .to_string_lossy()
        ))
        .arg("--config")
        .arg(format!("env.TILO_THIN_BIN_CRATES={:?}", bins.join(":")));
    for name in [
        "CODEGEN_UNITS",
        "LTO",
        "OPT_LEVEL",
        "PANIC",
        "STRIP",
        "DEBUG",
    ] {
        command.env_remove(format!("CARGO_PROFILE_RELEASE_{name}"));
    }
    features.extend(requested.iter().map(|f| format!("{selected}/{f}")));
    Ok(features)
}

pub(crate) fn collect_toolkit(
    line: &str,
    app_dir: &Path,
    artifacts: &mut HashMap<String, PathBuf>,
) -> Result<()> {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(());
    };
    if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact")
        || message
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(serde_json::Value::as_str)
            != Some("runtime_dylib")
    {
        return Ok(());
    }
    let root = super::find_repo_root(app_dir).context("missing SDK checkout")?;
    let manifest = message
        .get("manifest_path")
        .and_then(serde_json::Value::as_str)
        .context("toolkit artifact has no manifest")?;
    if Path::new(manifest).canonicalize()?
        != root
            .join("crates/runtime/runtime-dylib/Cargo.toml")
            .canonicalize()?
    {
        bail!("toolkit artifact came from another SDK checkout");
    }
    for file in message
        .get("filenames")
        .and_then(serde_json::Value::as_array)
        .context("toolkit artifact has no filenames")?
    {
        let path = PathBuf::from(file.as_str().context("invalid toolkit artifact path")?);
        if path
            .file_name()
            .is_some_and(|name| name == "libruntime_dylib.so")
            && let Some(previous) = artifacts.insert("sdk:runtime_dylib".into(), path.clone())
            && previous != path
        {
            bail!("compiler reported ambiguous toolkit artifacts");
        }
    }
    Ok(())
}

pub(crate) fn bind_staged(
    selection: &SdkSelection,
    mut artifacts: HashMap<String, PathBuf>,
    staged: (&Path, &[(String, PathBuf)]),
    target: (&Path, Option<&str>),
) -> Result<String> {
    for (bin, path) in staged.1 {
        artifacts.insert(bin.clone(), staged.0.join(path));
    }
    bind(selection, &artifacts, staged.1, target.0, target.1)
}

/// Verify actual compiler output before assigning any native runtime name.
pub(crate) fn bind(
    selection: &SdkSelection,
    artifacts: &HashMap<String, PathBuf>,
    entrypoints: &[(String, PathBuf)],
    app_dir: &Path,
    triple: Option<&str>,
) -> Result<String> {
    let arch = match triple {
        Some(t) if t.starts_with("x86_64-") => "x86_64",
        Some(t) if t.starts_with("aarch64-") => "aarch64",
        None => std::env::consts::ARCH,
        _ => bail!("unsupported native SDK target"),
    };
    let bundle = if selection.bundle.is_absolute() {
        selection.bundle.clone()
    } else {
        app_dir.join(&selection.bundle)
    };
    let (snapshot, manifest) = verified_snapshot(selection, &bundle, arch)?;
    let bundle = snapshot.path();
    let (release, _) = runtime_bundle::split_abi_version(&manifest.version)
        .context("selected SDK has no native content fingerprint; build a paired native SDK")?;
    let authored = tilo_app_package::Manifest::from_toml_str(&std::fs::read_to_string(
        app_dir.join("tilo-app.toml"),
    )?)?;
    let expected = format!("tilo-runtime/{release}");
    if authored.package.runtime.as_deref() != Some(expected.as_str())
        && authored.package.runtime.as_deref()
            != Some(format!("tilo-runtime/{}", manifest.version).as_str())
    {
        bail!("authored runtime release differs from the selected native SDK");
    }
    let toolkit = artifacts
        .get("sdk:runtime_dylib")
        .context("Cargo did not report the actual linked toolkit artifact")?;
    equal_file(toolkit, &bundle.join("lib/libruntime_dylib.so"))
        .context("compiled toolkit differs from selected SDK; build/update the paired SDK")?;
    let std_dir = compiler_std_dir(app_dir, triple)?;
    let toolkit_dynamic =
        read_dynamic_section(toolkit)?.context("uninspectable compiled toolkit ELF")?;
    let std_names = toolkit_dynamic
        .needed
        .iter()
        .filter(|n| n.starts_with("libstd-") && Path::new(n).extension().is_some_and(|e| e == "so"))
        .collect::<Vec<_>>();
    if std_names.len() != 1 {
        bail!("compiled toolkit must identify exactly one Rust standard library");
    }
    for name in std_names {
        equal_file(&std_dir.join(name), &bundle.join("lib").join(name))
            .context("compiler Rust standard library differs from selected SDK")?;
    }
    let mut entries = Vec::new();
    for (bin, _) in entrypoints {
        let path = artifacts
            .get(bin)
            .context("missing compiled app entrypoint")?;
        let dynamic = read_dynamic_section(path)?.context("uninspectable app ELF")?;
        if !dynamic
            .needed
            .iter()
            .any(|name| name == "libruntime_dylib.so")
        {
            bail!("compiled app does not link the selected toolkit");
        }
        for name in dynamic.needed.iter().filter(|n| {
            n.starts_with("libstd-") && Path::new(n).extension().is_some_and(|e| e == "so")
        }) {
            equal_file(&std_dir.join(name), &bundle.join("lib").join(name))
                .context("compiler Rust standard library differs from selected SDK")?;
        }
        entries.push(path.clone());
    }
    verify_linkage(bundle, entries, arch)?;
    // Recheck the entire signed payload after ELF inspection, not just metadata.
    let after = runtime_bundle::verify_bundle_with(
        bundle,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(selection.publisher.clone()),
            require_signature: true,
            require_arch: Some(arch.to_owned()),
        },
    )?;
    if after.root_hash != manifest.root_hash || after.version != manifest.version {
        bail!("selected SDK changed during package admission");
    }
    Ok(format!("tilo-runtime/{}", manifest.version))
}

fn verified_snapshot(
    selection: &SdkSelection,
    source: &Path,
    arch: &str,
) -> Result<(tempfile::TempDir, runtime_bundle::BundleManifest)> {
    let policy = runtime_bundle::VerifyOptions {
        require_publisher: Some(selection.publisher.clone()),
        require_signature: true,
        require_arch: Some(arch.to_owned()),
    };
    let manifest = runtime_bundle::verify_bundle_with(source, &policy)
        .context("verifying independently selected SDK")?;
    if manifest.root_hash != selection.root {
        bail!("selected SDK payload root differs from its authenticated receipt");
    }
    let snapshot = tempfile::Builder::new()
        .prefix("tilo-package-sdk-")
        .tempdir()?;
    std::fs::copy(
        source.join("bundle.toml"),
        snapshot.path().join("bundle.toml"),
    )?;
    for file in &manifest.files {
        let destination = snapshot.path().join(&file.path);
        std::fs::create_dir_all(destination.parent().context("SDK file has no parent")?)?;
        std::fs::copy(source.join(&file.path), destination)?;
    }
    let copied = runtime_bundle::verify_bundle_with(snapshot.path(), &policy)?;
    if copied.root_hash != manifest.root_hash || copied.version != manifest.version {
        bail!("selected SDK changed while taking its verified snapshot");
    }
    Ok((snapshot, copied))
}

fn compiler_std_dir(app_dir: &Path, triple: Option<&str>) -> Result<PathBuf> {
    let mut command = Command::new("rustc");
    command
        .current_dir(app_dir)
        .arg("--print")
        .arg("target-libdir");
    if let Some(triple) = triple {
        command.arg("--target").arg(triple);
    }
    let output = command.output()?;
    if !output.status.success() {
        bail!("cannot inspect compiler Rust standard library");
    }
    Ok(PathBuf::from(std::str::from_utf8(&output.stdout)?.trim()))
}

fn hash(path: &Path) -> Result<blake3::Hash> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize())
}

fn equal_file(compiled: &Path, selected: &Path) -> Result<()> {
    if hash(compiled)? != hash(selected)? {
        bail!("compiled SDK bytes do not match the verified payload");
    }
    Ok(())
}

fn native(path: &Path, arch: &str) -> Result<()> {
    let mut header = [0; 20];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    let machine = u16::from_le_bytes([
        *header.get(18).context("missing ELF machine")?,
        *header.get(19).context("missing ELF machine")?,
    ]);
    let expected = match arch {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => bail!("unsupported native architecture"),
    };
    if !header.starts_with(b"\x7fELF\x02\x01") || machine != expected {
        bail!("native SDK closure has the wrong ELF architecture or format");
    }
    Ok(())
}

fn verify_linkage(bundle: &Path, entries: Vec<PathBuf>, arch: &str) -> Result<()> {
    let mut pending: VecDeque<_> = entries.into();
    let mut objects = BTreeSet::new();
    while let Some(path) = pending.pop_front() {
        if !objects.insert(path.clone()) {
            continue;
        }
        if objects.len() > 1024 {
            bail!("native SDK dependency closure exceeds its bound");
        }
        native(&path, arch)?;
        let dynamic =
            read_dynamic_section(&path)?.context("native SDK contains an uninspectable ELF")?;
        if !dynamic.rpath.is_empty() {
            bail!("native SDK linkage may not override the verified bundle with RPATH");
        }
        if let Some(interpreter) = read_interpreter(&path)? {
            let name = Path::new(&interpreter)
                .file_name()
                .context("invalid ELF interpreter")?;
            pending.push_back(bundle.join("lib").join(name));
        }
        for name in dynamic.needed {
            if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
                bail!("unsafe native SDK dependency name");
            }
            pending.push_back(bundle.join("lib").join(name));
        }
    }
    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    for object in objects {
        symbols(&object, &mut exports, &mut imports)?;
    }
    if let Some(missing) = imports.difference(&exports).next() {
        bail!(
            "compiled native app/SDK has an unresolved dynamic import {missing}; build the matching paired SDK"
        );
    }
    Ok(())
}

fn symbols(
    path: &Path,
    exports: &mut BTreeSet<String>,
    imports: &mut BTreeSet<String>,
) -> Result<()> {
    let mut child = Command::new("readelf")
        .args(["--wide", "--dyn-syms"])
        .arg(path)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("inspecting ELF imports without executing application code")?;
    let stdout = child.stdout.take().context("readelf has no output pipe")?;
    let result = parse_symbols(BufReader::new(stdout), exports, imports);
    let stopped = if result.is_err() {
        child.kill()
    } else {
        Ok(())
    };
    let status = child.wait()?;
    stopped?;
    result?;
    if !status.success() {
        bail!("cannot inspect native SDK dynamic symbols");
    }
    Ok(())
}

fn parse_symbols(
    mut reader: impl BufRead,
    exports: &mut BTreeSet<String>,
    imports: &mut BTreeSet<String>,
) -> Result<()> {
    let mut bytes = 0usize;
    let mut table = false;
    loop {
        let mut line = String::new();
        let read = (&mut reader).take(16 * 1024 + 1).read_line(&mut line)?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read);
        if read > 16 * 1024 || bytes > 128 * 1024 * 1024 {
            bail!("ELF symbol output exceeds its bound");
        }
        if line.starts_with("Symbol table '.dynsym'") {
            table = true;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let [number, _, _, _, binding, _, section, name, ..] = fields.as_slice() else {
            continue;
        };
        if number
            .strip_suffix(':')
            .and_then(|n| n.parse::<u64>().ok())
            .is_none()
            || !matches!(*binding, "GLOBAL" | "UNIQUE" | "WEAK")
        {
            continue;
        }
        if fields.len() > 9
            || fields.get(8).is_some_and(|version| {
                version
                    .strip_prefix('(')
                    .and_then(|s| s.strip_suffix(')'))
                    .and_then(|s| s.parse::<u64>().ok())
                    .is_none()
            })
        {
            bail!("ELF symbol name or version cannot be inspected unambiguously");
        }
        if *section == "UND" {
            if *binding != "WEAK" {
                imports.insert((*name).to_owned());
            }
        } else {
            exports.insert(name.replace("@@", "@"));
            if let Some((base, _)) = name.split_once("@@") {
                exports.insert(base.to_owned());
            }
        }
        if exports.len().saturating_add(imports.len()) > 2_000_000 {
            bail!("ELF symbol set exceeds its bound");
        }
    }
    if !table {
        bail!("ELF dynamic symbol table could not be observed");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
