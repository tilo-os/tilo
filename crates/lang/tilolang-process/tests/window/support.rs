//! The package a test binary runs: compiled from `apps/tl/<name>` the way `tilolang build` does
//! and installed as the launcher's payload would be. A process runs one package, so each test
//! binary installs one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ui::application::ApplicationDefinition;

/// The packages `apps` and everything they import, in load order, and the platform's packages
/// their imports name where no directory holds them: a package comes after those it imports, so
/// a package's own imports are no further work than its name.
///
/// # Panics
/// A package's imports do not resolve.
#[allow(dead_code, reason = "each test binary uses part of the support")]
pub fn ordered(apps: &[&str]) -> (Vec<PathBuf>, Vec<tilolang::PackageMeta>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let platform = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
    let provided = tilolang::source::apps_of(&platform);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for app in apps {
        let resolved = tilolang::source::dependency_order_with(&root.join(app), &[], &provided)
            .unwrap_or_else(|e| panic!("{app}'s packages resolve: {e}"));
        for dir in resolved {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    (dirs, platform)
}

/// The definition of the package `apps/tl/<name>`, installed in this process.
///
/// # Panics
/// Another package is installed in this process, or the package does not compile.
#[allow(dead_code, reason = "each test binary uses part of the support")]
pub fn installed(name: &str) -> &'static ApplicationDefinition {
    static INSTALLED: OnceLock<String> = OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // An app package, or a package only the tests use.
        let shipped = root.join("../../../apps/tl").join(name);
        let dir = if shipped.is_dir() {
            shipped
        } else {
            root.join("tests/window").join(name)
        };
        let platform = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
        let provided = tilolang::source::apps_of(&platform);
        let dirs = tilolang::source::dependency_order_with(&dir, &[], &provided)
            .expect("the packages resolve");
        let built =
            tilolang::source::compile_dirs(&dirs, &platform, &mut tilolang::OsEntropy::default())
                .expect("the packages compile");
        let (package, used) = built.split_last().expect("the package is built last");
        let imports = used.iter().map(|used| &used.compiled);
        let bundle = tilolang_tilo::bundle::Bundle::new(&package.compiled, imports);
        tilolang_process::window::install(bundle).expect("nothing else is installed");
        name.to_owned()
    });
    assert_eq!(installed, name, "a process runs one package");
    tilolang_process::window::definition()
}

/// [`installed`] for a package that draws the person's own views, which the session hands every
/// installed app (`with_every_installed_app` in the shell's launcher): `apps/tl/<name>` with the
/// packages `beside` loaded next to it, as the apps the workspace has.
///
/// # Panics
/// As [`installed`].
#[allow(dead_code, reason = "each test binary uses part of the support")]
pub fn installed_beside(name: &str, beside: &[&str]) -> &'static ApplicationDefinition {
    static INSTALLED: OnceLock<String> = OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        let mut apps: Vec<&str> = beside.to_vec();
        apps.push(name);
        let (dirs, platform) = ordered(&apps);
        let built =
            tilolang::source::compile_dirs(&dirs, &platform, &mut tilolang::OsEntropy::default())
                .expect("the packages compile");
        let (package, used) = built.split_last().expect("the package is built last");
        let imports = used.iter().map(|used| &used.compiled);
        let bundle = tilolang_tilo::bundle::Bundle::new(&package.compiled, imports);
        tilolang_process::window::install(bundle).expect("nothing else is installed");
        name.to_owned()
    });
    assert_eq!(installed, name, "a process runs one package");
    tilolang_process::window::definition()
}
