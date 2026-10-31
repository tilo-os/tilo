//! The package a test binary runs: compiled from `apps/tl/<name>` the way `tilolang build` does
//! and installed as the launcher's payload would be. A process runs one package, so each test
//! binary installs one.

use std::sync::OnceLock;

use ui::application::ApplicationDefinition;

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
        let shipped = root.join("../tl").join(name);
        let dir = if shipped.is_dir() {
            shipped
        } else {
            root.join("tests").join(name)
        };
        let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
        let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
            .expect("the packages compile");
        let (package, used) = built.split_last().expect("the package is built last");
        let imports = used.iter().map(|used| &used.compiled);
        let bundle = tilolang_tilo::bundle::Bundle::new(&package.compiled, imports);
        tilolang_app::install(bundle).expect("nothing else is installed");
        name.to_owned()
    });
    assert_eq!(installed, name, "a process runs one package");
    tilolang_app::definition()
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
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl");
        let mut dirs = tilolang::source::dependency_order(&root.join(name), &[])
            .expect("the packages resolve");
        for other in beside {
            let dir = root.join(other);
            if !dirs.contains(&dir) {
                dirs.insert(0, dir);
            }
        }
        let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
            .expect("the packages compile");
        let (package, used) = built.split_last().expect("the package is built last");
        let imports = used.iter().map(|used| &used.compiled);
        let bundle = tilolang_tilo::bundle::Bundle::new(&package.compiled, imports);
        tilolang_app::install(bundle).expect("nothing else is installed");
        name.to_owned()
    });
    assert_eq!(installed, name, "a process runs one package");
    tilolang_app::definition()
}
