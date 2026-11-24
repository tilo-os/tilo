//! Installing a development build: the device installs its own signed dev build under the id its
//! marking derives, and refuses one another device signed or one signed by nobody.

use package_store::Catalog;
use package_store::installer;
use tilo_app_package::{Manifest, SigningIdentity, SignedRoot, pack};

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// A development build of `of` made by `identity`, as `tilolang dev` packs it: the marking, the
/// derived id, and the publisher's signature.
fn dev_manifest(identity: &SigningIdentity, of: &str) -> Manifest {
    let device = hex::encode(identity.public_key_bytes());
    let id = tilo_app_package::dev_app_id(of, &identity.public_key_bytes());
    Manifest::from_toml_str(&format!(
        r#"[package]
id = "{id}"
name = "Notes"
version = "0.1.0"
kind = "app"

[package.dev]
device = "ed25519:{device}"
of = "{of}"

[publisher]
identity = "ed25519:{device}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/notes"
type = "gui"
"#
    ))
    .unwrap()
}

/// The bytes of `manifest`'s package with `signature`'s signer, or none.
fn tapp(identity: &SigningIdentity, manifest: &Manifest, unsigned: bool) -> Vec<u8> {
    let payload = scratch();
    std::fs::create_dir_all(payload.path().join("bin")).unwrap();
    std::fs::write(payload.path().join("bin/notes"), b"program").unwrap();
    let packed = pack(payload.path(), manifest, identity).unwrap();
    if !unsigned {
        return tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    }
    let unsigned = tilo_app_package::PackageMeta::from_root(
        SignedRoot::unsigned(
            packed.meta.root.manifest_hash,
            packed.meta.root.payload_root_hash,
            packed.meta.root.publisher_identity,
        ),
        packed.meta.manifest_bytes.clone(),
        packed.meta.collection_bytes.clone(),
    )
    .unwrap();
    tilo_app_package::tapp::to_bytes(&unsigned, Some(&packed)).unwrap()
}

fn store(root: &std::path::Path) -> (package_store::PackageStore, Catalog) {
    (
        package_store::PackageStore::at(root),
        Catalog::at(root, None),
    )
}

#[test]
fn a_dev_build_installs_under_its_own_id() {
    let dir = scratch();
    let identity = SigningIdentity::from_seed(&[7; 32]);
    let of = "org.example.notes";
    let manifest = dev_manifest(&identity, of);
    let bytes = tapp(&identity, &manifest, false);
    let (store, catalog) = store(dir.path());
    let report = installer::install_dev(
        &store,
        &catalog,
        &bytes,
        &identity,
        "dev.tapp",
    )
    .expect("the device installs the build it made");
    let id = report.selected.as_ref().unwrap().id.clone();
    assert_eq!(
        id,
        tilo_app_package::dev_app_id(of, &identity.public_key_bytes())
    );
    assert_ne!(id, of, "a dev build never takes the app's own id");
    let installed = catalog.find(&id).unwrap().expect("it is installed");
    let read = std::fs::read_to_string(installed.manifest_path).unwrap();
    let manifest = Manifest::from_toml_str(&read).unwrap();
    assert_eq!(manifest.package.dev.unwrap().of, of);
}

#[test]
fn a_dev_build_another_device_signed_is_refused() {
    let dir = scratch();
    let this_device = SigningIdentity::from_seed(&[7; 32]);
    let another = SigningIdentity::from_seed(&[9; 32]);
    let bytes = tapp(&another, &dev_manifest(&another, "org.example.notes"), false);
    let (store, catalog) = store(dir.path());
    let refused = installer::install_dev(
        &store,
        &catalog,
        &bytes,
        &this_device,
        "dev.tapp",
    )
    .unwrap_err();
    match refused {
        installer::Error::Policy(said) => assert!(
            said.contains("another device"),
            "the refusal names the device that signed: {said}"
        ),
        other => panic!("a foreign dev build was not refused as policy: {other:?}"),
    }
    assert!(
        catalog.find("org.example.notes").unwrap().is_none(),
        "nothing was installed"
    );
}

#[test]
fn an_unsigned_dev_build_is_refused() {
    let dir = scratch();
    let identity = SigningIdentity::from_seed(&[7; 32]);
    let bytes = tapp(&identity, &dev_manifest(&identity, "org.example.notes"), true);
    let (store, catalog) = store(dir.path());
    let refused = installer::install_dev(
        &store,
        &catalog,
        &bytes,
        &identity,
        "dev.tapp",
    )
    .unwrap_err();
    match refused {
        installer::Error::Policy(said) => assert!(
            said.contains("no signature this device made"),
            "the refusal says it is unsigned: {said}"
        ),
        other => panic!("an unsigned dev build was not refused as policy: {other:?}"),
    }
}

#[test]
fn a_package_without_a_dev_marking_is_refused() {
    let dir = scratch();
    let identity = SigningIdentity::from_seed(&[7; 32]);
    let manifest = Manifest::from_toml_str(&format!(
        r#"[package]
id = "org.example.notes"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/notes"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let bytes = tapp(&identity, &manifest, false);
    let (store, catalog) = store(dir.path());
    let refused = installer::install_dev(
        &store,
        &catalog,
        &bytes,
        &identity,
        "dev.tapp",
    )
    .unwrap_err();
    assert!(
        matches!(&refused, installer::Error::Policy(said) if said.contains("names no device")),
        "an unmarked package is no dev build: {refused:?}"
    );
}
