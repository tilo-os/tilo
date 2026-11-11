//! The model modules a host test runs: each app's model crate built for
//! `wasm32-unknown-unknown`, in this crate's `OUT_DIR`.
//!
//! `docs/architecture/apps.md` §1 makes an app's model and verbs a WebAssembly module the
//! document host runs. A test that proved the host on a linked-in model would prove nothing
//! about that, so the fixture is the app's own crate, compiled the way an app's module is.
//!
//! The cargo that builds it has a target directory of its own beside `OUT_DIR`: a build script
//! that shared the outer run's target directory would ask cargo to build while cargo is
//! building, and the two would wait on each other. `OUT_DIR` keeps the module alone, because that
//! is the tree a build's compiler cache keeps of a build script's run, and a target directory in
//! it is far larger than one run may be to keep: t refused every run with "its `OUT_DIR` ... is
//! too large to key", so no build ever kept this one, and every build started that cargo again
//! for a module a build without tests never reads.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The app model crates a host test runs, each built as the module an app ships: Tasks, Tube
/// and Console, the apps the storage migration has moved (`one-app-model.md` §6).
const MODELS: &[&str] = &["tasks-model", "tube-model", "console-model"];

fn main() -> Result<(), String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR for build scripts")?);
    // The crate reads this build's `OUT_DIR` to find the modules, or nothing when there are
    // none; it is set before any path that might return.
    println!("cargo::rustc-env=TILO_MODEL_FIXTURE_DIR={}", out.display());
    println!("cargo::rerun-if-env-changed=TILO_MODEL_FIXTURE_OFFLINE");
    // The guest crate the modules are built with, and the ABI they are built against: a module
    // of an older guest is not the module this host reads.
    let shared = [
        manifest.join("../tilo-model-guest/src"),
        manifest.join("../tilo-model-guest/Cargo.toml"),
        manifest.join("../tilo-model-abi/src"),
    ];
    // Where the modules are built: beside `OUT_DIR`, in this build's own directory, never in the
    // tree the cache keeps (the module's documentation above says why).
    let target = out
        .parent()
        .ok_or("cargo's OUT_DIR lies in a build directory")?
        .join("model-target");
    // A build before this kept its target directory in `OUT_DIR`; nothing reads it now, and its
    // size is what kept this run out of every cache.
    let _ = std::fs::remove_dir_all(out.join("target"));
    // Every crate of this repository the models link: their sources are inputs of the module as
    // much as the models' own, and a build script that does not name a file is never rerun for
    // it — a change in one would leave the module built from the sources before it.
    let linked = linked_crates(&manifest);
    let mut common: Vec<PathBuf> = shared.to_vec();
    common.extend(linked.iter().cloned());
    // Every source the module is built from: cargo watches a directory's own entry only, so a
    // file changed under it is not seen. What every model shares is named once.
    for watched in &common {
        println!("cargo::rerun-if-changed={}", watched.display());
    }
    for model_crate in MODELS {
        let model = manifest.join("../../../apps").join(model_crate);
        let module = out.join(format!("{}.wasm", model_crate.replace('-', "_")));
        let mut sources = common.clone();
        sources.push(model.join("src"));
        sources.push(model.join("Cargo.toml"));
        println!("cargo::rerun-if-changed={}", module.display());
        if sources.iter().all(|source| fresh(&module, source)) {
            continue;
        }
        if let Err(error) = build(&model, model_crate, &target, &module) {
            // A host that cannot be tested without a module still builds; the test that needs
            // one says so itself, rather than every build of this crate failing on a wasm
            // toolchain.
            println!("cargo::warning=the {model_crate} module fixture was not built: {error}");
        }
    }
    Ok(())
}

/// The sources of every crate of this repository the models link, as cargo names them: each
/// crate's `src` and its manifest, from the union of the models' dependency graphs.
fn linked_crates(manifest: &Path) -> BTreeSet<PathBuf> {
    let repository = manifest.join("../../..");
    let mut sources = BTreeSet::new();
    for model_crate in MODELS {
        let model = repository.join("apps").join(model_crate);
        for crate_dir in tree_paths(&model).unwrap_or_default() {
            if !crate_dir.starts_with(&repository) {
                continue;
            }
            sources.insert(crate_dir.join("src"));
            sources.insert(crate_dir.join("Cargo.toml"));
        }
    }
    sources
}

/// Where the crates `model` links live, from `cargo tree`: each line names a package by its
/// path in parentheses when it comes from this repository.
fn tree_paths(model: &Path) -> Result<Vec<PathBuf>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .arg("tree")
        .arg("--manifest-path")
        .arg(model.join("Cargo.toml"))
        .args(["--edges", "normal", "--prefix", "none"]);
    if std::env::var_os("TILO_MODEL_FIXTURE_OFFLINE").is_some()
        || std::env::var_os("CARGO_NET_OFFLINE").is_some_and(|value| value == "true")
    {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run cargo tree: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let listed = String::from_utf8_lossy(&output.stdout);
    Ok(listed
        .lines()
        .filter_map(|line| line.split_once(" (/"))
        .filter_map(|(_, rest)| rest.split_once(')'))
        .map(|(path, _)| PathBuf::from(path))
        .collect())
}

/// Build `model` for `wasm32-unknown-unknown` into `target`, and copy the module to `module`.
fn build(model: &Path, name: &str, target: &Path, module: &Path) -> Result<(), String> {
    // A toolchain that can build a module, not whatever `cargo` is first on PATH: a wrapper may
    // hand the build to another machine, which cannot build one here, and a system Rust may
    // have no wasm32 target installed.
    let rustc = module_toolchain().ok_or_else(|| {
        "no Rust toolchain here has the wasm32-unknown-unknown target (rustup target add \
         wasm32-unknown-unknown)"
            .to_owned()
    })?;
    let cargo = std::path::Path::new(&rustc).join("bin").join("cargo");
    let mut command = Command::new(&cargo);
    // The cargo's own `rustc`, not the one this build found on `PATH`: a system Rust may be a
    // different toolchain, without the target.
    command.env(
        "RUSTC",
        std::path::Path::new(&rustc).join("bin").join("rustc"),
    );
    command
        .arg("build")
        .arg("--package")
        .arg(name)
        // The module's own side: its export and the host imports under it, which the crate
        // leaves out of a build an app links for its types.
        .arg("--features")
        .arg("module")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--target-dir")
        .arg(target)
        .current_dir(model)
        // The module's entropy is the host's, through the import `tilo-model-guest` answers:
        // getrandom's custom backend, as the host that runs the module provides it. The build's
        // own flags stay: the cfg's are added to them, not put in their place.
        .env("CARGO_ENCODED_RUSTFLAGS", module_flags())
        .env_remove("CARGO_TARGET_DIR");
    if std::env::var_os("TILO_MODEL_FIXTURE_OFFLINE").is_some()
        || std::env::var_os("CARGO_NET_OFFLINE").is_some_and(|value| value == "true")
    {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    if !output.status.success() {
        // Only the errors: a failed build's stderr is mostly the workspace's warnings.
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The errors, and what the linker says after them: a link failure's cause is in its
        // notes, not its first line.
        let errors: Vec<&str> = stderr
            .lines()
            .filter(|line| {
                line.starts_with("error")
                    || line.contains(": error")
                    || line.contains("undefined symbol")
                    || line.contains("rust-lld:")
            })
            .collect();
        // The errors, or, when none names itself, the tail of what the tool said.
        return Err(if errors.is_empty() {
            stderr.lines().rev().take(12).collect::<Vec<_>>().join("\n")
        } else {
            errors.join("\n")
        });
    }
    let wasm_name = format!("{}.wasm", name.replace('-', "_"));
    let built = target
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(&wasm_name);
    std::fs::copy(&built, module)
        .map_err(|error| format!("could not copy {}: {error}", built.display()))?;
    Ok(())
}

/// The sysroot of a toolchain that can build `wasm32-unknown-unknown`: the one running this
/// build when it has the target, else the newest rustup toolchain that has it, else the
/// system's.
fn module_toolchain() -> Option<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    candidates(&rustc)
        .into_iter()
        .find(|sysroot| has_wasm(sysroot))
}

/// The sysroots to try, in order: the running toolchain, then every rustup one.
fn candidates(rustc: &std::ffi::OsString) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(output) = Command::new(rustc).arg("--print").arg("sysroot").output() {
        out.push(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let home = std::env::var_os("RUSTUP_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".rustup")));
    if let Some(toolchains) = home.map(|home| home.join("toolchains"))
        && let Ok(entries) = std::fs::read_dir(toolchains)
    {
        for entry in entries.flatten() {
            let sysroot = entry.path();
            if sysroot
                .join("lib")
                .join("rustlib")
                .join("wasm32-unknown-unknown")
                .is_dir()
            {
                out.push(sysroot.display().to_string());
            }
        }
    }
    out
}

/// Whether `module` is newer than every `.rs` file under `sources`: a module built from the
/// sources as they are now.
fn fresh(module: &Path, sources: &Path) -> bool {
    let Ok(built) = std::fs::metadata(module).and_then(|meta| meta.modified()) else {
        return false;
    };
    newest(sources).is_none_or(|modified| modified <= built)
}

/// The modification time of the newest `.rs` file under `dir`.
fn newest(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = std::fs::read_dir(&path).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
                && newest.is_none_or(|newest| modified > newest)
            {
                newest = Some(modified);
            }
        }
    }
    newest
}

/// The flags a model module builds with: the two that make it a host module — no JavaScript
/// page to ask for anything (`tilo_no_js`), and getrandom's backend the host's
/// (`getrandom_backend`) — and nothing of the outer build's.
///
/// The outer build's flags are not inherited: they harden and relocate a Linux executable
/// (`-z relro`, `--compress-debug-sections`, the x86-64 baseline), and `rust-lld` refuses them
/// for a wasm module, which is not an ELF at all. A module is data the host loads, not a
/// program of this machine.
fn module_flags() -> String {
    [
        "--cfg",
        "getrandom_backend=\"custom\"",
        "--cfg",
        "tilo_no_js",
    ]
    .join("\u{1f}")
}

/// Whether `sysroot` has the wasm32 target's standard library.
fn has_wasm(sysroot: &str) -> bool {
    std::path::Path::new(sysroot)
        .join("lib")
        .join("rustlib")
        .join("wasm32-unknown-unknown")
        .join("lib")
        .is_dir()
}
