//! The tilolang apps a development session installs, for the tests that start one: each
//! `apps/tl/<name>` built with `tilolang build` into a signed `.tapp` , and installed with
//! `package-store` into the test's own app store, the way `tilo-install` installs a person's app
//! and `dist/dev/shell-run.sh` installs a dev session's.
//! The session then reads the apps the way it reads any installed app: the payload from its store
//! row, the manifest the launcher opens it with, and the origins the connector planning plans from.
//! Nothing here compiles a package into a session; the only thing a test seeds is each app's first
//! scenario, which [`tilo_session::seeded_apps::seeded`] does.
//! The packages are copied to a scratch directory before they are built: a build writes each
//! package's ids lock and source catalogue, and a test never writes the tree it runs from.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tilo_app_package::pack::SigningIdentity;
use tilolang_cli::package::{self, BuildOptions};

/// The tilolang apps a development session installs, by the directory under `apps/tl` : the package
/// ids [`tilo_session::seeded_apps::APPS`] names are these, in this order.
pub const APPS: [&str; 6] = ["discussion", "notes", "tasks", "chat", "mail", "tube"];

/// The tree's `apps/tl` .
fn packages() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

/// The Tilo runtime the packages a test builds name: the release the tree's apps declare
/// (`tilo-runtime/2026.07`). Nothing here launches an app in a sandbox — a test's session reads the
/// packages and plans their connectors — so the bundle need not be installed on the machine the
/// test runs on, and a build on a builder without one is the same test.
fn runtime() -> &'static str {
    package::DEFAULT_TILO_RUNTIME
}

/// The publisher key every package of one test process is built and signed with: a test's own,
/// written beside the packages it builds, so the store believes one publisher for the run. The
/// key file is the one [`tilolang_cli::identity_at`] writes; each build reads it again, since
/// `SigningIdentity` is not `Clone`.
fn publisher(scratch: &Path) -> SigningIdentity {
    static KEY: OnceLock<PathBuf> = OnceLock::new();
    let path = KEY.get_or_init(|| scratch.join("publisher.json"));
    tilolang_cli::identity_at(path).expect("a publisher key")
}

/// Build [`APPS`] from this tree into signed `.tapp`s and install them into the app store under
/// `store`: what a device's app store holds once a person installed them.
///
/// # Panics
/// A package does not compile, the store refuses one, or a file cannot be written.
pub fn install(store: &Path, scratch: &Path) -> Vec<String> {
    let source = scratch.join("tl");
    let out = scratch.join("tapps");
    let publisher = publisher(scratch);
    let mut installed: Vec<String> = Vec::new();
    for app in APPS {
        // A build writes the package's ids lock and source catalogue, so the tree's own copy is
        // never built: a scratch copy of the package and of the packages it uses.
        let dirs = tilolang_cli::package_dirs(&packages().join(app), &[packages()])
            .unwrap_or_else(|error| panic!("{app}'s packages resolve: {error}"));
        for dir in &dirs {
            let name = dir.file_name().expect("a package directory is named");
            let target = source.join(name);
            if !target.is_dir() {
                copy_package(dir, &target);
            }
        }
        let built = package::build(
            &BuildOptions {
                dir: source.join(app),
                search: vec![source.clone()],
                out: out.clone(),
                tilo_runtime: runtime().to_owned(),
                tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
                version: "0.1.0".to_owned(),
            },
            &publisher,
        )
        .unwrap_or_else(|error| panic!("{app} builds: {error}"));
        for app in built {
            // A build of one app is a build of every package it uses too, and each is an app of its
            // own: install each once, so a package two of them use is not installed twice.
            if installed.contains(&app.app) {
                continue;
            }
            let tapp = std::fs::File::open(&app.tapp).expect("the built .tapp reads");
            let source = app.tapp.display().to_string();
            package_store::PackageStore::at(store)
                .install(
                    std::io::BufReader::new(tapp),
                    move |_meta| {
                        Ok(package_store::Admission {
                            pinned: false,
                            attested_by: String::new(),
                            source,
                            held: false,
                        })
                    },
                    None,
                )
                .unwrap_or_else(|error| panic!("{}: {error}", app.app));
            installed.push(app.app);
        }
    }
    std::fs::remove_dir_all(&out).ok();
    installed
}

/// Copy the package at `from` into `to` , created.
fn copy_package(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("the package's directory");
    for entry in std::fs::read_dir(from).expect("the package reads") {
        let entry = entry.expect("the package's entries read");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("an entry's kind").is_dir() {
            copy_package(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("the package's file");
        }
    }
}
