use super::*;
use crate::setup::{
    account_transfer::{self, SourceRoots},
    journey::{browser::SelectedSources, prepared_media::MediaSelections},
    prepared_identity::PreparedIdentity,
};
use imager_backend::setup::browser_profiles;
use std::{fs, os::unix::fs::PermissionsExt};
use zeroize::Zeroizing;
fn fixture(root: &Path) -> Arc<PreparedMedia> {
    let source = root.join("source");
    let profile = source.join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("Bookmarks"), b"my bookmarks").unwrap();
    fs::write(profile.join("Cookies"), b"private login").unwrap();
    let inventory = account_transfer::discover(&SourceRoots {
        home: source.clone(),
        config: source.join(".config"),
    });
    let browser = SelectedSources::from_reviewed(
        browser_profiles::discover(&source, &inventory.candidates)
            .unwrap()
            .into_iter()
            .map(|source| {
                let profile = source.profile.clone();
                (Arc::new(source), profile)
            })
            .collect(),
    )
    .unwrap();
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [20, 80, 140],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    PreparedMedia::prepare_with_selections(
        &identity,
        None,
        MediaSelections {
            sessions: None,
            mail: None,
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: None,
            browser: Some(browser),

            other_device: None,
            tpm_target: None,
        },
        Zeroizing::new("a sufficiently long transport password".into()),
        "desktop-continuity".into(),
        None,
    )
    .unwrap()
}
fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[test]
fn desktop_delivery_publishes_private_ciphertext_idempotently_without_activation() {
    let private = root();
    let prepared = fixture(private.path());
    let source = prepared
        .refreshed_for_transfer(&AtomicBool::new(false))
        .unwrap()
        .staging_artifact()
        .unwrap();
    let home = private.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::create_dir(home.join(".local")).unwrap();
    fs::set_permissions(home.join(".local"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(home.join(".config")).unwrap();
    fs::write(
        home.join(".config/existing-account"),
        b"unchanged account settings",
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let receipt = deliver(Arc::clone(&prepared), &home, &cancel).unwrap();
    fs::write(
        private
            .path()
            .join("source/.config/chromium/Default/Bookmarks"),
        b"browser activity after publication",
    )
    .unwrap();
    assert_eq!(
        deliver(Arc::clone(&prepared), &home, &cancel).unwrap(),
        receipt
    );
    let destination = home.join(".local/state/tilo/prepared-setup");
    assert_eq!(
        fs::read(destination.join("prepared-setup.sealed")).unwrap(),
        fs::read(&source.source_path).unwrap()
    );
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(destination.join("prepared-setup.sealed"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<crate::model::PreparedSetupExpectation>(
            &fs::read(destination.join("receipt.json")).unwrap()
        )
        .unwrap(),
        receipt.setup
    );
    let browser = source.browser.unwrap();
    let target = home.join(".local/state/tilo/prepared-browser");
    assert_eq!(
        fs::read(target.join(&browser.expectation.filename)).unwrap(),
        fs::read(&browser.source_path).unwrap()
    );
    assert_eq!(
        serde_json::from_slice::<super::super::browser_transfer::Expectation>(
            &fs::read(target.join("expectation.json")).unwrap()
        )
        .unwrap(),
        browser.expectation
    );
    assert!(!home.join(".config/tilo/identity").exists());
    assert_eq!(
        fs::metadata(home.join(".local")).unwrap().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read(home.join(".config/existing-account")).unwrap(),
        b"unchanged account settings"
    );
    assert_eq!(
        fs::read(
            private
                .path()
                .join("source/.config/chromium/Default/Cookies")
        )
        .unwrap(),
        b"private login"
    );
}
#[test]
fn conflicting_custody_and_cancellation_preserve_existing_data_without_browser_publication() {
    let private = root();
    let prepared = fixture(private.path());
    let home = private.path().join("home");
    fs::create_dir(&home).unwrap();
    assert!(deliver(Arc::clone(&prepared), &home, &AtomicBool::new(true)).is_err());
    assert!(!home.join(".local").exists());
    let state = home.join(".local/state/tilo/prepared-setup");
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let old = state.join("prepared-setup.sealed");
    fs::write(&old, b"existing different preparation").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(deliver(prepared, &home, &AtomicBool::new(false)).is_err());
    assert_eq!(fs::read(old).unwrap(), b"existing different preparation");
    assert!(!state.join("receipt.json").exists());
    assert!(!home.join(".local/state/tilo/prepared-browser").exists());
}

#[test]
fn cancellation_before_readiness_removes_new_payloads_and_preserves_existing_directory() {
    let private = root();
    let prepared = fixture(private.path());
    let artifact = prepared.staging_artifact().unwrap();
    let caller = super::super::staging::prepared_setup::caller_uid().unwrap();
    let setup = artifact.read(caller).unwrap();
    let browser = artifact.browser.as_ref().unwrap().read(caller).unwrap();
    let destination = private.path().join("pending-target");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(destination.join("existing-note"), b"keep me").unwrap();
    let directory = PinnedDirectory::open_absolute(&destination).unwrap();
    let cancel = AtomicBool::new(false);
    {
        let mut publication = Publication {
            files: Vec::new(),
            directories: Vec::new(),
            committed: false,
        };
        let (setup_root, _) = publish_payloads(
            &mut publication,
            &directory,
            &artifact,
            &setup,
            Some(&browser),
            &cancel,
        )
        .unwrap();
        assert!(
            destination
                .join("prepared-browser")
                .join(&artifact.browser.as_ref().unwrap().expectation.filename)
                .exists()
        );
        assert!(
            destination
                .join("prepared-setup/prepared-setup.sealed")
                .exists()
        );
        assert!(!destination.join("prepared-setup/receipt.json").exists());
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            publication
                .publish(&setup_root, "receipt.json", b"not ready", &cancel)
                .is_err()
        );
    }
    assert!(!destination.join("prepared-browser").exists());
    assert!(!destination.join("prepared-setup").exists());
    assert_eq!(
        fs::read(destination.join("existing-note")).unwrap(),
        b"keep me"
    );
    assert!(Path::new(&artifact.source_path).exists());
    assert!(Path::new(&artifact.browser.unwrap().source_path).exists());
}
