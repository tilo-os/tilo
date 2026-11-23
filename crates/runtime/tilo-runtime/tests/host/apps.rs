//! The tilolang apps a test's device has installed, the way a person's computer has them: each
//! `apps/tl/<name>` built from this tree into a signed `.tapp` and installed into the device's app
//! store, and the two signed runtime bundles they name (the tilolang host, `tilolang-process`, and
//! the Tilo runtime holding the libraries it and the launcher's sandbox shim load), from a
//! publisher the device's launcher trusts. A view of one then starts as on a computer: the daemon
//! starts `tilo-launcher tilolang-process` under bubblewrap, on those bundles.

use std::ffi::OsString;
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
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let host = tilo_test_env::sibling_binary("tilolang-process", "tilolang-process");

    let bundles = runtime_bundle::Identity::generate();
    let signer = root.join("runtime-publisher.json");
    std::fs::write(&signer, bundles.to_json_bytes()).unwrap();
    let version = |runtime: &'static str| runtime.split_once('/').unwrap().1;
    bundle(
        data,
        version(package::DEFAULT_TILOLANG_RUNTIME),
        &signer,
        &[],
        &[host.clone()],
    );
    bundle(
        data,
        version(package::DEFAULT_TILO_RUNTIME),
        &signer,
        &native_closure(&[&host, &launcher]),
        &[],
    );
    let trusted = root.join("trusted-publishers.txt");
    std::fs::write(&trusted, format!("{}\n", bundles.public_key_hex())).unwrap();

    let store = root.join("apps");
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
                tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
                version: "0.1.0".to_owned(),
            },
            &publisher,
        )
        .unwrap_or_else(|error| panic!("{app} builds: {error}"));
        // The app is built last, after the packages it is made with; only it is installed.
        let built = built.last().unwrap();
        let tapp = std::fs::File::open(&built.tapp).unwrap();
        let from = built.tapp.display().to_string();
        package_store::PackageStore::at(&store)
            .install(
                std::io::BufReader::new(tapp),
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
            .unwrap_or_else(|error| panic!("{app} installs: {error}"));
    }
    vec![
        ("TILO_APPS_DATA_DIR", store.into()),
        ("TILO_LAUNCHER_BIN", launcher.into()),
        (
            launcher::dev_env::TRUSTED_PUBLISHERS_FILE_ENV,
            trusted.into(),
        ),
    ]
}

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
