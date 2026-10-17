use super::*;
use crate::setup::{
    account_transfer::{self, SourceRoots},
    browser_profiles,
};
use std::{fs, sync::Arc};

fn private_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

fn selected(home: &Path, history: bool) -> SelectedSources {
    let inventory = account_transfer::discover(&SourceRoots {
        home: home.into(),
        config: home.join(".config"),
    });
    SelectedSources::from_reviewed(
        browser_profiles::discover(home, &inventory.candidates)
            .unwrap()
            .into_iter()
            .map(|source| {
                let mut profile = source.profile.clone();
                profile.history &= history;
                (Arc::new(source), profile)
            })
            .collect(),
    )
    .unwrap()
}
fn fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let profile = home.path().join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("Bookmarks"), b"actual bookmark snapshot").unwrap();
    fs::write(profile.join("History"), b"actual history snapshot").unwrap();
    fs::write(profile.join("History-wal"), b"stable WAL").unwrap();
    fs::write(profile.join("Cookies"), b"unselected login credentials").unwrap();
    fs::write(profile.join("History-shm"), b"unselected runtime state").unwrap();
    home
}
#[test]
fn actual_carrier_authenticates_selected_domains_without_home_or_credentials() {
    let home = fixture();
    let private = private_directory();
    let selected = selected(home.path(), false);
    let plan = selected.plan("setup-binding".into(), [7; 32]).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let package = private.path().join(format!("browser-{operation}.sealed"));
    let cancel = AtomicBool::new(false);
    let expected = export(
        &selected,
        &plan,
        ExportRequest {
            staging_parent: private.path(),
            package: &package,
            operation: &operation,
            password: Zeroizing::new(b"sample password".to_vec()),
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(
        fs::read_dir(private.path()).unwrap().count(),
        1,
        "plaintext stage must be gone"
    );
    let quarantine = private.path();
    let tree = extract(
        &expected,
        &plan,
        &package,
        Zeroizing::new(b"sample password".to_vec()),
        quarantine,
        &cancel,
    )
    .unwrap();
    let profile = tree
        .tree
        .root
        .join(PROFILES_DIRECTORY)
        .join(&plan.profiles[0].id);
    assert_eq!(
        fs::read(profile.join("Bookmarks")).unwrap(),
        b"actual bookmark snapshot"
    );
    assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
    assert!(!profile.join("History").exists());
    assert!(!profile.join("Cookies").exists());
    assert!(!profile.join("History-shm").exists());
    assert_eq!(
        fs::read(home.path().join(".config/chromium/Default/Cookies")).unwrap(),
        b"unselected login credentials"
    );
    let mut wrong = plan.clone();
    wrong.identity_public_key = [8; 32];
    wrong.validate().unwrap();
    assert_eq!(wrong.setup_binding, plan.setup_binding);
    assert_eq!(wrong.profiles, plan.profiles);
    assert!(expected.validate_plan(&wrong).is_err());
    assert!(review(&expected, &wrong, &package, b"sample password", &cancel).is_err());
    assert!(review(&expected, &plan, &package, b"wrong password", &cancel).is_err());
    fs::write(profile.join("Cookies"), b"not approved").unwrap();
    assert!(verify_tree(&tree.tree.root, &plan, &cancel).is_err());
    drop(tree);
    assert_eq!(fs::read_dir(private.path()).unwrap().count(), 1);
}
#[test]
fn cancellation_and_replaced_grants_leave_no_plaintext_or_ready_package() {
    let home = fixture();
    let private = private_directory();
    let selected = selected(home.path(), true);
    let plan = selected.plan("setup-binding".into(), [7; 32]).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let package = private.path().join(format!("browser-{operation}.sealed"));
    let cancel = AtomicBool::new(true);
    assert!(
        export(
            &selected,
            &plan,
            ExportRequest {
                staging_parent: private.path(),
                package: &package,
                operation: &operation,
                password: Zeroizing::new(b"sample password".to_vec())
            },
            &cancel
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(private.path()).unwrap().count(), 0);
    let profile = home.path().join(".config/chromium/Default");
    fs::rename(&profile, home.path().join("original")).unwrap();
    fs::create_dir(&profile).unwrap();
    assert!(
        export(
            &selected,
            &plan,
            ExportRequest {
                staging_parent: private.path(),
                package: &package,
                operation: &operation,
                password: Zeroizing::new(b"sample password".to_vec())
            },
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(private.path()).unwrap().count(), 0);
    assert_eq!(
        fs::read(home.path().join("original/History")).unwrap(),
        b"actual history snapshot"
    );
}

#[test]
fn history_and_wal_are_carried_together_and_ciphertext_changes_are_refused() {
    let home = fixture();
    let private = private_directory();
    let selected = selected(home.path(), true);
    let plan = selected.plan("setup-binding".into(), [7; 32]).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let package = private.path().join(format!("browser-{operation}.sealed"));
    let cancel = AtomicBool::new(false);
    let expected = export(
        &selected,
        &plan,
        ExportRequest {
            staging_parent: private.path(),
            package: &package,
            operation: &operation,
            password: Zeroizing::new(b"sample password".to_vec()),
        },
        &cancel,
    )
    .unwrap();
    let projection = extract(
        &expected,
        &plan,
        &package,
        Zeroizing::new(b"sample password".to_vec()),
        private.path(),
        &cancel,
    )
    .unwrap();
    let profile = projection
        .tree
        .root
        .join(PROFILES_DIRECTORY)
        .join(&plan.profiles[0].id);
    assert_eq!(
        fs::read(profile.join("History")).unwrap(),
        b"actual history snapshot"
    );
    assert_eq!(
        fs::read(profile.join("History-wal")).unwrap(),
        b"stable WAL"
    );
    assert_eq!(fs::read_dir(profile).unwrap().count(), 3);
    drop(projection);
    let mut cipher = fs::read(&package).unwrap();
    let index = cipher.len() / 2;
    cipher[index] ^= 1;
    fs::write(&package, cipher).unwrap();
    assert!(
        extract(
            &expected,
            &plan,
            &package,
            Zeroizing::new(b"sample password".to_vec()),
            private.path(),
            &cancel
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(private.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read(home.path().join(".config/chromium/Default/History-wal")).unwrap(),
        b"stable WAL"
    );
}
