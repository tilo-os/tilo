use super::*;
use crate::setup::{
    account_transfer::{self, SourceRoots},
    browser_transfer,
};
use imager_backend::setup::browser_profiles;
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn browser_only_preparation_authenticates_identity_and_keeps_only_ciphertext() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let private = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = private.path();
    let home = root.join("source");
    let profile = home.join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("Bookmarks"), b"selected personal bookmarks").unwrap();
    fs::write(profile.join("Cookies"), b"unselected credentials").unwrap();
    fs::write(home.join("document"), b"unselected document").unwrap();
    let inventory = account_transfer::discover(&SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let selected = super::super::browser::SelectedSources::from_reviewed(
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
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [20, 80, 140],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let password = "a sufficiently long transport password";
    let media = PreparedMedia::prepare_with_selections(
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
            browser: Some(selected),

            other_device: None,
            tpm_target: None,
        },
        Zeroizing::new(password.into()),
        "browser-only-setup".into(),
        None,
    )
    .unwrap();
    assert!(media.home.is_none());
    assert!(media.applications.is_none());
    assert!(Delivery::with_artifact(media.clone()).requires_data_partition());
    assert_eq!(fs::read_dir(media._custody.path()).unwrap().count(), 2);
    let setup = prepared_setup::PreparedSetupReview::open(
        &media.package(),
        password.as_bytes(),
        &root.join("setup-review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let declaration = setup.manifest().browser.as_ref().unwrap();
    assert_eq!(declaration.plan.identity_public_key, identity.public_key());
    assert_eq!(declaration.plan.setup_binding, "browser-only-setup");
    assert!(setup.manifest().portable_home.is_none());
    assert!(setup.manifest().portable_flatpak.is_none());
    declaration
        .payload
        .validate_plan(&declaration.plan)
        .unwrap();
    let package = media._custody.path().join(&declaration.payload.filename);
    let projection = browser_transfer::extract(
        &declaration.payload,
        &declaration.plan,
        &package,
        Zeroizing::new(password.as_bytes().to_vec()),
        root,
        &AtomicBool::new(false),
    )
    .unwrap();
    let profile = projection
        .tree
        .root
        .join(imager_backend::browser_selection::PROFILES_DIRECTORY)
        .join(&declaration.plan.profiles[0].id);
    assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
    assert_eq!(
        fs::read(profile.join("Bookmarks")).unwrap(),
        b"selected personal bookmarks"
    );
    assert!(!projection.tree.root.join("document").exists());
    assert_eq!(
        fs::read(home.join("document")).unwrap(),
        b"unselected document"
    );
    assert_eq!(
        fs::read(home.join(".config/chromium/Default/Cookies")).unwrap(),
        b"unselected credentials"
    );
    assert!(!media.projection().to_string().contains(password));
    assert!(
        !media
            .projection()
            .to_string()
            .contains("personal bookmarks")
    );
}
