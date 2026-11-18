//! An app's model module: the `model.wasm` its package carries.
//!
//! Every device that holds a workspace's keys applies an installed app's
//! actions from these bytes, with no build of its own (`tilo-model-engine`,
//! `crates/sdk/tilo-model-guest`). One mechanism builds it for every app: the
//! app keeps its model as a guest crate beside it, `apps/tasks` →
//! `apps/tasks-model-wasm`, which exports the model ABI over the app's
//! resource, and [`build`] builds that crate for `wasm32-unknown-unknown`.
//! Packaging writes the result into the payload as [`MODEL_MODULE`]; a tree
//! keeps no built module, so `t`'s `app-models` check builds them for the checks
//! that read one.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

pub use tilo_app_package::MODEL_MODULE;

/// The directory under the target directory a built module is kept in, per app.
const MODULES: &str = "app-models";

/// Where each guest crate's own cargo target directory lives under the target
/// directory, per app: the module is the only thing built for wasm32, and its
/// build never shares a lock with the app's native builds.
const BUILDS: &str = "app-model-build";

/// The suffix of the guest crate that sits beside its app.
const GUEST: &str = "-model-wasm";

/// Resolve the app directory `start` names or sits inside, then build the app's
/// model module, answering where it is; `Ok(None)` when the app keeps no model.
///
/// # Errors
///
/// When the guest crate's build fails or Cargo reports no module.
pub fn build_app(start: &Path) -> Result<Option<PathBuf>> {
    build(&crate::resolve_app_dir(start)?)
}

/// Build the model module of the app at `app_dir`, answering where it is;
/// `Ok(None)` when the app has no guest crate beside it.
///
/// # Errors
///
/// When the guest crate's manifest cannot be read, its build fails, or the
/// module it should have written is not there.
pub fn build(app_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(guest) = guest_crate(app_dir) else {
        return Ok(None);
    };
    let manifest = guest.join("Cargo.toml");
    let lib = lib_name(&manifest)?;
    let app = app_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("app directory {} has no name", app_dir.display()))?;
    let target = target_dir(app_dir);
    let build = target.join(BUILDS).join(app);
    eprintln!("[tilo-app] building {app}'s model module");
    // The guest crate's own `.cargo/config.toml` carries what makes a module a
    // module rather than a page (no JavaScript glue, no browser clocks), so
    // cargo starts in the crate's directory and reads it from there.
    let status = Command::new(crate::targets::cargo_program())
        .arg("build")
        .arg("--locked")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&build)
        .current_dir(&guest)
        .status()
        .with_context(|| format!("building {}", manifest.display()))?;
    if !status.success() {
        bail!(
            "{}: the model build failed with {status}",
            manifest.display()
        );
    }
    let module = build
        .join("wasm32-unknown-unknown/release")
        .join(format!("{lib}.wasm"));
    if !module.is_file() {
        bail!("the model build reported no module at {}", module.display());
    }
    let output = target.join(MODULES).join(app).join(MODEL_MODULE);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let staged = output.with_extension("staging");
    std::fs::copy(&module, &staged)
        .with_context(|| format!("copying {} to {}", module.display(), staged.display()))?;
    std::fs::rename(&staged, &output).with_context(|| format!("writing {}", output.display()))?;
    Ok(Some(output))
}

/// The guest crate beside an app directory: `apps/tasks` →
/// `apps/tasks-model-wasm`, when it is there.
#[must_use]
pub fn guest_crate(app_dir: &Path) -> Option<PathBuf> {
    let name = app_dir.file_name()?.to_str()?;
    let guest = app_dir.with_file_name(format!("{name}{GUEST}"));
    guest.join("Cargo.toml").is_file().then_some(guest)
}

/// The crate's `[lib].name`, or its package name with the dashes of a crate
/// name: what the module it builds is called on disk.
fn lib_name(manifest: &Path) -> Result<String> {
    let source = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let table: toml::Table = source
        .parse()
        .with_context(|| format!("parsing {}", manifest.display()))?;
    if let Some(lib) = table
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
    {
        return Ok(lib.to_owned());
    }
    table
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(|name| name.replace('-', "_"))
        .ok_or_else(|| anyhow::anyhow!("{} has no [package].name", manifest.display()))
}

/// The cargo target directory `app_dir` builds into: the one the environment
/// names, else the nearest workspace root's `target/`, else the app's own.
fn target_dir(app_dir: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    for dir in app_dir.ancestors() {
        let manifest = dir.join("Cargo.toml");
        if let Ok(source) = std::fs::read_to_string(&manifest)
            && source.contains("[workspace]")
        {
            return dir.join("target");
        }
    }
    app_dir.join("target")
}
