// Opt-in acceptance of the real current-owner preparation path, on disposable custody.
use super::*;
use ed25519_dalek::Signer as _;
use std::os::unix::fs::PermissionsExt as _;

fn private(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn current_owner_desktop_actions_restart_and_authenticated_import_preserve_original() {
    // SAFETY: credential observation only. This opt-in test requires real root custody.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "requires native root-owned host qualification"
    );
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    private(&root);
    let home = root.join("home");
    let config = home.join(".config");
    let identity = config.join("tilo/identity");
    let state = home.join(".local/state");
    let system = root.join("host");
    for directory in [
        &home,
        &config,
        &identity,
        &state,
        &system,
        &identity.join("fleets"),
    ] {
        private(directory);
    }
    let signer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let public_key = signer.verifying_key().to_bytes();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Existing laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    cert.signature = signer
        .sign(&tilo_user_identity::device_cert_signing_digest(&cert).unwrap())
        .to_bytes();
    let paths = tilo_user_identity::persist::IdentityPaths::under(&system);
    paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
    tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &public_key).unwrap();
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert).unwrap();
    let record = identity.join("member.json");
    let original = runtime_identity::public_member_record("Ada", [1, 2, 3], public_key).unwrap();
    tilo_files::native::atomic::write_with_mode(&record, &original, 0o600).unwrap();
    let mut fleet = tilo_identity_host::device_fleet::DeviceFleet::new(hex::encode(public_key));
    fleet.certificates.push(cert);
    tilo_files::native::atomic::write_with_mode(
        &identity
            .join("fleets")
            .join(format!("{}.json", hex::encode(public_key))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    runtime_identity::set_active_identity(&identity, "member").unwrap();
    let registry_store = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
    let mut registry = registry_store.load().unwrap();
    registry.register_record(&record).unwrap();
    registry_store.save(&registry).unwrap();
    let before = [
        std::fs::read(&record).unwrap(),
        std::fs::read(&paths.device_privkey).unwrap(),
        std::fs::read(&paths.device_cert).unwrap(),
        std::fs::read(identity.join("active")).unwrap(),
    ];
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", &home);
    environment.set("XDG_CONFIG_HOME", &config);
    environment.set("XDG_STATE_HOME", &state);
    environment.set("TILO_IDENTITY_ROOT", &system);
    let mut app = fixture();
    // Qualify retained prepared migrations; new Desktop choices bypass identity.
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.journey.destination = Some(Destination::Desktop);
        preview.journey.page = Page::Migration;
        preview.journey.categories = [false, false, true];
        preview.dirty = true;
    })
    .unwrap();
    choose(&mut app, "migrate");
    app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
        .unwrap();
    choose(&mut app, "existing");
    choose(&mut app, "current");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.settle();
        if app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.ready() && preview.existing_identity.ready()
            })
            .unwrap()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "real current-owner worker must settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(status(&mut app)["page"], "connect");
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.journey.password = "long current owner preparation password".into();
        preview.journey.confirmation = "long current owner preparation password".into();
        preview.dirty = true;
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.journey.page = Page::Password;
        preview.dirty = true;
    })
    .unwrap();
    assert_eq!(status(&mut app)["page"], "password");
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.device_label_locked());
        assert_eq!(
            preview.device_label_locked_message(),
            "installer-current-device-name-locked"
        );
        let row = preview.review_device_row();
        assert!(
            row.accessibility()
                .unwrap()
                .name
                .unwrap()
                .contains("Existing laptop")
        );
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SetPreviewPassword {
        value: "long current owner preparation password".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
        value: "long current owner preparation password".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(status(&mut app)["page"], "devices");
    assert!(
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: "Replacement laptop".into()
        })
        .is_err()
    );
    assert_eq!(status(&mut app)["device_name"], "Existing laptop");

    let choices = app
        .with_widget::<InstallerPreview, _>(|preview| preview.journey.draft_choices())
        .unwrap();
    let store = draft::DraftStore::for_session().unwrap();
    let draft::DraftStatus::Ready(ready) = store.review().unwrap() else {
        panic!("actual acknowledged current-owner draft");
    };
    assert_eq!(ready.kind(), draft::IdentityKind::CurrentDevice);
    assert_eq!(ready.choices(), &choices);
    let resumed = ready.resume().unwrap();
    assert_eq!(
        resumed.current_declaration().unwrap().unwrap().public_key(),
        public_key
    );
    assert!(resumed.update_name("Replacement").is_err());
    let password = "long current owner preparation password";
    let mut profile = crate::model::InstallProfile::default();
    profile.user.full_name = "Ada".into();
    profile.device_label = "Existing laptop".into();
    let profile = crate::setup::prepared_setup::PublicInstallProfile::from_draft(&profile).unwrap();
    let prepared = prepared_media::PreparedMedia::prepare_profiled_for_test(
        &resumed,
        zeroize::Zeroizing::new(password.into()),
        "current-owner-desktop".into(),
        profile,
    )
    .unwrap();
    assert!(matches!(
        prepared.desktop_manifest().unwrap().identity_kind,
        crate::setup::prepared_setup::IdentityKind::CurrentDevice { .. }
    ));
    let package = prepared.package();
    let mut output = Vec::new();
    let args = [
        "--package".into(),
        package.into_os_string(),
        "--quarantine".into(),
        root.join("quarantine").into_os_string(),
        "--identity-dir".into(),
        identity.clone().into_os_string(),
        "--config-root".into(),
        config.clone().into_os_string(),
    ];
    crate::setup::prepared_setup_helper::run(args, &mut password.as_bytes(), &mut output).unwrap();
    let receipt: crate::setup::prepared_setup_helper::Receipt =
        serde_json::from_slice(&output).unwrap();
    let admitted = receipt
        .current_identity
        .unwrap()
        .resolve_current(&system, &identity)
        .unwrap();
    assert_eq!(admitted.load().unwrap().group_public_key(), public_key);
    let after = [
        std::fs::read(&record).unwrap(),
        std::fs::read(&paths.device_privkey).unwrap(),
        std::fs::read(&paths.device_cert).unwrap(),
        std::fs::read(identity.join("active")).unwrap(),
    ];
    assert_eq!(
        before, after,
        "no original record, selector, certificate or key replacement"
    );
    tilo_files::native::atomic::write_with_mode(
        &record,
        &runtime_identity::public_member_record("Changed", [1, 2, 3], public_key).unwrap(),
        0o600,
    )
    .unwrap();
    assert!(admitted.load().is_err());
    assert!(
        ready.resume().is_err(),
        "same root with changed source bytes is not restart authority"
    );
    assert!(
        prepared.desktop_manifest().is_err(),
        "delivery retains original source custody"
    );
}
