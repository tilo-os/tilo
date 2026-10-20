//! Live semantic recovery from a changed, explicitly reviewed home.
use super::*;
use crate::resource::GuidedInstallerShell;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use ui::framework::testing::TestApp;
type App = TestApp<GuidedInstallerShell>;
fn state(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}
fn wait(app: &mut App, predicate: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.settle();
        let current = state(app);
        assert_ne!(current["account_checkpoint"]["failed"], true, "{current}");
        if predicate(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "unfinished recovery: {current}");
        std::thread::sleep(Duration::from_millis(10));
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
fn open_preparation(app: &mut App, home: &Path) {
    wait(app, |v| v["restart"]["state"] == "missing");
    for choice in ["another", "migrate", "new"] {
        choose(app, choice);
    }
    app.effect(&InstallerPreviewEffect::SetPreviewName {
        value: "Ada".into(),
    })
    .unwrap();
    app.with_widget::<InstallerPreview, _>(|view| {
        view.journey.password = "source recovery fixture password".into();
        view.journey.confirmation = "source recovery fixture password".into();
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
        v["page"] == "belongings"
            && v["belongings_checking"] == false
            && v["restart"]["checking"] == false
    });
    let belongings = state(app);
    assert_eq!(
        belongings["belongings"]["home"],
        home.to_str().unwrap(),
        "{belongings}"
    );
    assert_eq!(belongings["categories"][0], true, "{belongings}");
    let services = ui::host::HostServices::null().with_files(std::sync::Arc::new(
        RecognizedHomePicker(home.to_path_buf()),
    ));
    let _picker = ui::host::enter(&services);
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "accounts"
            && v["account_checkpoint"]["accounts"] == 0
            && v["account_checkpoint"]["saving"] == false
            && v["portable_preparation"]["prepared"] == true
    });
}
/// Answers the actual unscoped host chooser used by the installer, rather
/// than the separate scoped-chooser interception fixture.
struct RecognizedHomePicker(PathBuf);
impl ui::host::FilePicker for RecognizedHomePicker {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        assert!(request.is_directory());
        assert_eq!(request.directory_hint(), Some(self.0.as_path()));
        assert!(request.title().contains(self.0.to_str().unwrap()));
        assert_eq!(request.accept_label(), t("installer-migration-home-allow"));
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(ui::PortalOutcome::Opened(self.0.clone()))
            .unwrap();
        receiver
    }
    fn pick_multiple(&self, _request: ui::PickRequest) -> ui::portal::MultiChannel {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(ui::portal::MultiOutcome::Cancelled).unwrap();
        receiver
    }
    fn open_file(&self, _request: ui::PickRequest) -> ui::portal::OpenFileChannel {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(ui::portal::OpenFileOutcome::Cancelled).unwrap();
        receiver
    }
    fn launch(&self, _request: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(ui::portal::LaunchOutcome::Failed(
                "No external launches in this fixture".into(),
            ))
            .unwrap();
        receiver
    }
}
fn recover_source(app: &mut App) {
    app.effect(&InstallerPreviewEffect::PrepareUsb).unwrap();
    wait(app, |v| v["media_preparation"]["state"] == "source-changed");
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(visual_tests::control(view, "installer.review-changed-files").is_some());
        assert!(visual_tests::control(view, "installer.prepare-usb").is_none());
    })
    .unwrap();
    assert!(app.effect(&InstallerPreviewEffect::PrepareUsb).is_err());
    let reviewing = app
        .effect(&InstallerPreviewEffect::ReviewChangedFiles)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(reviewing["page"], "belongings");
    assert_eq!(reviewing["portable_preparation"]["state"], "reviewing");
    assert_eq!(reviewing["portable_preparation"]["prepared"], false);
    wait(app, |v| {
        v["page"] == "accounts" && v["portable_preparation"]["prepared"] == true
    });
}
#[test]
#[ignore = "needs the owned-GPT native harness as root (os/scripts/installer-source-fixture/native_journey_tests.py)"]
fn changed_document_has_published_recovery_and_fresh_review_preserves_identity_and_accounts() {
    if !prepared_media::native_retained::enter_owned_journey(
        "setup::journey::source_change_tests::changed_document_has_published_recovery_and_fresh_review_preserves_identity_and_accounts",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    // Exercise the live-source contract on the actor's private tmpfs. Ext4/XFS
    // intentionally defer Home inventory until original-system offline boot,
    // so they cannot exercise a changed live inventory or its byte summary.
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::create_dir_all(home.join(".var/app/story.app")).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document = home.join("document");
    std::fs::write(&document, b"before").unwrap();
    std::fs::write(home.join(".var/app/story.app/data"), b"hidden app data").unwrap();
    // Live inventories spill only into acknowledged, native block-backed
    // preparation custody. NBD is not admitted as native inventory storage;
    // grant a fresh actor-owned directory on the builder's native filesystem.
    let custody = tempfile::tempdir_in("/var/tmp").unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _platform = main_story_tests::source_fixture(&root);
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", home.join(".config"));
    env.set("XDG_STATE_HOME", custody.path().join("state"));
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mut app = App::launched(&["--installer"]).unwrap();
    open_preparation(&mut app, &home);
    let accounts = accounts::checkpoint_fixture_for_journey(&root.join("account-fixture"));
    let ids = accounts.prepared_handle().unwrap().selected_ids();
    app.with_widget::<InstallerPreview, _>(|view| view.accounts = accounts)
        .unwrap();
    wait(&mut app, |v| {
        v["account_checkpoint"]["accounts"] == 2
            && v["account_checkpoint"]["saving"] == false
            && v["restart"]["checking"] == false
    });
    // Saving account credentials must not revoke the independently reviewed
    // Home inventory or its acknowledged preparation storage.
    app.with_widget::<InstallerPreview, _>(|view| {
        view.portable_source
            .reviewed_handle()
            .unwrap()
            .validate()
            .unwrap();
    })
    .unwrap();
    let original_key = state(&mut app)["prepared_identity"]["public_key"].clone();
    let original_bytes = state(&mut app)["portable_preparation"]["bytes"]
        .as_u64()
        .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    // Another-device media has no local disk-allocation decision. The live
    // route proceeds straight to Review, where source custody is consumed.
    wait(&mut app, |v| {
        v["page"] == "review" && v["restart"]["checking"] == false
    });
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(!view.has_storage_decision());
        assert!(!view.journey.history.contains(&Page::Storage));
    })
    .unwrap();
    std::fs::write(&document, b"changed while preparing").unwrap();
    recover_source(&mut app);
    let renewed = state(&mut app);
    assert_eq!(renewed["prepared_identity"]["public_key"], original_key);
    assert_eq!(renewed["account_checkpoint"]["accounts"], 2);
    assert!(renewed["portable_preparation"]["bytes"].as_u64().unwrap() > original_bytes);
    app.with_widget::<InstallerPreview, _>(|view| {
        assert_eq!(view.accounts.prepared_handle().unwrap().selected_ids(), ids);
        view.portable_source
            .reviewed_handle()
            .unwrap()
            .validate()
            .unwrap();
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |v| {
        v["page"] == "review" && v["restart"]["checking"] == false
    });
    let bundle = app
        .with_widget::<InstallerPreview, _>(|view| view.media_bundle.as_ref().unwrap().clone())
        .unwrap();
    app.effect(&InstallerPreviewEffect::PrepareUsb).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let prepared = loop {
        app.settle();
        if let Some(prepared) = bundle.get() {
            break prepared;
        }
        assert!(
            Instant::now() < deadline,
            "renewed media preparation did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    verify_prepared(&prepared, &root, original_key.as_str().unwrap(), &ids);
    verify_switched_recovery(&mut app, &home, original_key.as_str().unwrap(), &ids);
    assert_eq!(
        std::fs::read(document).unwrap(),
        b"changed again in the writer"
    );
    assert_eq!(
        std::fs::read(home.join(".var/app/story.app/data")).unwrap(),
        b"hidden app data"
    );
}

fn verify_prepared(
    prepared: &prepared_media::PreparedMedia,
    root: &Path,
    public_key: &str,
    ids: &[String],
) {
    let authenticated = super::super::prepared_setup::PreparedSetupReview::open(
        &prepared.package(),
        b"source recovery fixture password",
        &root.join(format!(
            "authenticated-{}",
            prepared.expectation().ciphertext_sha256
        )),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        hex::encode(authenticated.manifest().identity_public_key),
        public_key
    );
    assert_eq!(authenticated.manifest().account_ids, ids);
}

fn verify_switched_recovery(app: &mut App, home: &Path, public_key: &str, ids: &[String]) {
    verify_capacity_before_write(app, home);
    let media = home.parent().unwrap().join("media-filesystem-fixture");
    std::fs::create_dir(&media).unwrap();
    std::fs::write(home.join("document"), b"changed again in the writer").unwrap();
    app.with_widget::<crate::app::ImagerApp, _>(|writer| {
        let error = writer.validate_prepared_source_fixture().unwrap_err();
        assert!(crate::setup::portable_home_media::source_changed(&error));
        writer.start_changed_home_fixture(media.clone())
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.settle();
        let status = app
            .effect(&crate::resource::ImagerEffect::Status)
            .unwrap()
            .value
            .unwrap();
        if status["prepared_setup"]["state"] == "source-changed" {
            assert!(status["prepared_setup"]["receipt"].is_null());
            assert!(status["prepared_setup"]["home_receipt"].is_null());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "source export did not report its real changed-source error: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    app.with_widget::<crate::app::ImagerApp, _>(|writer| {
        assert!(visual_tests::control(writer, "imager.review-changed-files").is_some());
        assert!(visual_tests::control(writer, "imager.retry-prepared-medium").is_none());
    })
    .unwrap();
    assert_eq!(
        std::fs::read_dir(&media).unwrap().count(),
        0,
        "changed-source refusal publishes no ciphertext or disk receipt"
    );
    app.effect(&crate::resource::ImagerEffect::ReviewChangedPreparedFiles)
        .unwrap();
    let renewed = wait(app, |v| {
        v["page"] == "accounts" && v["portable_preparation"]["prepared"] == true
    });
    assert_eq!(renewed["prepared_identity"]["public_key"], public_key);
    assert_eq!(renewed["account_checkpoint"]["accounts"], 2);
    let bundle = app
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.accounts.prepared_handle().unwrap().selected_ids(), ids);
            view.media_bundle.as_ref().unwrap().clone()
        })
        .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |v| {
        v["page"] == "review" && v["restart"]["checking"] == false
    });
    app.effect(&InstallerPreviewEffect::PrepareUsb).unwrap();
    let artifact = loop {
        app.settle();
        if let Some(artifact) = bundle.get() {
            break artifact;
        }
        assert!(
            Instant::now() < deadline,
            "renewed switched preparation did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    verify_prepared(&artifact, home.parent().unwrap(), public_key, ids);
    let status = app
        .effect(&crate::resource::ImagerEffect::Status)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["prepared_setup"]["state"], "waiting-for-write");
    assert!(status["prepared_setup"]["receipt"].is_null());
    assert!(status["prepared_setup"]["home_receipt"].is_null());
}

fn verify_capacity_before_write(app: &mut App, home: &Path) {
    use crate::resource::ImagerEffect;
    let image = home.parent().unwrap().join("capacity-image.img");
    std::fs::write(&image, [0u8; 4096]).unwrap();
    let inventory = serde_json::json!({"blockdevices":[
        {"name":"sdb","model":"Image only","serial":"small-fixture","size":8192,"type":"disk","tran":"usb","rm":true},
        {"name":"sdc","model":"Complete migration","serial":"large-fixture","size":4294967296u64,"type":"disk","tran":"usb","rm":true}
    ]});
    app.with_widget::<crate::app::ImagerApp, _>(|writer| {
        writer
            .review_media_inputs_fixture(image.clone(), &inventory)
            .unwrap();
    })
    .unwrap();
    app.settle();
    app.with_widget::<crate::app::ImagerApp, _>(|writer| {
        writer.validate_prepared_source_fixture().unwrap();
    })
    .unwrap();
    let status = app.effect(&ImagerEffect::Status).unwrap().value.unwrap();
    let required = status["capacity"]["required"].as_u64().unwrap();
    assert!(
        required > 8192,
        "selected migration must contribute capacity: {status}"
    );
    assert!(
        required < 4294967296,
        "small fixture migration must fit a normal USB: {status}"
    );
    assert!(
        app.effect(&ImagerEffect::ChooseDrive {
            drive: "sdb".into()
        })
        .is_err()
    );
    assert!(
        app.effect(&ImagerEffect::Write {
            device: "/dev/sdb".into(),
            confirmation: "sdb".into()
        })
        .is_err()
    );
    let refused = app.effect(&ImagerEffect::Status).unwrap().value.unwrap();
    assert!(refused["capacity"]["available"].is_null());
    assert!(refused["prepared_setup"]["receipt"].is_null());
    assert!(refused["prepared_setup"]["home_receipt"].is_null());
    assert_eq!(std::fs::read(&image).unwrap(), [0u8; 4096]);
    app.effect(&ImagerEffect::ChooseDrive {
        drive: "sdc".into(),
    })
    .unwrap();
    let selected = app.effect(&ImagerEffect::Status).unwrap().value.unwrap();
    assert_eq!(selected["capacity"]["available"], 4294967296u64);
}
