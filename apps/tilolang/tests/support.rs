//! The package a test binary runs: compiled from `apps/tl/<name>` the way `tilolang build` does
//! and installed as the launcher's payload would be. A process runs one package, so each test
//! binary installs one.

use std::sync::OnceLock;

use ui::application::ApplicationDefinition;

/// The definition of the package `apps/tl/<name>`, installed in this process.
///
/// # Panics
/// Another package is installed in this process, or the package does not compile.
pub fn installed(name: &str) -> &'static ApplicationDefinition {
    static INSTALLED: OnceLock<String> = OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tl")
            .join(name);
        let dirs = tilolang::source::dependency_order(&dir, &[]).expect("the packages resolve");
        let built = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
            .expect("the packages compile");
        let (package, used) = built.split_last().expect("the package is built last");
        let imports = used.iter().map(|used| used.compiled.meta.clone()).collect();
        let bundle = tilolang_tilo::bundle::Bundle::new(&package.compiled, imports);
        tilolang_app::install(bundle).expect("nothing else is installed");
        name.to_owned()
    });
    assert_eq!(installed, name, "a process runs one package");
    tilolang_app::definition()
}
