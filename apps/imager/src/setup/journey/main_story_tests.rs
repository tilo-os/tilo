//! One guided story through real actions; only host discovery is an isolated fixture.

#[cfg(feature = "native-capture-qualification")]
use super::*;
#[cfg(feature = "native-capture-qualification")]
use crate::resource::ImagerShell;
#[cfg(feature = "native-capture-qualification")]
use std::{path::Path, sync::Arc};
#[cfg(feature = "native-capture-qualification")]
use ui::framework::testing::TestApp;

#[cfg(feature = "native-capture-qualification")]
type App = TestApp<ImagerShell>;
pub(in crate::setup) use imager_backend::setup::discovery_fixture::{
    fixture_mount_root, source_fixture,
};
#[cfg(feature = "native-capture-qualification")]
fn state(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}
#[cfg(feature = "native-capture-qualification")]
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
#[cfg(feature = "native-capture-qualification")]
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
#[cfg(feature = "native-capture-qualification")]
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

pub(in crate::setup) fn fixture_host_facts() -> Option<super::super::staging::probe::HostFacts> {
    use super::super::staging::{plan, probe::HostFacts};
    use imager_backend::platform::Partition;
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
    env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    let originals: Vec<_> = [rclone]
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let mut app = App::launched(&["--setup"]).unwrap();
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
    assert!(identity.ownership_directory().starts_with(&namespace));
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
    let setup = imager_backend::setup::prepared_setup::PreparedSetupReview::open(
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
) -> Arc<imager_backend::setup::portable_home_media::HomeSource> {
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
        source: AbsolutePath::new(&home).unwrap(),
    })
    .unwrap();
    wait(app, |v| {
        v["page"] == "accounts" && v["portable_preparation"]["prepared"] == true
    });
    app.with_widget::<InstallerPreview, _>(|v| v.portable_source.reviewed_handle().unwrap())
        .unwrap()
}
