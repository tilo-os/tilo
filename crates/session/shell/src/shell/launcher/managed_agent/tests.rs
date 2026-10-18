//! The managed path verifies installed bytes independently of launch metadata.
use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use tilo_app_package::{BlobHash, Collection, SignedRoot};

fn install(root: &std::path::Path) -> InstalledApp {
    let manifest = crate::shell::test_support::canonical_manifest(
        &format!("id = {WORKSPACE_AGENT_PACKAGE_ID:?}\nversion = \"0.1.0\"\nkind = \"service\""),
        "[entrypoint]\nbin = \"agent\"\ntype = \"cli\"\n",
    );
    let collection = Collection::from_entries([("agent".into(), BlobHash::of(b"executor"))]);
    let collection = collection.encode();
    let key = SigningKey::from_bytes(&[7; 32]);
    let mut signed = SignedRoot::unsigned(
        BlobHash::of(manifest.as_bytes()),
        BlobHash::of(&collection),
        key.verifying_key().to_bytes(),
    );
    signed.signature = Some([0; 64]);
    signed.signature = Some(key.sign(&signed.signing_bytes()).to_bytes());
    let signed = signed.encode();
    let hash = BlobHash::of(&signed).to_hex();
    let payload = root.join("pkgs").join(&hash);
    let metadata = root.join("pkgs").join(format!("{hash}.pkg"));
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::create_dir_all(&metadata).unwrap();
    std::fs::write(payload.join("agent"), b"executor").unwrap();
    std::fs::write(metadata.join("signed-root"), signed).unwrap();
    std::fs::write(metadata.join("manifest.toml"), &manifest).unwrap();
    std::fs::write(metadata.join("collection.qcl"), &collection).unwrap();
    let mut registry = String::new();
    write!(registry,
        "[[entry]]\nid = {WORKSPACE_AGENT_PACKAGE_ID:?}\nversion = \"0.1.0\"\npackage_id_hex = {hash:?}\npayload_root = {:?}\nmanifest_path = {:?}\npublisher_key_hex = {:?}\n",
        payload.display().to_string(), metadata.join("manifest.toml").display().to_string(), hex::encode(key.verifying_key().to_bytes())
    ).unwrap();
    std::fs::write(package_store::Layout::new(root).registry(), registry).unwrap();
    InstalledApp {
        package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
        display_name: "Executor".into(),
        localized_names: BTreeMap::default(),
        keywords: vec![],
        category: None,
        version: "0.1.0".into(),
        manifest_path: metadata.join("manifest.toml"),
        payload_root: payload,
        has_gui: false,
        opens_extensions: vec![],
        opens_mime_types: vec![],
        opens_uri_schemes: vec![],
        opens_secondary: false,
        offers: vec![],
    }
}

#[test]
#[serial_test::serial(process_env)]
fn managed_package_hash_requires_the_actual_signed_payload() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let app = install(&root);
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", &root);
    // Host trust pins this exact package, independently of its self-signature.
    let expected = package_store::find_installed(WORKSPACE_AGENT_PACKAGE_ID)
        .unwrap()
        .unwrap()
        .package_id_hex;
    assert!(verify_installed_package_hash(&app, &"00".repeat(32)).is_err());
    let verified = verify_installed_package_hash(&app, &expected).unwrap();
    assert_eq!(
        verified,
        package_store::find_installed(WORKSPACE_AGENT_PACKAGE_ID)
            .unwrap()
            .unwrap()
            .package_id_hex
    );
    std::fs::write(app.payload_root.join("agent"), b"modified").unwrap();
    assert!(
        verify_installed_package_hash(&app, &expected).is_err(),
        "signed metadata alone is not payload verification"
    );
    std::fs::write(app.payload_root.join("agent"), b"executor").unwrap();
    std::fs::write(app.payload_root.join("extra"), b"injected").unwrap();
    assert!(
        verify_installed_package_hash(&app, &expected).is_err(),
        "managed launches reject unrecorded payload files"
    );
    std::fs::remove_file(app.payload_root.join("extra")).unwrap();
    std::fs::write(&app.manifest_path, b"modified").unwrap();
    assert!(verify_installed_package_hash(&app, &expected).is_err());
}

#[test]
fn another_package_cannot_supply_managed_custody() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = install(directory.path());
    app.package_id = "so.tilo.other".into();
    assert!(verify_installed_package_hash(&app, &"00".repeat(32)).is_err());
}
