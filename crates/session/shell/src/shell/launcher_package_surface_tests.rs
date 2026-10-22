//! A package the shell launches keeps the person's records it declares in
//! its signed surface as a built-in app does: the launch takes the surface
//! from the signed package and hands it to the identity service and broker.
//! A surface changed after install declares nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    let (payload, manifest_path) =
        crate::shell::test_support::install_signed_package(root, id, "0.1.0", files);
    InstalledApp {
        package_id: id.into(),
        display_name: id.into(),
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        version: "0.1.0".into(),
        manifest_path,
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
        Path::new("/usr/libexec/tilo/tilo-launcher"),
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
