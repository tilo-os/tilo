//! Portable packaging must retain user data while excluding installer custody.
use super::{OriginalPreparation, SourceInventory};
use crate::setup::{
    migration::PinnedDirectory,
    portable_transfer::{self, ExportWork, ImportReview, Limits, Purpose},
};
use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};
use zeroize::Zeroizing;

#[test]
fn original_omission_roundtrip_preserves_full_user_home_and_rejects_replaced_namespace() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let state = home.join(".local/state");
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", &home);
    environment.set("XDG_STATE_HOME", &state);
    let namespace = state.join("tilo/installer/preparations");
    fs::create_dir_all(&namespace).unwrap();
    fs::set_permissions(&namespace, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(namespace.join("identity-secret"), b"installer-only-secret").unwrap();
    fs::create_dir_all(home.join(".var/app/real-app")).unwrap();
    fs::write(
        home.join(".var/app/real-app/account"),
        b"user-flatpak-state",
    )
    .unwrap();
    fs::create_dir_all(home.join(".hidden/private")).unwrap();
    fs::write(home.join(".hidden/private/settings"), b"user-private-state").unwrap();
    fs::write(home.join("document"), b"user-document").unwrap();
    let preparation = OriginalPreparation::review(&home).unwrap().unwrap();
    let proof = preparation
        .validate_original(&home, rustix::process::getuid().as_raw())
        .unwrap();
    environment.set("HOME", root.join("unrelated-receiver"));
    environment.set("XDG_STATE_HOME", root.join("unrelated-state"));
    let pin = PinnedDirectory::open_absolute(&home).unwrap();
    let inventory =
        SourceInventory::capture_original_home(&pin, std::slice::from_ref(&proof)).unwrap();
    assert_eq!(inventory.summary().unwrap().files, 3);
    let limits = Limits {
        plaintext_bytes: 4 * 1024 * 1024,
        entries: 128,
        metadata_bytes: 1024 * 1024,
    };
    let package = root.join("home.sealed");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&package)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let password = b"synthetic fixture password";
    let mut observer = |_| Ok(());
    let info = portable_transfer::export_stream_reporting(
        &pin,
        &inventory,
        Purpose::Home,
        limits,
        &mut file,
        &mut ExportWork {
            password,
            cancel: &cancel,
            setup_binding: Some("fixture-binding"),
            observer: &mut observer,
        },
    )
    .unwrap();
    assert!(info.sealed_bytes <= limits.sealed_bound().unwrap());
    drop(file);
    let imported =
        ImportReview::review(&package, password, Purpose::Home, limits, &cancel).unwrap();
    let destination = root.join("imported");
    let imported_tree = imported
        .extract(Zeroizing::new(password.to_vec()), &destination, &cancel)
        .unwrap();
    assert_eq!(
        fs::read(imported_tree.root.join("document")).unwrap(),
        b"user-document"
    );
    assert_eq!(
        fs::read(imported_tree.root.join(".var/app/real-app/account")).unwrap(),
        b"user-flatpak-state"
    );
    assert_eq!(
        fs::read(imported_tree.root.join(".hidden/private/settings")).unwrap(),
        b"user-private-state"
    );
    assert!(
        !imported_tree
            .root
            .join(".local/state/tilo/installer/preparations")
            .exists()
    );
    assert_eq!(
        fs::read(namespace.join("identity-secret")).unwrap(),
        b"installer-only-secret"
    );
    fs::rename(&namespace, namespace.with_extension("retired")).unwrap();
    fs::create_dir(&namespace).unwrap();
    fs::set_permissions(&namespace, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        preparation
            .validate_original(&home, rustix::process::getuid().as_raw())
            .is_err()
    );
    assert!(inventory.validate(&pin).is_err());
    assert!(SourceInventory::capture_original_home(&pin, &[proof]).is_err());
}
