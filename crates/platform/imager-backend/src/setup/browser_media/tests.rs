use super::*;
use crate::browser_selection::SelectedSources;
use crate::setup::{
    account_transfer::{self, SourceRoots},
    browser_profiles,
    browser_transfer::{self, ExportRequest},
    // Selected browser custody is shared with the native export actor.
};
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};
use zeroize::Zeroizing;

#[test]
fn real_browser_delivery_reuses_verified_ciphertext_and_refuses_changed_media() {
    let private = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home = private.path().join("source");
    let profile = home.join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("Bookmarks"), b"selected bookmarks").unwrap();
    fs::write(profile.join("Cookies"), b"unselected secret").unwrap();
    let inventory = account_transfer::discover(&SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let selected = SelectedSources::from_reviewed(
        browser_profiles::discover(&home, &inventory.candidates)
            .unwrap()
            .into_iter()
            .map(|source| {
                let profile = source.profile.clone();
                (Arc::new(source), profile)
            })
            .collect(),
    )
    .unwrap();
    let plan = selected.plan("exact setup".into(), [3; 32]).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let package = private.path().join(format!("browser-{operation}.sealed"));
    let cancel = AtomicBool::new(false);
    let expected = browser_transfer::export(
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
    let mut archive =
        browser_transfer::review(&expected, &plan, &package, b"sample password", &cancel).unwrap();
    let destination = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let media = MediaGrant::open(destination.path()).unwrap();
    assert_eq!(
        media.browser_allocation(&expected, &cancel).unwrap(),
        expected.sealed_bytes
    );
    media
        .deliver_browser(&mut archive, &expected, &cancel)
        .unwrap();
    let published = destination.path().join(&expected.filename);
    let first = fs::read(&published).unwrap();
    assert_eq!(first, fs::read(&package).unwrap());
    assert_eq!(media.browser_allocation(&expected, &cancel).unwrap(), 0);
    media
        .deliver_browser(&mut archive, &expected, &cancel)
        .unwrap();
    assert_eq!(fs::read(&published).unwrap(), first);
    let receipt = destination.path().join(format!("{operation}.receipt.json"));
    assert_eq!(
        serde_json::from_slice::<Expectation>(&fs::read(receipt).unwrap()).unwrap(),
        expected
    );
    fs::write(&published, b"different package").unwrap();
    assert!(media.browser_allocation(&expected, &cancel).is_err());
    assert!(
        media
            .deliver_browser(&mut archive, &expected, &cancel)
            .is_err()
    );
    assert_eq!(fs::read(published).unwrap(), b"different package");
    assert_eq!(
        fs::read(profile.join("Cookies")).unwrap(),
        b"unselected secret"
    );
}
