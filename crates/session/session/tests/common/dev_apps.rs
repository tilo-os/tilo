//! The tilolang apps a development session installs, for the tests that start one: each
//! `apps/tl/<name>` built with `tilolang build` into a signed `.tapp` , and installed with
//! `package-store` into the test's own app store, the way `tilo-install` installs a person's app
//! and `tilolang run` installs a dev session's.
//! The session then reads the apps the way it reads any installed app: the payload from its store
//! row, the manifest the launcher opens it with, and the origins the connector planning plans from.
//! Nothing here compiles a package into a session; the only thing a test seeds is each app's first
//! scenario, which [`tilo_session::seeded_apps::seeded`] does.
//! The packages are copied to a scratch directory before they are built: a build writes each
//! package's ids lock and source catalogue, and a test never writes the tree it runs from.
//! The build is made once for every test process that would make the same one ([`built`]): a run
//! starts a process per test, and each built the same six packages again before its first line.
//! Every process still installs the packages into a store of its own.

use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use tilo_app_package::pack::SigningIdentity;
use tilolang_cli::package::{self, BuildOptions};

/// The tilolang packages a development session installs, by the directory under `apps/tl` : the
/// package ids [`tilo_session::seeded_apps::APPS`] names are these, and the shell is the first:
/// the session loads `so.tilo.shell` from the store as it loads any app, and this build's copy
/// stands in only where the store holds none (docs/designs/app-language/shell.md §6). Discussion
/// is no app of the session's: Tasks and Notes are made with it, and it is installed as no app
/// of its own, so their comments stay in their own records.
pub const APPS: [&str; 6] = ["shell", "notes", "tasks", "chat", "mail", "tube"];

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

/// The publisher key file of a build, in the build's own directory.
const PUBLISHER: &str = "publisher.json";

/// The list a finished build ends with, in the build's own directory: a line for each app, its
/// package id and its `.tapp`. A build that has it is whole.
const BUILT: &str = "apps";

/// How long a build is kept once another takes its place: a build is made anew whenever the
/// packages or this program change, and the ones before it are of no use to anyone.
const KEPT: Duration = Duration::from_secs(24 * 60 * 60);

/// The publisher key every package of the run is built and signed with: the test run's own, kept
/// beside the apps it built ([`built`]), so the store believes one publisher for the run. The key
/// file is the one [`tilolang_cli::identity_at`] writes; each build reads it again, since
/// `SigningIdentity` is not `Clone`.
fn publisher() -> SigningIdentity {
    tilolang_cli::identity_at(&built().dir.join(PUBLISHER)).expect("a publisher key")
}

/// One build of [`APPS`]: where it stands, and each app's package id with its signed `.tapp`, in
/// the order they were built.
struct Built {
    dir: PathBuf,
    apps: Vec<(String, PathBuf)>,
}

/// [`APPS`] built from this tree into signed `.tapp`s, once for every test process that would
/// build the same: the build stands in a directory named for what it is made from
/// ([`revision`]), under the build tree's own temporary directory, which the processes of a run
/// share. A process that finds it whole reads it. One that does not builds the packages where a
/// test's own files go, as every process did, copies what it built beside the others and moves
/// the copy into place, so a build is only ever seen whole, and two processes that build at once
/// both end with a whole one.
///
/// # Panics
/// A package does not compile, or a file cannot be written.
fn built() -> &'static Built {
    static BUILD: OnceLock<Built> = OnceLock::new();
    BUILD.get_or_init(|| {
        let builds = builds();
        let dir = builds.join(revision());
        if !dir.join(BUILT).is_file() {
            forget_old(&builds, &dir);
            // The build writes a few hundred small files, and the build tree's disk is slower
            // than the test's own temporary directory: only the packages are kept there.
            let scratch = tempfile::tempdir().expect("a directory to build the apps in");
            build_into(scratch.path());
            let staging = tempfile::Builder::new()
                .prefix(".building-")
                .tempdir_in(&builds)
                .expect("a directory beside the builds");
            copy_tree(scratch.path(), staging.path());
            let staged = staging.keep();
            // Another process may have finished the same build first: either is whole, so the one
            // that stands is the one read.
            if std::fs::rename(&staged, &dir).is_err() {
                std::fs::remove_dir_all(&staged).ok();
            }
        }
        let list = std::fs::read_to_string(dir.join(BUILT))
            .unwrap_or_else(|error| panic!("the build at {} is whole: {error}", dir.display()));
        let apps = list
            .lines()
            .map(|line| {
                let (app, tapp) = line.split_once('\t').expect("an app and its package");
                (app.to_owned(), dir.join("tapps").join(tapp))
            })
            .collect();
        Built { dir, apps }
    })
}

/// Where the builds stand: the build tree's temporary directory, which Cargo gives every test
/// process of a build and which outlives each of them. Where it cannot be written (a run that
/// moved its test binaries to a machine without the build tree) it is the process's own temporary
/// directory, and the process builds for itself.
fn builds() -> PathBuf {
    let shared = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tilo-session-dev-apps");
    if std::fs::create_dir_all(&shared).is_ok() {
        return shared;
    }
    let own = std::env::temp_dir().join("tilo-session-dev-apps");
    std::fs::create_dir_all(&own).expect("a directory for the apps' build");
    own
}

/// What a build is made from, as one word: every file of the packages under `apps/tl` , by its
/// path and what it holds, and the program that compiles them, which is this test binary. Two
/// processes answer the same word only where they would build the same packages with the same
/// compiler.
fn revision() -> String {
    fn files_below(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the packages read") {
            let path = entry.expect("the packages' entries read").path();
            if path.is_dir() {
                files_below(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let program = std::env::current_exe().expect("this test's program");
    let compiled = std::fs::metadata(&program).expect("this test's program reads");
    program.hash(&mut hasher);
    compiled.len().hash(&mut hasher);
    compiled.modified().ok().hash(&mut hasher);
    let packages = packages();
    let mut files = Vec::new();
    files_below(&packages, &mut files);
    files.sort();
    for file in files {
        file.strip_prefix(&packages)
            .expect("a file below the packages")
            .hash(&mut hasher);
        std::fs::read(&file)
            .expect("a package's file reads")
            .hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Remove the builds under `builds` made longer ago than [`KEPT`], the one at `dir` excepted:
/// those of packages and programs since changed, and what a build that was killed left half made.
fn forget_old(builds: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(builds) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|held| held.modified())
            .ok()
            .and_then(|made| SystemTime::now().duration_since(made).ok())
            .is_some_and(|age| age > KEPT);
        if old && path != dir {
            std::fs::remove_dir_all(&path).ok();
        }
    }
}

/// Build [`APPS`] from this tree into signed `.tapp`s under `dir` , with a publisher key made
/// there, and end with the list of what was built ([`BUILT`]).
///
/// # Panics
/// A package does not compile, or a file cannot be written.
fn build_into(dir: &Path) {
    let source = dir.join("tl");
    let out = dir.join("tapps");
    let publisher = tilolang_cli::identity_at(&dir.join(PUBLISHER)).expect("a publisher key");
    let mut apps: Vec<(String, String)> = Vec::new();
    for app in APPS {
        // A build writes the package's ids lock and source catalogue, so the tree's own copy is
        // never built: a scratch copy of the package and of the packages it uses.
        let dirs = tilolang_cli::package_dirs(&packages().join(app), &[packages()])
            .unwrap_or_else(|error| panic!("{app}'s packages resolve: {error}"));
        for dir in &dirs {
            let name = dir.file_name().expect("a package directory is named");
            let target = source.join(name);
            if !target.is_dir() {
                copy_tree(dir, &target);
            }
        }
        let built = package::build(
            &BuildOptions {
                dir: source.join(app),
                // The packages it uses are the copied tilolang ones, and the native apps whose
                // surfaces they read (`use so.tilo.session`, `apps/tl/session`'s).
                search: vec![source.clone(), packages()],
                out: out.clone(),
                tilo_runtime: runtime().to_owned(),
                tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
                version: "0.1.0".to_owned(),
            },
            &publisher,
        )
        .unwrap_or_else(|error| panic!("{app} builds: {error}"));
        // The build answers the packages the app is made with first, and the app itself last:
        // the app is what is installed, as a person installs an app and not the packages it is
        // made with (Discussion among them, whose records Tasks and Notes keep in their own).
        let named = built
            .into_iter()
            .last()
            .unwrap_or_else(|| panic!("{app} builds no package"));
        if !apps.iter().any(|(held, _)| *held == named.app) {
            let tapp = named
                .tapp
                .file_name()
                .expect("a built .tapp is named")
                .to_string_lossy()
                .into_owned();
            apps.push((named.app, tapp));
        }
    }
    // What a build leaves beside its `.tapp`s is of no use once they are packed: the copied
    // sources and each package's staged payload.
    std::fs::remove_dir_all(&source).ok();
    for staged in std::fs::read_dir(&out)
        .expect("the built packages read")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
    {
        std::fs::remove_dir_all(&staged).ok();
    }
    let mut list = String::new();
    for (app, tapp) in &apps {
        list.push_str(app);
        list.push('\t');
        list.push_str(tapp);
        list.push('\n');
    }
    std::fs::write(dir.join(BUILT), list).expect("the list of what was built");
}

/// Install [`APPS`], built from this tree into signed `.tapp`s ([`built`]), into the app store
/// under `store`: what a device's app store holds once a person installed them. The second
/// argument is where a process once made a build of its own; the build is the run's now, and its
/// callers still name one.
///
/// # Panics
/// A package does not compile, the store refuses one, or a file cannot be written.
pub fn install(store: &Path, _scratch: &Path) -> Vec<String> {
    let mut installed: Vec<String> = Vec::new();
    for (app, tapp) in &built().apps {
        let package = std::fs::File::open(tapp).expect("the built .tapp reads");
        let source = tapp.display().to_string();
        package_store::PackageStore::at(store)
            .install(
                std::io::BufReader::new(package),
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
            .unwrap_or_else(|error| panic!("{app}: {error}"));
        installed.push(app.clone());
    }
    installed
}

/// Build the tilolang package written at `dir` (a directory of `.tl` files) and install it into
/// the store `store`, signing it with the test process's publisher key. A shell-style package in
/// a test is written into a scratch directory and installed here, exactly as a person installs
/// one into their own store (`shell.md` §6); the session then lists its style like any other.
///
/// # Panics
/// The package does not compile, or the store refuses it.
pub fn install_package(dir: &Path, store: &Path) -> String {
    let scratch = dir.with_file_name(format!(
        "{}-built",
        dir.file_name()
            .expect("a directory is named")
            .to_string_lossy()
    ));
    std::fs::create_dir_all(&scratch).expect("a scratch directory for the build");
    let out = scratch.join("tapps");
    let built = package::build(
        &BuildOptions {
            dir: dir.to_path_buf(),
            search: vec![packages()],
            out: out.clone(),
            tilo_runtime: runtime().to_owned(),
            tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
            version: "0.1.0".to_owned(),
        },
        &publisher(),
    )
    .unwrap_or_else(|error| panic!("{} builds: {error}", dir.display()));
    let mut installed = String::new();
    for app in built {
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
        installed = app.app;
    }
    std::fs::remove_dir_all(&out).ok();
    installed
}

/// Copy the directory at `from` , a package or a build of them, into `to` , created.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("the copy's directory");
    for entry in std::fs::read_dir(from).expect("the directory reads") {
        let entry = entry.expect("the directory's entries read");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("an entry's kind").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("the copied file");
        }
    }
}
