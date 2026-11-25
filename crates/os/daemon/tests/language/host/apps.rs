//! The tilolang apps a test's device has installed, the way a person's computer has them: each
//! `apps/tl/<name>` built from this tree into a signed `.tapp` and installed into the device's app
//! store, and the runtimes they name. On Linux those are two signed runtime bundles (the tilolang
//! host, `tilolang-process`, and the Tilo runtime holding the libraries it and the launcher's
//! sandbox shim load), from a publisher the device's launcher trusts, and a view of one starts as
//! on a computer: the daemon starts `tilo-launcher tilolang-process` under bubblewrap, on those
//! bundles. On macOS the tilolang runtime is the signed runtime package Tilo.app carries,
//! installed into the same store (`tilolang-process` and the `GStreamer` it loads, relocated beside
//! it as `dist/macos` ships them), and the daemon starts its binary in a Seatbelt sandbox.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tilo_test_env::SpawnUnderEnvLock as _;
use tilolang_cli::package::{self, BuildOptions};

/// The tree's `apps/tl`.
fn packages() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

/// Install `apps` (directories under `apps/tl`) under `root`, the device's data directory being
/// `data`: answers the environment the device's process is started with, which names its app
/// store, its launcher and the publisher its runtime bundles are trusted from.
///
/// # Panics
/// A package does not compile, a program the bundles carry was not built by the run, or a file
/// cannot be written.
pub fn install(root: &Path, data: &Path, apps: &[&str]) -> Vec<(&'static str, OsString)> {
    let store = root.join("apps");
    #[cfg(target_os = "linux")]
    let mut env = runtime_bundles(root, data);
    #[cfg(target_os = "macos")]
    let mut env = {
        let _ = data;
        runtime_package(root, &store);
        Vec::new()
    };
    let publisher = tilolang_cli::identity_at(&root.join("publisher.json")).unwrap();
    let source = root.join("tl");
    for app in apps {
        // A build writes each package's ids lock and source catalogue: the tree's own copy is
        // never built, a scratch copy of the app and the packages it uses is.
        for dir in tilolang_cli::package_dirs(&packages().join(app), &[packages()]).unwrap() {
            let target = source.join(dir.file_name().unwrap());
            if !target.is_dir() {
                copy_tree(&dir, &target);
            }
        }
        let built = package::build(
            &BuildOptions {
                dir: source.join(app),
                search: vec![source.clone(), packages()],
                out: root.join("tapps"),
                tilo_runtime: package::DEFAULT_TILO_RUNTIME.to_owned(),
                tilolang_runtime: package::default_tilolang_runtime(),
                version: "0.1.0".to_owned(),
                // The scratch copy's ids lock is the build's to write, as a developer's tree is.
                locked: false,
            },
            &publisher,
        )
        .unwrap_or_else(|error| panic!("{app} builds: {error}"));
        // The app is built last, after the packages it is made with; only it is installed.
        install_tapp(&store, &built.last().unwrap().tapp);
    }
    env.push(("TILO_APPS_DATA_DIR", store.into()));
    env
}

/// Build the package `source` (an `app.tl`), written into a scratch directory as `name`, and
/// install it into the store the device's process names (`TILO_APPS_DATA_DIR`), as a person or
/// their agent installs a package while the device runs: signed by the publisher [`install`]
/// made, and built against the packages it installed.
///
/// # Panics
/// The process names no store [`install`] made, or the package does not build or install.
pub fn install_source(name: &str, source: &str) {
    let store = PathBuf::from(
        std::env::var_os("TILO_APPS_DATA_DIR").expect("the device's process names its store"),
    );
    let root = store
        .parent()
        .expect("the store is in the test's root")
        .to_owned();
    let publisher = tilolang_cli::identity_at(&root.join("publisher.json")).unwrap();
    let dir = root.join("written").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.ids"), "").unwrap();
    std::fs::write(dir.join("app.tl"), source).unwrap();
    let built = package::build(
        &BuildOptions {
            dir,
            search: vec![root.join("tl"), packages()],
            out: root.join("tapps"),
            tilo_runtime: package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_runtime: package::default_tilolang_runtime(),
            version: "0.1.0".to_owned(),
            locked: false,
        },
        &publisher,
    )
    .unwrap_or_else(|error| panic!("{name} builds: {error}"));
    install_tapp(&store, &built.last().unwrap().tapp);
}

/// Install the package at `tapp` into the app store at `store`.
fn install_tapp(store: &Path, tapp: &Path) {
    let from = tapp.display().to_string();
    let file = std::fs::File::open(tapp).unwrap();
    package_store::PackageStore::at(store)
        .install(
            std::io::BufReader::new(file),
            move |_| {
                Ok(package_store::Admission {
                    pinned: false,
                    attested_by: String::new(),
                    source: from,
                    held: false,
                })
            },
            None,
        )
        .unwrap_or_else(|error| panic!("{} installs: {error}", tapp.display()));
}

/// The runtime bundles the apps name, under `data`'s runtimes, signed by a publisher of `root`'s:
/// answers the environment that names the launcher and that publisher.
#[cfg(target_os = "linux")]
fn runtime_bundles(root: &Path, data: &Path) -> Vec<(&'static str, OsString)> {
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let host = tilo_test_env::sibling_binary("tilolang-process", "tilolang-process");

    let bundles = runtime_bundle::Identity::generate();
    let signer = root.join("runtime-publisher.json");
    std::fs::write(&signer, bundles.to_json_bytes()).unwrap();
    let version = |runtime: &'static str| runtime.split_once('/').unwrap().1;
    // The tilolang runtime carries the standard library the apps are built against, which none of
    // them does.
    let tilolang = package::default_tilolang_runtime();
    let std = package::write_std(root).unwrap();
    bundle(
        data,
        tilolang.split_once('/').unwrap().1,
        &signer,
        std::slice::from_ref(&std),
        std::slice::from_ref(&host),
    );
    // The Tilo runtime carries the pinned font catalog the product's bundles carry, at the
    // `lib/fonts` the launcher binds where a sandboxed app sets text. Without it every app a
    // test's daemon starts draws in no font at all.
    let mut libs = native_closure(&[&host, &launcher]);
    if let Some(fonts) = carried_fonts(root) {
        libs.push(fonts);
    }
    bundle(
        data,
        version(package::DEFAULT_TILO_RUNTIME),
        &signer,
        &libs,
        &[],
    );
    let trusted = root.join("trusted-publishers.txt");
    std::fs::write(&trusted, format!("{}\n", bundles.public_key_hex())).unwrap();
    vec![
        ("TILO_LAUNCHER_BIN", launcher.into()),
        (
            launcher::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
            trusted.into(),
        ),
    ]
}

/// This build's pinned font catalog under a directory named `fonts`, which is what a runtime
/// bundle's include keeps as its `lib/fonts`.
#[cfg(target_os = "linux")]
fn carried_fonts(root: &Path) -> Option<PathBuf> {
    let catalog = ui::font_catalog::catalog_root()?;
    let fonts = root.join("fonts");
    std::os::unix::fs::symlink(&catalog, &fonts).ok()?;
    Some(fonts)
}

/// The tilolang runtime package the apps name, as Tilo.app carries it, installed into the app
/// store at `store`: this build's `tilolang-process` and the GStreamer closure it loads, staged
/// beside it by `dist/macos/collection_local_runtime.py` and signed ad hoc, as a developer's Mac
/// runs them.
#[cfg(target_os = "macos")]
fn runtime_package(root: &Path, store: &Path) {
    let host = tilo_test_env::sibling_binary("tilolang-process", "tilolang-process");
    let payload = root.join("tilolang-runtime");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let staged = std::process::Command::new("python3")
        .arg(repo.join("dist/macos/collection_local_runtime.py"))
        .arg("tilolang")
        .arg("--binary")
        .arg(&host)
        .arg("--out")
        .arg(&payload)
        .status_locked()
        .unwrap();
    assert!(staged.success(), "the tilolang runtime is staged");
    // Relocating a library rewrites it: each is signed again, the binary last.
    let mut signed = Vec::new();
    for dir in ["Frameworks", "PlugIns", "Helpers"] {
        files_under(&payload.join("lib/gstreamer").join(dir), &mut signed);
    }
    signed.push(payload.join(launcher::manifest::TILOLANG_BIN));
    for file in signed {
        let status = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&file)
            .status_locked()
            .unwrap();
        assert!(status.success(), "{} is signed", file.display());
    }
    package::write_std(&payload.join("lib")).unwrap();
    let identity = tilolang_cli::identity_at(&root.join("runtime-publisher.json")).unwrap();
    let tilolang = package::default_tilolang_runtime();
    let version = tilolang.split_once('/').unwrap().1;
    let manifest = format!(
        r#"[package]
id = "so.tilo.runtime.tilolang"
name = "tilolang runtime"
category = "System"
version = "0.1.0"
kind = "runtime"
provides = "tilolang/{version}"
platforms = ["macos"]
platform-reason = "Its binaries are Mac binaries."

[package.isa]
aarch64 = "aarch64"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
type = "cli"
bin = "{bin}"
"#,
        publisher = hex::encode(identity.public_key_bytes()),
        bin = launcher::manifest::TILOLANG_BIN,
    );
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(&manifest).unwrap();
    let packed = tilo_app_package::pack::pack(&payload, &manifest, &identity).unwrap();
    let tapp = root.join("tilolang-runtime.tapp");
    tilo_app_package::tapp::write_file(
        &tapp,
        &packed.meta,
        Some(&packed as &dyn tilo_app_package::BlobSource),
        tilo_app_package::tapp::WriteOptions::replace(),
    )
    .unwrap();
    install_tapp(store, &tapp);
}

/// Every file below `dir`, into `files`.
#[cfg(target_os = "macos")]
fn files_under(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            files_under(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

#[cfg(target_os = "linux")]
/// A runtime bundle `version` under `data`'s runtimes, of this machine's loader, the libraries
/// `libs` and the programs `bins`, signed by the publisher at `signer`.
fn bundle(data: &Path, version: &str, signer: &Path, libs: &[PathBuf], bins: &[PathBuf]) {
    runtime_bundle::build_bundle_with(
        &data.join("tilo-runtime").join(version),
        version,
        &runtime_bundle::BuildOptions {
            from: None,
            arch: None,
            identity: Some(signer),
            extra_libs: libs,
            extra_bins: bins,
        },
    )
    .unwrap_or_else(|error| panic!("the runtime bundle {version}: {error}"));
}

#[cfg(target_os = "linux")]
/// The shared objects `executables` load, as this machine's loader resolves them: what a bundle
/// built here carries so they run inside a sandbox.
fn native_closure(executables: &[&Path]) -> Vec<PathBuf> {
    let mut libraries = std::collections::BTreeSet::new();
    for executable in executables {
        let report = std::process::Command::new("ldd")
            .arg(executable)
            .output_locked()
            .unwrap();
        assert!(report.status.success(), "ldd {}", executable.display());
        let report = String::from_utf8(report.stdout).unwrap();
        assert!(!report.contains("not found"), "{report}");
        libraries.extend(
            report
                .split_whitespace()
                .filter(|word| word.starts_with('/') && !word.ends_with(':'))
                .map(PathBuf::from),
        );
    }
    libraries.into_iter().collect()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Install `package`, a Rust app of this tree's `apps/` (its entrypoint built by this run as
/// `bin`), into the device's app store at `root`, beside the tilolang apps [`install`] made
/// there: its payload from the binary this run built and the surface the app publishes, signed
/// by a publisher of the test's own, as `tilo-install` leaves one.
///
/// The daemon runs every app in a sandbox, so the package needs what a person's computer gives
/// it and no less: the manifest is installed as the tree writes it, but for the device shell
/// service, which only a session hands over. A record action needs none of it.
///
/// # Panics
/// The app's manifest or surface cannot be read, the run built no `bin`, or the store cannot be
/// written.
pub fn install_rust(root: &Path, package: &str, bin: &str) {
    let dir = packages().parent().unwrap().join(package);
    let manifest = std::fs::read_to_string(dir.join("tilo-app.toml"))
        .unwrap_or_else(|error| panic!("{package}'s manifest: {error}"))
        .replace("ush    = \"required@37\"", "ush    = \"optional@37\"");
    let mut parsed = tilo_app_package::Manifest::from_toml_str(&manifest)
        .unwrap_or_else(|error| panic!("{package}'s manifest reads: {error}"));
    let publisher = tilo_app_package::SigningIdentity::from_seed(&[9_u8; 32]);
    parsed.publisher.identity = format!("ed25519:{}", hex::encode(publisher.public_key_bytes()));
    // The payload as the packer reads it: the binary this run built at the manifest's entrypoint
    // and the surface the app publishes.
    let payload = root.join("rust").join(package);
    let entry = payload.join(
        &parsed
            .entrypoint
            .as_ref()
            .expect("the manifest names its entrypoint")
            .bin,
    );
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    // A debug build is larger than any package may be, and the store's install refuses it, so the
    // package is placed as an install leaves one; a copy keeps the build's mode, so it runs.
    std::fs::copy(tilo_test_env::sibling_binary(package, bin), &entry)
        .unwrap_or_else(|error| panic!("{package}'s binary: {error}"));
    std::fs::copy(dir.join("surface.json"), payload.join("surface.json"))
        .unwrap_or_else(|error| panic!("{package}'s surface: {error}"));
    let meta = tilo_app_package::pack(&payload, &parsed, &publisher)
        .unwrap_or_else(|error| panic!("{package} packs: {error}"))
        .meta;
    let store = package_store::layout::Layout::new(root.join("apps"));
    std::fs::create_dir_all(store.packages()).unwrap();
    std::fs::rename(&payload, store.payload(meta.id)).unwrap();
    let record = store.meta_dir(meta.id);
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(
        record.join(package_store::layout::SIGNED_ROOT_FILE),
        meta.signed_root_bytes(),
    )
    .unwrap();
    std::fs::write(store.manifest(meta.id), &meta.manifest_bytes).unwrap();
    std::fs::write(
        record.join(package_store::layout::COLLECTION_FILE),
        &meta.collection_bytes,
    )
    .unwrap();
    let mut registry = std::fs::read_to_string(store.registry()).unwrap_or_default();
    write!(
        registry,
        "[[entry]]\nid = {:?}\nversion = {:?}\npackage_id_hex = {:?}\npayload_root = {:?}\n\
         manifest_path = {:?}\npublisher_key_hex = {:?}\nruntime = {:?}\n\n",
        parsed.package.id,
        parsed.package.version,
        meta.id.to_hex(),
        store.payload(meta.id).display().to_string(),
        store.manifest(meta.id).display().to_string(),
        hex::encode(publisher.public_key_bytes()),
        parsed.package.runtime.clone().unwrap_or_default(),
    )
    .unwrap();
    std::fs::write(store.registry(), registry).unwrap();
}
