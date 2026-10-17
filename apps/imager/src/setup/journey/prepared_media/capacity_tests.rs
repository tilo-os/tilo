use super::*;
use crate::setup::{
    account_transfer, journey::browser::SelectedSources, portable_home_media::HomeSource,
};
use imager_backend::setup::browser_profiles;
use std::os::unix::fs::PermissionsExt;

#[test]
fn shared_home_generations_rebuild_capacity_from_their_own_authenticated_artifacts() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let source = root.path().join("source");
    let profile = source.join(".config/chromium/Default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Bookmarks"), b"actual selected browser bytes").unwrap();
    std::fs::write(source.join("document"), b"whole home document").unwrap();
    let inventory = account_transfer::discover(&account_transfer::SourceRoots {
        home: source.clone(),
        config: source.join(".config"),
    });
    let selected = SelectedSources::from_reviewed(
        browser_profiles::discover(&source, &inventory.candidates)
            .unwrap()
            .into_iter()
            .map(|source| {
                let mut profile = source.profile.clone();
                profile.history = false;
                (Arc::new(source), profile)
            })
            .collect(),
    )
    .unwrap();
    let identity = PreparedIdentity::create(
        &root.path().join("identity"),
        "Ada",
        [20, 80, 140],
        tilo_identity_host::provisioning::IdentityStorage::for_session(),
    )
    .unwrap();
    let original = PreparedMedia::prepare_with_selections(
        &identity,
        None,
        MediaSelections {
            sessions: None,
            home_applications: false,
            home: Some(HomeSource::review(&source).unwrap()),
            retained: None,
            planned_direct: None,
            applications: None,
            browser: Some(selected),
        },
        Zeroizing::new("a sufficiently long transport password".into()),
        "shared-home-capacity".into(),
        None,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let fresh = original.refreshed_for_transfer(&cancel).unwrap();
    assert!(!Arc::ptr_eq(&original, &fresh));
    assert!(Arc::ptr_eq(
        original.home.as_ref().unwrap(),
        fresh.home.as_ref().unwrap()
    ));
    // Cached totals are observation, never authority. A different generation's
    // lagging total must not influence recomputation from current held custody.
    original.reserved_data_bytes.store(0, Ordering::Release);
    fresh.reserved_data_bytes.store(1, Ordering::Release);
    for generation in [&original, &fresh] {
        generation.validate_for_write(&cancel).unwrap();
        let home = generation.home.as_ref().unwrap().lock().unwrap();
        let expected = generation.sealed_bytes
            + home.reservation
            + generation
                .browser
                .as_ref()
                .unwrap()
                .expectation
                .sealed_bytes
            + 128 * 1024;
        assert_eq!(
            generation.reserved_data_bytes.load(Ordering::Acquire),
            expected
        );
        assert_eq!(
            std::fs::metadata(generation.package()).unwrap().len(),
            generation.sealed_bytes
        );
    }
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"whole home document"
    );
    assert_eq!(
        std::fs::read(profile.join("Bookmarks")).unwrap(),
        b"actual selected browser bytes"
    );
}
