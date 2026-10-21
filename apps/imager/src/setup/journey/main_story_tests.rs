//! One guided story through real actions; only host discovery is an isolated fixture.
use super::*;
use crate::resource::GuidedInstallerShell;
use std::{path::Path, sync::Arc};
use ui::framework::testing::TestApp;

type App = TestApp<GuidedInstallerShell>;
use imager_backend::setup::discovery_fixture::PlatformFixture;
pub(in crate::setup) use imager_backend::setup::discovery_fixture::{
    fixture_mount_root, source_fixture,
};
fn state(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}
fn wait(app: &mut App, predicate: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let current = state(app);
        if predicate(&current) {
            return current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unfinished story: {current}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
fn identify_other_computer(app: &mut App) {
    for (field, value) in [
        ("manufacturer", "Test Vendor"),
        ("model", "Other Test Laptop"),
        ("variant", "OTHER-TEST-BOARD"),
    ] {
        app.effect(&InstallerPreviewEffect::SetTargetIdentityField {
            field: field.into(),
            value: value.into(),
        })
        .unwrap();
    }
}
fn choose(app: &mut App, choice: &str) {
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: choice.into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    if state(app)["page"] == "target" {
        identify_other_computer(app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
    }
}
fn paint(app: &mut App, scenario: &str, ids: &[&str]) {
    for (tag, theme, viewport, scale) in [
        ("dark", Theme::dark(), (480, 800), 1.0),
        ("light", Theme::light(), (480, 800), 1.0),
        ("dark", Theme::dark(), (1180, 820), 1.0),
        ("light", Theme::light(), (1180, 820), 1.0),
        ("dark-2x", Theme::dark(), (2560, 1440), 2.0),
    ] {
        app.harness().resize(viewport).unwrap();
        app.harness()
            .set_theme(theme.with_ui_scale(scale).with_reduce_motion(true));
        visual_tests::paint_app(app, scenario, ids, viewport, tag);
    }
}
fn absent(app: &mut App, id: &str) {
    app.settle();
    app.with_widget::<InstallerPreview, _>(|v| {
        assert!(
            visual_tests::control(v, id).is_none(),
            "unexpected control: {id}"
        );
    })
    .unwrap();
}
fn profile(
    app: &mut App,
    picture: &Path,
) -> Arc<super::super::prepared_identity::PreparedIdentity> {
    app.effect(&InstallerPreviewEffect::SetPreviewName {
        value: "Ada's new computing life".into(),
    })
    .unwrap();
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(view.personal.photo().is_none());
        assert_eq!(view.avatar(&view.journey.name).initials(), "AL");
    })
    .unwrap();
    paint(
        app,
        "main-story-profile-initials",
        &["installer.name", "installer.avatar", "installer.next"],
    );
    // This selected-picture fixture is a solid orange PNG. A photo replaces
    // initials, so capture its branch separately from the default avatar.
    app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
        source_file: picture.to_str().unwrap().into(),
    })
    .unwrap();
    wait(app, |v| v["prepared_identity"]["state"] == "photo-selected");
    app.with_widget::<InstallerPreview, _>(|view| assert!(view.personal.photo().is_some()))
        .unwrap();
    paint(
        app,
        "main-story-profile",
        &["installer.name", "installer.avatar", "installer.next"],
    );
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "devices" && v["prepared_identity"]["state"] == "prepared"
    });
    app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
        value: "Ada's computer".into(),
    })
    .unwrap();
    wait(app, |v| v["restart"]["checking"] == false);
    app.with_widget::<InstallerPreview, _>(|view| view.personal.prepared_handle().unwrap())
        .unwrap()
}

pub(in crate::setup) fn fixture_host_facts() -> Option<super::super::staging::probe::HostFacts> {
    use super::super::staging::{plan, platform::Partition, probe::HostFacts};
    fixture_mount_root()?;
    Some(HostFacts {
        basis: Some(plan::tests::linux_basis(
            512,
            512u64 << 30,
            vec![Partition {
                number: 1,
                guid: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
                type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                first_lba: 2048,
                last_lba: (128u64 << 30) / 512 - 1,
            }],
        )),
        bitlocker_protected: false,
        blockers: vec![],
    })
}

pub(in crate::setup) fn fixture_support_report()
-> Option<imager_backend::support_report::SupportReport> {
    use imager_backend::support_report::{
        Applicability, ComponentKind, ComponentSupport, DestinationIdentity, DeviceBinding,
        Evidence, EvidenceFact, EvidenceScope, REPORT_SCHEMA, Remedy, ReportSource, SupportReason,
        SupportReport, SupportState,
    };
    let device = |name: &str, driver: Option<&str>, scope| Evidence {
        scope,
        fact: EvidenceFact::Device {
            name: name.into(),
            driver: driver.map(str::to_string),
            firmware: None,
            vendor: None,
            product: None,
            reported_problem: false,
        },
    };
    let component = |kind,
                     name: &str,
                     present,
                     state,
                     applicability,
                     reason,
                     evidence: Vec<Evidence>,
                     remedy: Option<Remedy>| {
        ComponentSupport {
            kind,
            name: name.into(),
            present,
            state,
            applicability,
            reason,
            evidence,
            remedy,
        }
    };
    Some(SupportReport {
        schema: REPORT_SCHEMA.into(),
        identity: DestinationIdentity {
            source: ReportSource::LocalScan,
            manufacturer: Some("Test Vendor".into()),
            model: Some("Test Laptop 15".into()),
            variant: Some("TESTBOARD".into()),
            board: Some("TESTBOARD".into()),
            sku: Some("SKU-1".into()),
            firmware: Some("Test Firmware 1.2.3".into()),
        },
        kernel: Some("7.2.7-test".into()),
        architecture: Some("aarch64".into()),
        components: vec![
            component(
                ComponentKind::Graphics,
                "AMD graphics, NVIDIA graphics",
                true,
                SupportState::Supported,
                Applicability::ReleaseVerified,
                SupportReason::HybridGraphics {
                    primary: DeviceBinding {
                        device: "AMD graphics".into(),
                        driver: Some("amdgpu".into()),
                    },
                    additional: vec![DeviceBinding {
                        device: "NVIDIA graphics".into(),
                        driver: Some("nvidia".into()),
                    }],
                },
                vec![
                    device(
                        "AMD graphics",
                        Some("amdgpu"),
                        EvidenceScope::SelectedRelease,
                    ),
                    device(
                        "NVIDIA graphics",
                        Some("nvidia"),
                        EvidenceScope::SelectedRelease,
                    ),
                ],
                None,
            ),
            component(
                ComponentKind::Display,
                "eDP-1, DP-1",
                true,
                SupportState::Supported,
                Applicability::ReleaseVerified,
                SupportReason::DisplaysActive {
                    internal: 1,
                    external: 1,
                    devices: vec!["eDP-1".into(), "DP-1".into()],
                },
                vec![
                    Evidence {
                        scope: EvidenceScope::SelectedRelease,
                        fact: EvidenceFact::Display {
                            name: "eDP-1".into(),
                            connected: true,
                            active: true,
                        },
                    },
                    Evidence {
                        scope: EvidenceScope::SelectedRelease,
                        fact: EvidenceFact::Display {
                            name: "DP-1".into(),
                            connected: true,
                            active: true,
                        },
                    },
                ],
                None,
            ),
            component(
                ComponentKind::Wifi,
                "wlp4s0",
                true,
                SupportState::Supported,
                Applicability::ReleaseVerified,
                SupportReason::DriverBound {
                    bindings: vec![DeviceBinding {
                        device: "wlp4s0".into(),
                        driver: Some("iwlwifi".into()),
                    }],
                },
                vec![device(
                    "wlp4s0",
                    Some("iwlwifi"),
                    EvidenceScope::SelectedRelease,
                )],
                None,
            ),
            component(
                ComponentKind::Fingerprint,
                "Fingerprint Sensor",
                true,
                SupportState::Limited,
                Applicability::ObservationOnly,
                SupportReason::FingerprintUnverified {
                    device: "Fingerprint Sensor".into(),
                },
                vec![Evidence {
                    scope: EvidenceScope::Observation,
                    fact: EvidenceFact::Device {
                        name: "Fingerprint Sensor".into(),
                        driver: None,
                        firmware: None,
                        vendor: Some("1234".into()),
                        product: Some("5678".into()),
                        reported_problem: false,
                    },
                }],
                Some(Remedy::PasswordSignIn),
            ),
            component(
                ComponentKind::Camera,
                "",
                false,
                SupportState::Unverified,
                Applicability::Unknown,
                SupportReason::NotDetected,
                Vec::new(),
                Some(Remedy::ExternalCamera),
            ),
            component(
                ComponentKind::Boot,
                "",
                true,
                SupportState::Unverified,
                Applicability::ObservationOnly,
                SupportReason::Uefi {
                    secure_boot: Some(true),
                    tpm: Some("TPM 2.0".into()),
                },
                vec![device("UEFI", None, EvidenceScope::Observation)],
                None,
            ),
            component(
                ComponentKind::Storage,
                "Test NVMe",
                true,
                SupportState::Supported,
                Applicability::ReleaseVerified,
                SupportReason::DriverBound {
                    bindings: vec![DeviceBinding {
                        device: "Test NVMe".into(),
                        driver: Some("nvme".into()),
                    }],
                },
                vec![device(
                    "Test NVMe",
                    Some("nvme"),
                    EvidenceScope::SelectedRelease,
                )],
                None,
            ),
        ],
    })
}

fn story_accounts_and_storage(app: &mut App, home: &Path) {
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "belongings" && v["belongings_checking"] == false
    });
    paint(
        app,
        "main-story-belongings",
        &[
            "installer.category.files",
            "installer.category.accounts",
            "installer.next",
        ],
    );
    let overview = app.harness().inspection_json().unwrap();
    assert!(overview.contains(&t("installer-accounts-nextcloud")));
    assert!(overview.contains("ada"));
    app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
        .unwrap();
    paint(
        app,
        "main-story-belongings-details",
        &[
            "installer.category.files",
            "installer.category.accounts",
            "installer.details",
            "installer.next",
        ],
    );
    let expanded = app.harness().inspection_json().unwrap();
    assert!(expanded.contains(&t("installer-preview-accounts-body")));
    assert!(expanded.contains(&t("installer-accounts-nextcloud")));
    assert!(expanded.contains("ada"));
    app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
        .unwrap();
    app.effect(&InstallerPreviewEffect::SelectMigrationHome {
        source: home.to_str().unwrap().into(),
    })
    .unwrap();
    wait(app, |v| {
        v["migration_preparation"]["prepared"] == true && v["page"] == "accounts"
    });
    let intent = app
        .with_widget::<InstallerPreview, _>(|v| v.migration_selection.reviewed_intent().unwrap())
        .unwrap();
    assert_eq!(
        intent.source_plan().home_policy,
        super::super::migration_handoff::HomePolicy::AccessReadWrite
    );
    assert!(
        intent.application_minimum()
            < std::fs::metadata(home.join("Documents/archive.img"))
                .unwrap()
                .len(),
        "shared documents must not be charged as copied application data"
    );
    assert!(intent.source_plan().categories.home);
    assert!(!intent.source_plan().categories.flatpak);
    assert!(intent.source_plan().categories.accounts);
    let plan = serde_json::to_value(intent.source_plan()).unwrap();
    let relative: super::super::migration_store::NativePath =
        serde_json::from_value(plan["home"]["relative"].clone()).unwrap();
    let source_mount =
        std::path::PathBuf::from(std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE").unwrap());
    assert_eq!(
        relative.decode().unwrap(),
        home.strip_prefix(source_mount).unwrap()
    );
    assert_eq!(state(app)["accounts"]["can_continue"], false);
    let discovered = app
        .with_widget::<InstallerPreview, _>(|view| {
            format!(
                "installer.account.detected.{}",
                view.belongings
                    .accounts
                    .candidates
                    .iter()
                    .find(|candidate| candidate.provider
                        == super::super::account_transfer::Provider::Nextcloud)
                    .unwrap()
                    .id
            )
        })
        .unwrap();
    paint(
        app,
        "main-story-accounts-discovered",
        &[
            &discovered,
            "installer.account.review-saved",
            "installer.back",
        ],
    );
    let overview = app.harness().inspection_json().unwrap();
    assert!(overview.contains(&t("installer-accounts-nextcloud")));
    assert!(overview.contains("ada"));
    assert_eq!(state(app)["accounts"]["can_continue"], false);
    app.effect(&InstallerPreviewEffect::AccountStep {
        step: "skip-saved".into(),
        selection: None,
    })
    .unwrap();
    // This story declines saved credentials while retaining shared files.
    // A subsequent generic bulk import must not override that exclusion.
    assert!(
        app.effect(&InstallerPreviewEffect::AccountStep {
            step: "import-saved".into(),
            selection: None,
        })
        .is_err()
    );
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(view.accounts.prepared_handle().is_none());
        assert!(view.accounts.navigation_action().is_none());
    })
    .unwrap();
    wait(app, |v| {
        assert_ne!(v["account_checkpoint"]["failed"], true, "{v}");
        v["accounts"]["can_continue"] == true
            && v["account_checkpoint"]["saving"] == false
            && v["account_checkpoint"]["accounts"] == 0
    });
    paint(
        app,
        "main-story-accounts",
        &["installer.next", "installer.back"],
    );
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "storage"
            && v["storage"]["checking"] == false
            && v["storage"]["plan_digest"].is_string()
    });
    app.with_widget::<InstallerPreview, _>(|v| {
        prepared_media::native_retained::assert_journey_registration(v);
    })
    .unwrap();
    paint(
        app,
        "main-story-storage",
        &[
            "installer.storage.size",
            "installer.storage.shared-files",
            "installer.next",
            "installer.back",
        ],
    );
    absent(app, "installer.prepare-direct");
    absent(app, "installer.prepare-usb");
    absent(app, "installer.storage.capacity");
    absent(app, "installer.storage.recheck");
    app.effect(&InstallerPreviewEffect::ToggleStorageDetails)
        .unwrap();
    paint(
        app,
        "main-story-storage-details",
        &[
            "installer.storage.capacity",
            "installer.storage.existing",
            "installer.storage.temporary",
            "installer.storage.details",
        ],
    );
    app.effect(&InstallerPreviewEffect::ToggleStorageDetails)
        .unwrap();
    absent(app, "installer.storage.capacity");
    let initial = state(app)["storage"]["size_gib"].as_u64().unwrap();
    app.effect(&InstallerPreviewEffect::SetStorageSize { gib: initial + 1 })
        .unwrap();
    assert_eq!(state(app)["storage"]["size_gib"], initial + 1);
    assert!(state(app)["storage"]["plan_digest"].is_string());
    absent(app, "installer.storage.recheck");
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    assert_eq!(state(app)["page"], "accounts");
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(state(app)["storage"]["size_gib"], initial + 1);
    let retained = app
        .with_widget::<InstallerPreview, _>(|v| v.migration_selection.reviewed_intent().unwrap())
        .unwrap();
    assert_eq!(retained, intent);
    wait(app, |v| v["restart"]["checking"] == false);
    let saved = if let super::draft::DraftStatus::Ready(saved) =
        super::draft::DraftStore::for_session()
            .unwrap()
            .review()
            .unwrap()
    {
        Some(saved)
    } else {
        None
    }
    .expect("Actual prepared identity was not retained");
    assert_eq!(saved.choices().categories, [true, false, true]);
    assert_eq!(saved.choices().device_label, "Ada's computer");
    assert_eq!(saved.kind(), super::draft::IdentityKind::New);
}

#[test]
#[ignore = "needs the owned-GPT native harness as root (os/scripts/installer-source-fixture/native_journey_tests.py)"]
fn guided_new_user_migrates_recognized_home_then_reviews_shared_files_storage() {
    if !prepared_media::native_retained::enter_owned_journey(
        "setup::journey::main_story_tests::guided_new_user_migrates_recognized_home_then_reviews_shared_files_storage",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = prepared_media::native_retained::journey_directory();
    let root = temporary.path().canonicalize().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _platform = PlatformFixture::install(&root);
    let home = root.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(home.join(".var/app/story.app")).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(home.join("Documents/notes.txt"), b"same original files").unwrap();
    // A large real sparse document proves the allocation excludes shared files
    // without consuming that much physical storage in this fixture.
    std::fs::File::create(home.join("Documents/archive.img"))
        .unwrap()
        .set_len(64u64 << 30)
        .unwrap();
    std::fs::write(
        home.join(".var/app/story.app/state"),
        b"private application state",
    )
    .unwrap();
    std::fs::write(
        config.join("Nextcloud/nextcloud.cfg"),
        "[Accounts]\n0\\url=https://cloud.example\n0\\http_user=ada\n0\\authType=http\n",
    )
    .unwrap();
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &config);
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
    let picture = root.join("avatar.png");
    std::fs::write(&picture, include!("../fixtures/avatar.rs")).unwrap();
    let mut app = App::launched(&["--installer"]).unwrap();
    wait(&mut app, |v| v["restart"]["state"] == "missing");
    for choice in ["computer", "migrate", "new"] {
        choose(&mut app, choice);
    }
    let identity = profile(&mut app, &picture);
    assert_eq!(
        identity.metadata().unwrap().display_name,
        "Ada's new computing life"
    );
    assert!(identity.metadata().unwrap().profile_photo.is_some());
    paint(
        &mut app,
        "main-story-devices",
        &["installer.device-name", "installer.next", "installer.back"],
    );
    story_accounts_and_storage(&mut app, &home);
    env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "0");
    app.with_widget::<InstallerPreview, _>(|v| v.dirty = true)
        .unwrap();
    app.settle();
    // Installation starts only after the setup review. The fallback changes
    // the action shown there, not storage into a release-selection form.
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(state(&mut app)["page"], "review");
    paint(
        &mut app,
        "main-story-review-usb-fallback",
        &[
            "installer.review.usb-fallback",
            "installer.prepare-usb",
            "installer.back",
        ],
    );
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains(&t("installer-review-usb-fallback"))
    );
    let refused = app
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap_err();
    assert!(
        matches!(refused, tilo_actions::DoorError::Failed(ref message)
        if message == &t("installer-review-usb-choice-required")),
        "fallback requires the explicitly named USB action, never an implicit install: {refused}"
    );
    assert_eq!(state(&mut app)["page"], "review");
    absent(&mut app, "installer.storage.size");
    absent(&mut app, "installer.prepare-direct");
    assert_eq!(
        std::fs::read(home.join("Documents/notes.txt")).unwrap(),
        b"same original files"
    );
    assert_eq!(
        std::fs::read(home.join(".var/app/story.app/state")).unwrap(),
        b"private application state"
    );
    assert_eq!(
        std::fs::read_to_string(config.join("Nextcloud/nextcloud.cfg")).unwrap(),
        "[Accounts]\n0\\url=https://cloud.example\n0\\http_user=ada\n0\\authType=http\n"
    );
    assert_eq!(
        std::fs::metadata(home.join("Documents/archive.img"))
            .unwrap()
            .len(),
        64u64 << 30
    );
    assert!(!root.join("imported-home").exists());
    assert_eq!(
        identity.metadata().unwrap().display_name,
        "Ada's new computing life"
    );
}

// This original-only story is qualified on actual disposable GPT volumes by
// output::signed_original_home_and_named_applications_complete_and_restore.
// Run it as the real source owner; a temp directory cannot authorize offline export.
#[cfg(feature = "native-capture-qualification")]
pub(in crate::setup) fn prepare_native_portable_story(
    home: &Path,
    destination: &Path,
    password: &str,
) -> super::super::migration_review::OriginalPreparation {
    prepare_native_portable_story_source(home, destination, password, None)
}

#[cfg(feature = "native-capture-qualification")]
pub(in crate::setup) fn prepare_native_portable_story_with_dav(
    home: &Path,
    destination: &Path,
    password: &str,
    endpoint: &reqwest::Url,
) -> super::super::migration_review::OriginalPreparation {
    prepare_native_portable_story_source(home, destination, password, Some(endpoint))
}

#[cfg(feature = "native-capture-qualification")]
fn prepare_native_portable_story_source(
    home: &Path,
    destination: &Path,
    password: &str,
    endpoint: Option<&reqwest::Url>,
) -> super::super::migration_review::OriginalPreparation {
    use std::os::unix::fs::PermissionsExt as _;
    use zeroize::Zeroizing;
    assert_eq!(unsafe { libc::geteuid() }, 1000);
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(home.join(".var/app/story.app")).unwrap();
    std::fs::write(home.join("Documents/notes"), b"all my actual documents").unwrap();
    std::fs::write(
        home.join(".var/app/story.app/state"),
        b"original hidden app state",
    )
    .unwrap();
    let rclone = config.join("rclone/rclone.conf");
    if let Some(endpoint) = endpoint {
        // Match the real DAV fixture's credential format through canonical
        // discovery/capture, rather than manufacturing account grant metadata.
        let user = cloud_source_dav::fake::USER;
        let obscured = "AAECAwQFBgcICQoLDA0OD2LwFVdisTSqpU56FmE";
        std::fs::write(&rclone, format!("[Team files]\ntype=webdav\nurl={endpoint}\nuser={user}\npass={obscured}\n[Personal files]\ntype=webdav\nurl={endpoint}\nuser={user}\npass={obscured}\n")).unwrap();
    } else {
        std::fs::write(&rclone, "[Team files]\ntype=webdav\nurl=https://team.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n[Personal files]\ntype=webdav\nurl=https://personal.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n").unwrap();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", home);
    env.set("XDG_CONFIG_HOME", &config);
    env.set("XDG_STATE_HOME", home.join(".local/state"));
    env.set("XDG_CACHE_HOME", home.join(".cache"));
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let originals: Vec<_> = [rclone]
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let mut app = App::launched(&["--installer"]).unwrap();
    wait(&mut app, |v| v["restart"]["state"] == "missing");
    let reviewed = prepare_portable_story(&mut app, home, password);
    assert_eq!(reviewed.capture_status(), "awaiting-original-boot");
    app.effect(&InstallerPreviewEffect::AccountStep {
        step: "import-saved".into(),
        selection: None,
    })
    .unwrap();
    wait(&mut app, |v| {
        assert_ne!(v["account_checkpoint"]["failed"], true, "{v}");
        v["account_checkpoint"]["saving"] == false && v["account_checkpoint"]["accounts"] == 2
    });
    reviewed.validate().unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| {
        v["page"] == "review" && v["restart"]["checking"] == false
    });
    let (identity, selected) = app
        .with_widget::<InstallerPreview, _>(|v| {
            (
                v.personal.prepared_handle().unwrap(),
                v.accounts.prepared_handle(),
            )
        })
        .unwrap();
    let namespace = personal::preparation_namespace().unwrap();
    assert!(namespace.starts_with(home));
    assert!(identity.custody_directory().starts_with(&namespace));
    assert!(
        super::draft::DraftStore::for_session()
            .unwrap()
            .review()
            .is_ok()
    );
    let prepared = prepared_media::PreparedMedia::prepare_with_home(
        &identity,
        selected,
        Some(reviewed.clone()),
        Zeroizing::new(password.into()),
        "native-guided-home-story".into(),
    )
    .unwrap();
    // Export only the real guided encrypted parent. The root coordinator must
    // authenticate it before adding its genuine registered offline authority.
    std::fs::copy(prepared.package(), destination).unwrap();
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600)).unwrap();
    let review_root = destination.parent().unwrap().join("guided-parent-review");
    let setup = super::super::prepared_setup::PreparedSetupReview::open(
        destination,
        password.as_bytes(),
        &review_root,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(setup.manifest().identity_public_key, identity.public_key());
    assert_eq!(setup.manifest().account_ids.len(), 2);
    assert_eq!(identity.metadata().unwrap().display_name, "Ada");
    for (path, bytes) in originals {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(
        std::fs::read(home.join("Documents/notes")).unwrap(),
        b"all my actual documents"
    );
    assert!(namespace.exists());
    // Original-only consent pins the source, not a premature live file epoch.
    // The authenticated original reader establishes the actual export epoch.
    std::fs::write(
        home.join("Documents/notes"),
        b"an actual edit after original-only consent",
    )
    .unwrap();
    reviewed.validate().unwrap();
    std::fs::write(home.join("Documents/notes"), b"all my actual documents").unwrap();
    reviewed.validate().unwrap();
    super::super::migration_review::OriginalPreparation::review(home)
        .unwrap()
        .expect("the actual original owner must retain its private preparation omission")
}

#[cfg(feature = "native-capture-qualification")]
fn prepare_portable_story(
    app: &mut App,
    home: &Path,
    password: &str,
) -> Arc<super::super::portable_home_media::HomeSource> {
    for choice in ["another", "migrate", "new"] {
        choose(app, choice);
    }
    app.effect(&InstallerPreviewEffect::SetPreviewName {
        value: "Ada".into(),
    })
    .unwrap();
    app.with_widget::<InstallerPreview, _>(|view| {
        view.journey.password = password.into();
        view.journey.confirmation = password.into();
        view.dirty = true;
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "devices" && v["prepared_identity"]["state"] == "prepared"
    });
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| v["page"] == "purpose");
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: "computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "belongings" && v["belongings_checking"] == false
    });
    app.effect(&InstallerPreviewEffect::SelectMigrationHome {
        source: home.to_str().unwrap().into(),
    })
    .unwrap();
    wait(app, |v| {
        v["page"] == "accounts" && v["portable_preparation"]["prepared"] == true
    });
    app.with_widget::<InstallerPreview, _>(|v| v.portable_source.reviewed_handle().unwrap())
        .unwrap()
}

#[test]
#[ignore = "needs the owned-GPT native harness as root (os/scripts/installer-source-fixture/native_journey_tests.py)"]
fn guided_existing_backup_migrates_this_home_without_creating_another_identity() {
    if !prepared_media::native_retained::enter_owned_journey(
        "setup::journey::main_story_tests::guided_existing_backup_migrates_this_home_without_creating_another_identity",
    ) {
        return;
    }
    use super::super::prepared_identity::PreparedIdentity;
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = prepared_media::native_retained::journey_directory();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _platform = PlatformFixture::install(&root);
    let home = root.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(home.join("Documents/notes"), b"original shared document").unwrap();
    std::fs::write(home.join(".private-settings"), b"original hidden settings").unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &config);
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set("XDG_CACHE_HOME", root.join("cache"));
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
    let donor = PreparedIdentity::create(
        &root.join("donor"),
        "Existing Ada",
        [25, 80, 120],
        tilo_identity_host::provisioning::IdentityStorage::for_session(),
    )
    .unwrap();
    let backup = root.join("original.sealed");
    let backup_password = "backup password for existing identity";
    donor
        .export(&backup, zeroize::Zeroizing::new(backup_password.into()))
        .unwrap();
    let original_backup = std::fs::read(&backup).unwrap();
    let original_donor = std::fs::read(root.join("donor/primary.json")).unwrap();
    let expected_key = donor.public_key();
    let mut app = App::launched(&["--installer"]).unwrap();
    wait(&mut app, |v| v["restart"]["state"] == "missing");
    for choice in ["computer", "migrate", "existing"] {
        choose(&mut app, choice);
    }
    assert_eq!(state(&mut app)["page"], "connect");
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: "backup".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SelectExistingIdentityBackup {
        source_file: backup.to_str().unwrap().into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SetExistingIdentityPassword {
        value: backup_password.into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    let restored = wait(&mut app, |v| {
        v["existing_identity"]["state"] == "ready" && v["prepared_identity"]["state"] == "prepared"
    });
    assert_eq!(
        restored["page"], "connect",
        "restoration is reviewed before advancing"
    );
    assert_eq!(
        restored["prepared_identity"]["public_key"],
        hex::encode(expected_key)
    );
    app.with_widget::<InstallerPreview, _>(|view| {
        view.journey.password = "several unrelated installation words".into();
        view.journey.confirmation = "several unrelated installation words".into();
        view.dirty = true;
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| {
        v["page"] == "devices" && v["prepared_identity"]["state"] == "prepared"
    });
    app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
        value: "My existing identity computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| v["page"] == "purpose");
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: "computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| {
        v["page"] == "belongings" && v["belongings_checking"] == false
    });
    app.effect(&InstallerPreviewEffect::SelectMigrationHome {
        source: home.to_str().unwrap().into(),
    })
    .unwrap();
    wait(&mut app, |v| {
        v["page"] == "accounts" && v["migration_preparation"]["prepared"] == true
    });
    wait(&mut app, |v| {
        assert_ne!(v["account_checkpoint"]["failed"], true, "{v}");
        v["accounts"]["can_continue"] == true
            && v["account_checkpoint"]["saving"] == false
            && v["account_checkpoint"]["accounts"] == 0
    });
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| {
        v["page"] == "storage"
            && v["storage"]["checking"] == false
            && v["storage"]["plan_digest"].is_string()
    });
    let intent = app
        .with_widget::<InstallerPreview, _>(|v| {
            assert!(v.live, "the entire launched story remains on the live path");
            prepared_media::native_retained::assert_journey_registration(v);
            assert_eq!(
                v.personal.prepared_handle().unwrap().public_key(),
                expected_key
            );
            assert_eq!(v.journey.identity, Some(Identity::Existing));
            assert_eq!(v.journey.name, "Existing Ada");
            let intent = v.migration_selection.reviewed_intent().unwrap();
            assert_eq!(
                intent.source_plan().home_policy,
                super::super::migration_handoff::HomePolicy::AccessReadWrite
            );
            assert!(intent.source_plan().categories.home);
            intent
        })
        .unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original_backup);
    assert_eq!(
        std::fs::read(root.join("donor/primary.json")).unwrap(),
        original_donor
    );
    assert_eq!(
        std::fs::read(home.join("Documents/notes")).unwrap(),
        b"original shared document"
    );
    assert_eq!(
        std::fs::read(home.join(".private-settings")).unwrap(),
        b"original hidden settings"
    );
    wait(&mut app, |v| v["restart"]["checking"] == false);
    let super::draft::DraftStatus::Ready(saved) = super::draft::DraftStore::for_session()
        .unwrap()
        .review()
        .unwrap()
    else {
        panic!("existing identity and shared-home choices must be acknowledged together");
    };
    assert_eq!(saved.kind(), super::draft::IdentityKind::Backup);
    assert_eq!(
        saved.choices().destination,
        Some(super::draft::DestinationChoice::Computer)
    );
    assert_eq!(saved.choices().migrate, Some(true));
    assert!(saved.choices().categories[0]);
    assert_eq!(
        saved.choices().device_label,
        "My existing identity computer"
    );
    assert_eq!(saved.resume().unwrap().public_key(), expected_key);
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    assert_eq!(state(&mut app)["page"], "accounts");
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.with_widget::<InstallerPreview, _>(|v| {
        assert_eq!(v.migration_selection.reviewed_intent().unwrap(), intent);
        assert_eq!(
            v.personal.prepared_handle().unwrap().public_key(),
            expected_key
        );
    })
    .unwrap();
    assert_eq!(state(&mut app)["page"], "storage");
    absent(&mut app, "installer.prepare-direct");
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(state(&mut app)["page"], "review");
    app.with_widget::<InstallerPreview, _>(|v| {
        assert!(visual_tests::control(v, "installer.prepare-direct").is_some());
        assert_eq!(
            v.personal.prepared_handle().unwrap().public_key(),
            expected_key
        );
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    assert_eq!(state(&mut app)["page"], "storage");
    app.harness().resize((1180, 820)).unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    visual_tests::paint_app(
        &mut app,
        "existing-backup-shared-home-storage",
        &[
            "installer.storage.shared-files",
            "installer.storage.size",
            "installer.next",
            "installer.back",
        ],
        (1180, 820),
        "dark",
    );
}
