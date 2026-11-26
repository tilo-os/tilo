//! An app's model module: the `model.wasm` its package carries.
//!
//! Every device that holds a workspace's keys applies an installed app's
//! actions from these bytes, with no build of its own (`tilo-model-engine`,
//! `crates/sdk/tilo-model-guest`). The app keeps its model as a guest crate
//! beside it, `apps/remote` → `apps/remote-model-wasm`, which declares its entry
//! with `tilo_model_guest::model!`, and [`build`] builds that crate alone for
//! `wasm32-unknown-unknown` in the release profile the guest crate sizes for
//! the wire, then optimizes the module with `wasm-opt`: a whole module, which
//! any device calls as it is. That is what `tilolang package` stages as
//! [`MODEL_MODULE`], for an app built on its own.
//!
//! The guest crates are workspace members, so every app's module shares one
//! lockfile and one wasm32 target directory: a dependency compiles once for
//! every app's module, not once per app.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

pub use tilo_app_package::MODEL_MODULE;

/// The directory under the target directory a built module is kept in, per app.
const MODULES: &str = "app-models";

/// Where the modules' cargo target directory lives under the target
/// directory, shared by every app: the module is the only thing built for
/// wasm32, and its build never shares a lock with the native builds.
const BUILDS: &str = "app-model-build";

/// The `wasm-opt` arguments every module this repository ships is optimized
/// with, embedded when this tool compiles (tools/web/wasm-opt-flags): the
/// module a size check measures is the module a package carries.
const WASM_OPT_FLAGS: &str = include_str!("../../../../tools/web/wasm-opt-flags");

/// What the module's release build sets beyond the workspace's release
/// profile: the same bytes reach every key-holding device, so the build is
/// sized for the wire rather than for the machine that packages it. Fat LTO,
/// one codegen unit and no overflow checks come from `release`; these are
/// what a module that is never a process skips (unwinding, symbols, debug
/// info) and the size its own crates are compiled for.
const PROFILE: [(&str, &str); 4] = [
    ("CARGO_PROFILE_RELEASE_OPT_LEVEL", "z"),
    ("CARGO_PROFILE_RELEASE_DEBUG", "false"),
    ("CARGO_PROFILE_RELEASE_PANIC", "abort"),
    ("CARGO_PROFILE_RELEASE_STRIP", "symbols"),
];

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
    let (package, lib) = names(&manifest)?;
    let app = app_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("app directory {} has no name", app_dir.display()))?;
    let target = target_dir(app_dir);
    let build = target.join(BUILDS);
    eprintln!("[tilolang] building {app}'s model module");
    // The guest crate's own `.cargo/config.toml` carries what makes a module a
    // module rather than a page (no JavaScript glue, no browser clocks), so
    // cargo starts in the crate's directory and reads it from there.
    let mut command = Command::new(crate::targets::cargo_program());
    // The module is what the guest crate's own release profile says: short and
    // stripped. Release-profile overrides a caller exports for the app's native
    // build (`CARGO_PROFILE_RELEASE_LTO`, `…_CODEGEN_UNITS`) would otherwise
    // silently change the bytes every device applies actions with, so the same
    // source would give a different module depending on who built it.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_PROFILE_RELEASE_") {
            command.env_remove(&key);
        }
    }
    let status = command
        .arg("build")
        .arg("--locked")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("-p")
        .arg(&package)
        .arg("--target-dir")
        .arg(&build)
        .envs(PROFILE)
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
    optimize(&module)?;
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

/// The guest crate beside an app directory: `apps/remote` →
/// `apps/remote-model-wasm`, when it is there.
#[must_use]
pub fn guest_crate(app_dir: &Path) -> Option<PathBuf> {
    let name = app_dir.file_name()?.to_str()?;
    let guest = app_dir.with_file_name(format!("{name}{GUEST}"));
    guest.join("Cargo.toml").is_file().then_some(guest)
}

/// The crate's `[package].name`, which `cargo build -p` selects, and its
/// `[lib].name`, or the package name with the dashes of a crate name: what the
/// module it builds is called on disk.
fn names(manifest: &Path) -> Result<(String, String)> {
    let source = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let table: toml::Table = source
        .parse()
        .with_context(|| format!("parsing {}", manifest.display()))?;
    let package = table
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{} has no [package].name", manifest.display()))?
        .to_owned();
    let lib = table
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .map_or_else(|| package.replace('-', "_"), str::to_owned);
    Ok((package, lib))
}

/// Optimize the built module in place with `wasm-opt`, the same arguments the
/// page's modules are built with (tools/web/wasm-opt-flags).
fn optimize(module: &Path) -> Result<()> {
    let flags: Vec<&str> = WASM_OPT_FLAGS
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .collect();
    let optimized = module.with_extension("optimized.wasm");
    let status = Command::new("wasm-opt")
        .args(&flags)
        .arg(module)
        .arg("-o")
        .arg(&optimized)
        .status()
        .with_context(
            || "running wasm-opt (binaryen); it builds every module this repository ships",
        )?;
    if !status.success() {
        bail!("wasm-opt failed with {status} on {}", module.display());
    }
    std::fs::rename(&optimized, module).with_context(|| format!("writing {}", module.display()))
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
