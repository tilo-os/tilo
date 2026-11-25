//! The runtime runs a development build as the id it was compiled for: the device's own build of
//! the app, which the launcher names in `TILO_PACKAGE_ID`, and not the app its sources declare.

use tilolang::source::PackageSource;
use tilolang::{IdsLock, SeededEntropy, compile_package_source_for};
use tilolang_process::window::{InstallError, PACKAGE_ID_ENV, install_from_env};
use tilolang_tilo::bundle::{Bundle, MODULE_ENV};

const SOURCE: &str = r#"app org.example.dev "Dev build"

/// What the app keeps.
type Value {
    /// A count.
    value: Int merge counter
}

root Value

default view value for Value

/// Show the value.
view value(v: Value) {
    std.page(v) {
        ui.Text("{v.value}", role: .title, numeric: true)
    }
}
"#;

/// The module of [`SOURCE`], compiled as `app` when one is named, written where the launcher hands
/// the runtime its payload from.
fn staged(app: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let source = PackageSource::single(SOURCE);
    let compiled =
        compile_package_source_for(&source, &IdsLock::new(), &[], app, &mut SeededEntropy(0))
            .expect("the package compiles");
    Bundle::new(&compiled, core::iter::empty())
        .write_installed(dir.path())
        .expect("the bundle is written");
    dir
}

#[test]
fn a_development_build_runs_under_the_id_it_was_compiled_for() {
    let dev = "org.example.dev.dev.0123456789abcdef";
    let dir = staged(Some(dev));
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(MODULE_ENV, dir.path());
    environment.set(PACKAGE_ID_ENV, dev);
    install_from_env().expect("the device's own build installs under the id it runs as");
}

#[test]
fn a_bundle_compiled_for_another_id_is_refused() {
    let dev = "org.example.dev.dev.0123456789abcdef";
    let dir = staged(None);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(MODULE_ENV, dir.path());
    environment.set(PACKAGE_ID_ENV, dev);
    match install_from_env() {
        Err(InstallError::IdMismatch { signed, declared }) => {
            assert_eq!(signed, dev);
            assert_eq!(declared, "org.example.dev");
        }
        other => panic!("a bundle compiled for another id was not refused: {other:?}"),
    }
}
