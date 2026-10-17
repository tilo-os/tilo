//! A package the shell launches keeps the person's records it declares in
//! its signed surface as a built-in app does: the launch takes the surface
//! from the signed package and hands it to the identity service and broker.
//! A surface changed after install declares nothing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_app_package::{BlobHash, Collection, SignedRoot};
use tilo_identity_host::broker::PersonRecordKinds as _;

use super::{InstalledApp, build_launch_command};
use crate::shell::state::RoomId;

/// A surface whose app keeps the person's `{kind}.history`.
fn surface(kind: &str) -> String {
    format!(
        "[[resources]]\nkind = \"{kind}.page\"\nlabel = \"Page\"\nversion = 1\n\n\
         [[resources]]\nkind = \"{kind}.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n"
    )
}

/// Install `files` as the signed GUI package `id` in the user store at
/// `root`, as an install leaves it; the app the shell launches.
fn install(root: &Path, id: &str, files: &[(&str, &[u8])]) -> InstalledApp {
    let manifest = crate::shell::test_support::canonical_manifest(
        &format!("id = \"{id}\"\nversion = \"0.1.0\"\nkind = \"app\""),
        "[entrypoint]\nbin = \"probe\"\ntype = \"gui\"\n",
    );
    let collection = Collection::from_entries(
        files
            .iter()
            .map(|(path, bytes)| ((*path).to_string(), BlobHash::of(bytes))),
    );
    let collection_bytes = collection.encode();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let mut signed = SignedRoot::unsigned(
        BlobHash::of(manifest.as_bytes()),
        BlobHash::of(&collection_bytes),
        key.verifying_key().to_bytes(),
    );
    // The has-signature bit is part of the signed preimage.
    signed.signature = Some([0; 64]);
    signed.signature = Some(key.sign(&signed.signing_bytes()).to_bytes());
    let signed_bytes = signed.encode().to_vec();
    let id_hex = BlobHash::of(&signed_bytes).to_hex();
    let payload = root.join("pkgs").join(&id_hex);
    for (path, bytes) in files {
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join(path), bytes).unwrap();
    }
    let meta = root.join("pkgs").join(format!("{id_hex}.pkg"));
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("signed-root"), &signed_bytes).unwrap();
    std::fs::write(meta.join("manifest.toml"), &manifest).unwrap();
    std::fs::write(meta.join("collection.qcl"), &collection_bytes).unwrap();
    let registry = package_store::Layout::new(root).registry();
    let mut rows = std::fs::read_to_string(&registry).unwrap_or_default();
    write!(
        rows,
        "[[entry]]\nid = {id:?}\nversion = \"0.1.0\"\npackage_id_hex = {id_hex:?}\n\
         payload_root = {:?}\nmanifest_path = {:?}\npublisher_key_hex = {:?}\n\n",
        payload.display().to_string(),
        meta.join("manifest.toml").display().to_string(),
        hex::encode(key.verifying_key().to_bytes()),
    )
    .unwrap();
    std::fs::write(&registry, rows).unwrap();
    InstalledApp {
        package_id: id.into(),
        display_name: id.into(),
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        version: "0.1.0".into(),
        manifest_path: meta.join("manifest.toml"),
        payload_root: payload,
        has_gui: true,
        opens_extensions: Vec::new(),
        opens_mime_types: Vec::new(),
        opens_uri_schemes: Vec::new(),
        opens_secondary: false,
        offers: Vec::new(),
    }
}

fn launch(app: &InstalledApp) {
    let _ = build_launch_command(
        app,
        Path::new("/usr/bin/tilo-launcher"),
        Path::new("/run/user/1000/wayland-3"),
        RoomId::new(),
        None,
        None,
        None,
        None,
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_launched_package_declares_the_persons_records_its_signed_surface_names() {
    const KEEPER: &str = "so.tilo.shell-person-keeper";
    const TAMPERED: &str = "so.tilo.shell-person-tampered";
    let temp = tempfile::tempdir().unwrap();
    let root: PathBuf = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::create_dir_all(root.join("pkgs")).unwrap();
    let keeper_surface = surface("shell-person-keeper");
    let keeper = install(
        &root,
        KEEPER,
        &[
            ("probe", b"binary"),
            ("surface.toml", keeper_surface.as_bytes()),
        ],
    );
    let tampered = install(
        &root,
        TAMPERED,
        &[
            ("probe", b"binary"),
            ("surface.toml", surface("shell-person-tampered").as_bytes()),
        ],
    );
    std::fs::write(
        tampered.payload_root.join("surface.toml"),
        surface("shell-person-forged"),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &root);
    let declared = |app: &str| tilo_action_broker::SurfacePersonRecords.person_record_kinds(app);
    assert!(declared(KEEPER).is_empty(), "nothing before it is launched");

    launch(&keeper);
    launch(&tampered);
    env.remove("TILO_APPS_DATA_DIR");

    assert_eq!(declared(KEEPER), ["shell-person-keeper.history"]);
    assert!(
        declared(TAMPERED).is_empty(),
        "a surface that is not the signed one declares nothing"
    );
}
