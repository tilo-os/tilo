//! The live golden path from Destination through Review, driven only by the
//! production effects a person's Next reaches. Read-only host facts come from
//! the production planner fixture; no page, live flag or prepared identity is
//! assigned to stand in for the route.
use super::*;
use crate::resource::GuidedInstallerShell;
use crate::setup::view::FullOsCheck;
use std::path::Path;
use std::time::{Duration, Instant};
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
        if predicate(&current) {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "unfinished golden path: {current}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn choose(app: &mut App, choice: &str) {
    app.effect(&InstallerPreviewEffect::ChoosePreview {
        choice: choice.into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
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

fn paint(app: &mut App, scenario: &str, ids: &[&str]) {
    for (tag, theme, viewport, scale) in [
        ("dark", Theme::dark(), (480, 800), 1.0),
        ("light", Theme::light(), (480, 800), 1.0),
        ("dark-2x", Theme::dark(), (960, 1600), 2.0),
    ] {
        app.harness().resize(viewport).unwrap();
        app.harness()
            .set_theme(theme.with_ui_scale(scale).with_reduce_motion(true));
        visual_tests::paint_app(app, scenario, ids, viewport, tag);
    }
}

fn private_environment(root: &Path) -> tilo_test_env::ScopedEnv {
    use std::os::unix::fs::PermissionsExt as _;
    let home = root.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", &home);
    environment.set("XDG_CONFIG_HOME", &config);
    environment.set("XDG_STATE_HOME", root.join("state"));
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    // The development opt-in the live install gate reads; this test stops at
    // Review and never starts a privileged helper.
    environment.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
    environment
}

fn launch(root: &Path) -> (App, tilo_test_env::ScopedEnv) {
    let environment = private_environment(root);
    let mut app = App::launched(&["--installer"]).unwrap();
    wait(&mut app, |value| value["restart"]["state"] == "missing");
    (app, environment)
}

fn prepare_profile(app: &mut App) {
    app.effect(&InstallerPreviewEffect::SetPreviewName {
        value: "Ada".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |value| {
        value["page"] == "devices" && value["prepared_identity"]["state"] == "prepared"
    });
}

fn assert_prepared_draft(app: &mut App, target_model: Option<&str>) {
    app.with_widget::<InstallerPreview, _>(|preview| {
        let prepared = preview
            .personal
            .prepared_handle()
            .expect("Profile Continue must prepare the new person");
        assert_eq!(prepared.metadata().unwrap().display_name, "Ada");
        assert_eq!(
            preview.journey.page,
            Page::Devices,
            "identity preparation advances only after it is durable"
        );
    })
    .unwrap();
    let status = super::draft::DraftStore::for_session()
        .unwrap()
        .review()
        .unwrap();
    let saved = match status {
        super::draft::DraftStatus::Ready(saved) => Some(saved),
        _ => None,
    }
    .expect("the prepared identity must have a ready restart record");
    assert_eq!(saved.kind(), super::draft::IdentityKind::New);
    assert!(saved.choices().deferred);
    let target = saved
        .choices()
        .target
        .as_ref()
        .map(|target| target.model.as_str());
    assert_eq!(target, target_model);
}

/// Another computer: compatibility, migration, a new person, Your data and
/// review. No storage decision exists on this route, and no source moves
/// without a selected category, so this proves identity custody without a
/// native source provider.
#[test]
fn live_another_new_identity_prepares_before_your_data_and_review() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (mut app, _environment) = launch(&root);

    choose(&mut app, "another");
    assert_eq!(state(&mut app)["page"], "target");
    identify_other_computer(&mut app);
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(state(&mut app)["page"], "migration");
    choose(&mut app, "migrate");
    assert_eq!(state(&mut app)["page"], "identity");
    choose(&mut app, "new");
    assert_eq!(state(&mut app)["page"], "profile");

    paint(
        &mut app,
        "goldenpath-another-profile",
        &["installer.name", "installer.next", "installer.back"],
    );
    prepare_profile(&mut app);
    assert_prepared_draft(&mut app, Some("Other Test Laptop"));

    paint(
        &mut app,
        "goldenpath-another-devices",
        &["installer.device-name", "installer.next", "installer.back"],
    );
    app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
        value: "Ada's future computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |value| {
        value["page"] == "belongings" && value["belongings_checking"] == false
    });

    // The prepared person is exactly what the portable source would have
    // required on this page before the Profile Continue prepared it.
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(preview.journey.page, Page::Belongings);
        assert!(preview.personal.prepared_handle().is_some());
        assert!(preview.personal.ready());
        assert!(preview.your_data().source("files").is_some());
        assert!(preview.your_data().source("accounts").is_some());
    })
    .unwrap();
    paint(
        &mut app,
        "goldenpath-another-your-data",
        &[
            "installer.your-data.source.files",
            "installer.your-data.source.accounts",
            "installer.next",
            "installer.back",
        ],
    );

    // Leave nothing selected: the live route still proceeds to Review, and
    // another-device media has no local storage decision to invent.
    app.effect(&InstallerPreviewEffect::SetYourDataAll { included: false })
        .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |value| value["page"] == "review");
    let review = state(&mut app);
    assert_eq!(review["prepared_identity"]["state"], "prepared");
    assert!(review["prepared_identity"]["public_key"].is_string());
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.personal.prepared_handle().is_some());
        assert!(!preview.journey.history.contains(&Page::Storage));
    })
    .unwrap();
    paint(
        &mut app,
        "goldenpath-another-review",
        &["installer.review.profile", "installer.back"],
    );
}

/// This computer: the read-only compatibility facts enter the normal live
/// migration flow, then the same async Profile Continue leads through Your
/// data and the storage decision to Review.
#[test]
fn live_computer_new_identity_keeps_storage_and_review_after_profile() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (mut app, _environment) = launch(&root);

    choose(&mut app, "computer");
    assert_eq!(state(&mut app)["page"], "destination");
    // The compatibility page paints from read-only facts. Use the production
    // planner's fixture facts, then let the real compatibility drain advance
    // the journey; the page and live state are not assigned.
    app.with_widget::<InstallerPreview, _>(|preview| {
        let mut storage = storage::Storage::default();
        storage.stage();
        preview
            .compatibility
            .accept_read_only_check_for_test(FullOsCheck {
                facts: storage.facts.clone().unwrap(),
                plan: storage.plan.unwrap().unwrap(),
            });
    })
    .unwrap();
    wait(&mut app, |value| value["page"] == "migration");
    choose(&mut app, "migrate");
    assert_eq!(state(&mut app)["page"], "identity");
    choose(&mut app, "new");
    assert_eq!(state(&mut app)["page"], "profile");

    prepare_profile(&mut app);
    assert_prepared_draft(&mut app, Some(""));
    assert_eq!(
        state(&mut app)["prepared_identity"]["state"],
        "prepared",
        "Profile Continue must leave a durable prepared identity"
    );

    app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
        value: "Ada's computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |value| {
        value["page"] == "belongings" && value["belongings_checking"] == false
    });
    paint(
        &mut app,
        "goldenpath-computer-your-data",
        &[
            "installer.your-data.source.files",
            "installer.your-data.source.accounts",
            "installer.next",
            "installer.back",
        ],
    );

    // Keep the source tree at its default all-selected state; only the
    // optional Accounts step is skipped so the storage decision follows.
    app.effect(&InstallerPreviewEffect::SetYourDataSource {
        source: "accounts".into(),
        included: false,
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |value| value["page"] == "storage");
    let storage_state = state(&mut app);
    assert!(storage_state["storage"]["bounds_gib"].is_array());
    assert!(storage_state["storage"]["plan_digest"].is_string());
    assert_eq!(storage_state["prepared_identity"]["state"], "prepared");
    paint(
        &mut app,
        "goldenpath-computer-storage",
        &["installer.storage.size", "installer.next", "installer.back"],
    );

    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut app, |value| value["page"] == "review");
    let review = state(&mut app);
    assert_eq!(review["prepared_identity"]["state"], "prepared");
    assert_eq!(
        review["storage"]["plan_digest"],
        storage_state["storage"]["plan_digest"]
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(preview.personal.prepared_handle().is_some());
        assert!(preview.journey.history.contains(&Page::Storage));
        assert_eq!(preview.journey.identity, Some(Identity::New));
    })
    .unwrap();
    paint(
        &mut app,
        "goldenpath-computer-review",
        &["installer.review.profile", "installer.back"],
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(
            preview.direct_blocker().is_none(),
            "qualified review facts must admit the real action"
        );
        let root = preview.composition(&Theme::dark());
        let action = visual_tests::control(root.as_ref(), "installer.prepare-direct")
            .expect("the reviewed destination exposes its real install action");
        assert!(
            !action.accessibility().unwrap().state.disabled,
            "the admitted action is enabled on Review"
        );
        assert!(preview.personal.ready());
        assert!(preview.your_data().source("files").is_some());
        assert!(
            preview.migration_selection.reviewed_intent().is_none()
                && !preview.migration_selection.busy(),
            "a complete source with no eligible files needs no Home capture"
        );
    })
    .unwrap();
}
