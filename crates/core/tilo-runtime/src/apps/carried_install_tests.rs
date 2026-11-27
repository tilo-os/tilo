//! What a first start installs from the runtime packages the program carries: the Tilo runtime
//! a Mac's `Tilo.app` is, and the tilolang runtime its apps run on. A package a person installs
//! finds the Tilo runtime its manifest names in the store's records, exactly as if the runtime
//! had come from a registry.

use ed25519_dalek::SigningKey;

/// The runtime release Tilo.app's build is, and so the one a package names to run there.
const RELEASE: &str = "2026.07";

fn isa_line() -> &'static str {
    match runtime_bundle::host_arch() {
        "aarch64" => "aarch64 = \"aarch64\"",
        _ => "x86_64 = \"x86-64\"",
    }
}

/// A signed runtime package of the bundle at `bundle`, providing `provides`.
fn runtime_tapp(
    dir: &std::path::Path,
    bundle: &std::path::Path,
    provides: &str,
    publisher: &SigningKey,
) -> Vec<u8> {
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.runtime"
name = "Tilo runtime"
category = "System"
version = "0.1.0"
kind = "runtime"
provides = "{provides}"

[package.isa]
{isa}

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
        isa = isa_line(),
        publisher = hex::encode(publisher.verifying_key().to_bytes()),
    ))
    .unwrap();
    let packed = tilo_app_package::pack::pack(
        bundle,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(publisher.clone()),
    )
    .expect("the runtime package packs");
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    // The folder names each package by its root, as a carried package is named.
    let root = app_registry::tapp::load(&bytes)
        .unwrap()
        .package_id
        .to_hex();
    std::fs::write(dir.join(format!("{root}.tapp")), &bytes).unwrap();
    bytes
}

/// A signed package of a tilolang app that runs on `runtime`: no target, so the store demands the
/// Tilo runtime its manifest names.
fn app_tapp(
    dir: &std::path::Path,
    name: &str,
    runtime: &str,
    publisher: &SigningKey,
) -> std::path::PathBuf {
    use std::fmt::Write as _;
    let payload = dir.join(format!("payload-{name}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"#!/bin/true\n").unwrap();
    let mut bound = String::new();
    let _ = writeln!(bound, "runtime = \"{runtime}\"");
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.{name}"
version = "1.0.0"
kind = "app"
name = "Notes"
category = "Office"
{bound}

[package.isa]
{isa}

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
        isa = isa_line(),
        publisher = hex::encode(publisher.verifying_key().to_bytes()),
    ))
    .unwrap();
    let packed = tilo_app_package::pack::pack(
        &payload,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(publisher.clone()),
    )
    .expect("the app packs");
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let path = dir.join(format!("{name}.tapp"));
    std::fs::write(&path, &bytes).unwrap();
    path
}

struct Fixture {
    dir: tempfile::TempDir,
    _env: tilo_test_env::ScopedEnv,
    store: package_store::PackageStore,
    catalog: package_store::Catalog,
    carried: std::path::PathBuf,
}

/// A device with an empty store and the folder Tilo.app carries its runtimes in, holding one
/// signed Tilo runtime package named by its root.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let apps = dir.path().join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let carried = dir.path().join("Tilo.app/Contents/Resources/runtimes");
    std::fs::create_dir_all(&carried).unwrap();
    let bundle_key = runtime_bundle::Identity::generate();
    let key_file = dir.path().join("runtime-identity.json");
    std::fs::write(&key_file, bundle_key.to_json_bytes()).unwrap();
    std::fs::write(
        dir.path().join("trusted-runtime-publishers.txt"),
        format!("{}\n", bundle_key.public_key_hex()),
    )
    .unwrap();
    let binary = dir.path().join("tilo-runtime");
    std::fs::write(&binary, b"#!/bin/true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bundle = dir.path().join("bundle");
    runtime_bundle::build_binaries_bundle(
        &bundle,
        RELEASE,
        Some(runtime_bundle::host_arch()),
        Some(&key_file),
        &[binary],
        &[],
    )
    .expect("the Tilo runtime bundle bakes");
    let publisher = SigningKey::from_bytes(&[9; 32]);
    runtime_tapp(
        &carried,
        &bundle,
        &format!("tilo-runtime/{RELEASE}"),
        &publisher,
    );
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_APPS_SYSTEM_DIR")
        .remove(runtime_bundle::RUNTIME_DIR_ENV)
        .set("TILO_APPS_DATA_DIR", &apps)
        .set(
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
            dir.path().join("trusted-runtime-publishers.txt"),
        );
    Fixture {
        store: package_store::PackageStore::at(&apps),
        catalog: package_store::Catalog::at(&apps, None),
        carried,
        dir,
        _env: env,
    }
}

fn install(
    fixture: &Fixture,
    path: &std::path::Path,
) -> Result<(), package_store::installer::Error> {
    package_store::installer::install(
        &fixture.store,
        &fixture.catalog,
        &package_store::installer::Candidate::staged(path, "test"),
        &package_store::installer::Request {
            trust: package_store::installer::Trust {
                allow_self_signed: true,
                ..package_store::installer::Trust::default()
            },
            ..package_store::installer::Request::default()
        },
        None,
    )
    .map(drop)
}

#[test]
fn the_runtime_the_program_carries_is_recorded_and_an_app_naming_it_installs() {
    let fixture = fixture();
    super::install_carried_runtimes_in(
        &fixture.catalog,
        &fixture.store,
        &fixture.carried,
        "Tilo.app",
    );
    assert!(
        package_store::runtime_installed(&format!("tilo-runtime/{RELEASE}")),
        "the runtime Tilo.app is is installed in the store the installer reads"
    );
    assert!(
        fixture
            .dir
            .path()
            .join("apps/runtimes")
            .join(RELEASE)
            .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
            .is_file(),
        "the bundle lands where a launch resolves it"
    );

    let publisher = SigningKey::from_bytes(&[10; 32]);
    let app = app_tapp(
        fixture.dir.path(),
        "notes",
        &format!("tilo-runtime/{RELEASE}"),
        &publisher,
    );
    install(&fixture, &app).expect("the app naming the runtime Tilo.app is installs");
    assert!(
        package_store::find_installed("so.tilo.notes")
            .unwrap()
            .is_some(),
        "the app is installed"
    );
}

#[test]
fn an_app_naming_another_release_is_refused_by_name() {
    let fixture = fixture();
    super::install_carried_runtimes_in(
        &fixture.catalog,
        &fixture.store,
        &fixture.carried,
        "Tilo.app",
    );
    let publisher = SigningKey::from_bytes(&[10; 32]);
    let app = app_tapp(
        fixture.dir.path(),
        "later",
        "tilo-runtime/2027.01",
        &publisher,
    );
    let refused = install(&fixture, &app).expect_err("the runtime is not this computer's");
    let said = refused.to_string();
    assert!(said.contains("tilo-runtime/2027.01"), "{said}");
    assert!(said.contains("needs runtime"), "{said}");
    assert!(
        package_store::find_installed("so.tilo.later")
            .unwrap()
            .is_none(),
        "nothing is installed beside a runtime this computer does not have"
    );
}

/// A second start does not install what the first already did, and keeps what the person has.
#[test]
fn a_carried_runtime_is_installed_once() {
    let fixture = fixture();
    super::install_carried_runtimes_in(
        &fixture.catalog,
        &fixture.store,
        &fixture.carried,
        "Tilo.app",
    );
    let before = fixture
        .catalog
        .list()
        .unwrap()
        .into_iter()
        .map(|package| package.package_id_hex)
        .collect::<Vec<_>>();
    super::install_carried_runtimes_in(
        &fixture.catalog,
        &fixture.store,
        &fixture.carried,
        "Tilo.app",
    );
    let after = fixture
        .catalog
        .list()
        .unwrap()
        .into_iter()
        .map(|package| package.package_id_hex)
        .collect::<Vec<_>>();
    assert_eq!(before, after);
}
