//! Reopen the real guided resource; no host persona, pairing or installation starts.
use super::*;
use crate::resource::GuidedInstallerShell;
use ui::framework::testing::TestApp;

type App = TestApp<GuidedInstallerShell>;
fn read(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}
fn wait(app: &mut App, predicate: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    // Identity preparation derives keys; a loaded test node can take longer than a fixed 30 s.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let state = read(app);
        if predicate(&state) {
            return state;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unfinished controller: {state}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
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
fn prepare(
    app: &mut App,
    picture: &std::path::Path,
) -> std::sync::Arc<super::super::prepared_identity::PreparedIdentity> {
    prepare_with_migration(app, picture, false)
}
fn prepare_with_migration(
    app: &mut App,
    picture: &std::path::Path,
    migrate: bool,
) -> std::sync::Arc<super::super::prepared_identity::PreparedIdentity> {
    wait(app, |state| state["restart"]["state"] == "missing");
    for choice in ["computer", if migrate { "migrate" } else { "fresh" }, "new"] {
        choose(app, choice);
    }
    app.effect(&InstallerPreviewEffect::SetPreviewName {
        value: "My prepared name".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
        source_file: picture.to_str().unwrap().into(),
    })
    .unwrap();
    wait(app, |state| {
        state["prepared_identity"]["state"] == "photo-selected"
    });
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    for effect in [
        InstallerPreviewEffect::SetPreviewPassword {
            value: "Only-in-memory-password".into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: "Only-in-memory-password".into(),
        },
    ] {
        app.effect(&effect).unwrap();
    }
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(app, |state| {
        state["page"] == "devices" && state["prepared_identity"]["state"] == "prepared"
    });
    app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
        value: "My future computer".into(),
    })
    .unwrap();
    wait(app, |state| state["restart"]["checking"] == false);
    app.with_widget::<InstallerPreview, _>(|view| view.personal.prepared_handle().unwrap())
        .unwrap()
}
#[test]
fn reopening_guided_installer_recovers_exact_name_photo_and_signer_only_after_explicit_resume() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set("XDG_CONFIG_HOME", root.join("config"));
    let picture = root.join("picture.png");
    std::fs::write(&picture, include!("../fixtures/avatar.rs")).unwrap();
    let mut original = App::launched(&["--installer"]).unwrap();
    let identity = prepare(&mut original, &picture);
    let metadata = identity.metadata().unwrap();
    let identity_directory = identity.custody_directory().to_owned();
    let bytes = std::fs::read(identity_directory.join("primary.json")).unwrap();
    let signer = runtime_identity::load_from_path(identity_directory.join("primary.json")).unwrap();
    let signature = signer.sign(b"restart verification").unwrap();
    drop(original);
    drop(identity);
    let mut resumed = App::launched(&["--installer"]).unwrap();
    let offered = wait(&mut resumed, |state| {
        state["restart"]["state"] == "available"
    });
    assert!(offered["prepared_identity"]["public_key"].is_null());
    assert_eq!(offered["pairing"]["state"], "unconnected");
    fn find_continue(
        resources: &[ui::tilo_actions::Resource],
    ) -> Option<&ui::tilo_actions::ActionSpec> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| action.id == "continue-preview")
                .or_else(|| find_continue(&resource.children))
        })
    }
    let catalog = resumed.harness().resources();
    let continuation = find_continue(&catalog).expect("real guided Continue action");
    assert!(matches!(&continuation.contract.availability,
        ui::tilo_actions::Availability::Unavailable { code, .. } if code == "restart-review"));
    assert!(
        resumed
            .effect(&InstallerPreviewEffect::ContinuePreview)
            .is_err()
    );
    resumed
        .effect(&InstallerPreviewEffect::ResumeInstallerPreparation)
        .unwrap();
    let ready = wait(&mut resumed, |state| {
        state["page"] == "password" && state["restart"]["state"] == "resumed"
    });
    assert_eq!(ready["identity"], "new");
    assert_eq!(ready["name"], "My prepared name");
    assert_eq!(ready["device_name"], "My future computer");
    resumed
        .with_widget::<InstallerPreview, _>(|view| {
            assert!(view.journey.password.is_empty() && view.journey.confirmation.is_empty());
            assert!(!view.personal.imported());
            assert_eq!(view.personal.photo(), metadata.profile_photo.as_ref());
            assert!(view.belongings.home.is_none());
            assert!(view.storage.facts.is_none());
            assert!(!view.pairing.busy());
        })
        .unwrap();
    let recovered =
        runtime_identity::load_from_path(identity_directory.join("primary.json")).unwrap();
    assert_eq!(recovered.group_public_key(), signer.group_public_key());
    assert_eq!(recovered.sign(b"restart verification").unwrap(), signature);
    assert_eq!(
        std::fs::read(identity_directory.join("primary.json")).unwrap(),
        bytes
    );
    let draft =
        std::fs::read_to_string(root.join("state/tilo/installer/preparations/draft.json")).unwrap();
    assert!(!draft.contains("Only-in-memory-password"));
    assert!(!draft.contains("secret_bytes"));
    assert!(!root.join("config/tilo/persona-active").exists());
    drop(resumed);
    let mut fresh = App::launched(&["--installer"]).unwrap();
    wait(&mut fresh, |state| state["restart"]["state"] == "available");
    fresh
        .effect(&InstallerPreviewEffect::StartNewInstallerPreparation)
        .unwrap();
    let state = wait(&mut fresh, |state| state["restart"]["state"] == "retired");
    assert_eq!(state["page"], "destination");
    assert!(state["prepared_identity"]["public_key"].is_null());
    assert_eq!(
        std::fs::read(identity_directory.join("primary.json")).unwrap(),
        bytes
    );
}

fn has_control(root: &dyn Widget, id: &str) -> bool {
    root.test_id() == Some(id)
        || root
            .children()
            .into_iter()
            .any(|child| has_control(child, id))
}
fn enter_password(app: &mut App, password: &str) {
    app.settle();
    let unlocking = app
        .with_widget::<InstallerPreview, _>(|view| {
            let unlocking = view.unlocking_preparation();
            assert!(has_control(view, "installer.password"));
            assert_eq!(has_control(view, "installer.confirmation"), !unlocking);
            unlocking
        })
        .unwrap();
    app.effect(&InstallerPreviewEffect::SetPreviewPassword {
        value: password.into(),
    })
    .unwrap();
    let confirmation = app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
        value: password.into(),
    });
    if unlocking {
        assert!(confirmation.is_err());
    } else {
        confirmation.unwrap();
    }
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
}

fn await_checkpoint(app: &mut App, accounts: usize) {
    wait(app, |state| {
        assert_ne!(
            state["account_checkpoint"]["failed"], true,
            "checkpoint failed: {state}"
        );
        state["accounts"]["can_continue"] == true
            && state["account_checkpoint"]["saving"] == false
            && state["account_checkpoint"]["accounts"] == accounts
    });
}
fn go_to_accounts(app: &mut App) {
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(read(app)["page"], "purpose");
    choose(app, "computer");
    app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
        .unwrap();
    for category in ["files", "flatpak"] {
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: category.into(),
            included: false,
        })
        .unwrap();
    }
    wait(app, |state| state["belongings_checking"] == false);
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(read(app)["page"], "accounts");
}
fn assert_restored_grants(app: &mut App, ids: &[String]) {
    let handle = app
        .with_widget::<InstallerPreview, _>(|view| view.accounts.prepared_handle().unwrap())
        .unwrap();
    assert_eq!(handle.selected_ids(), ids);
    let store = ::accounts::AccountStore::load_in(&handle.config_root());
    let spotify = ::accounts::AccountId::new("music.spotify");
    assert_eq!(store.get(&spotify).unwrap().display_name, "Spotify");
    let secret = ::accounts::SecretVault::open_in(&handle.config_root())
        .get(&spotify)
        .unwrap();
    let token = secret.token.unwrap();
    assert!(token.contains("private-checkpoint-access"));
    assert!(token.contains("private-checkpoint-refresh"));
    assert!(!read(app).to_string().contains("private-checkpoint-access"));
}
fn open_at_saved_password() -> App {
    let mut app = App::launched(&["--installer"]).unwrap();
    wait(&mut app, |state| state["restart"]["state"] == "available");
    app.effect(&InstallerPreviewEffect::ResumeInstallerPreparation)
        .unwrap();
    wait(&mut app, |state| {
        state["page"] == "password" && state["restart"]["state"] == "resumed"
    });
    app
}

#[cfg(target_os = "linux")]
#[test]
fn resumed_draft_without_account_checkpoint_requires_review_before_bulk_import() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    for path in [&home, &config] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = config.join("rclone/rclone.conf");
    let source_bytes = b"[Files]\ntype=webdav\nurl=https://files.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n";
    std::fs::write(&source, source_bytes).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set("HOME", &home);
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set("XDG_CONFIG_HOME", &config);
    let picture = root.join("picture.png");
    std::fs::write(&picture, include!("../fixtures/avatar.rs")).unwrap();
    let mut original = App::launched(&["--installer"]).unwrap();
    let identity = prepare_with_migration(&mut original, &picture, true);
    assert_eq!(read(&mut original)["page"], "devices");
    let draft: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("state/tilo/installer/preparations/draft.json")).unwrap(),
    )
    .unwrap();
    assert!(draft["account_checkpoint"].is_null());
    drop(original);
    drop(identity);

    let mut resumed = open_at_saved_password();
    enter_password(&mut resumed, "Only-in-memory-password");
    wait(&mut resumed, |state| state["page"] == "devices");
    resumed
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(read(&mut resumed)["page"], "purpose");
    choose(&mut resumed, "computer");
    assert_eq!(read(&mut resumed)["page"], "belongings");
    // Normal forward navigation rediscovers metadata without importing
    // credentials or requiring the person to know about the recheck action.
    wait(&mut resumed, |state| state["belongings_checking"] == false);
    resumed
        .with_widget::<InstallerPreview, _>(|view| {
            assert!(view.belongings.roots.is_some());
            assert!(view.accounts.prepared_handle().is_none());
        })
        .unwrap();
    resumed
        .effect(&InstallerPreviewEffect::TogglePreviewDetails)
        .unwrap();
    for category in ["files", "flatpak"] {
        resumed
            .effect(&InstallerPreviewEffect::SetPreviewCategory {
                category: category.into(),
                included: false,
            })
            .unwrap();
    }
    resumed
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(read(&mut resumed)["page"], "accounts");
    resumed.settle();
    resumed
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.accounts.navigation_step(), "overview");
            assert!(view.accounts.prepared_handle().is_none());
            assert!(has_control(view, "installer.account.review-saved"));
        })
        .unwrap();
    assert!(
        resumed
            .effect(&InstallerPreviewEffect::AccountStep {
                step: "import-saved".into(),
                selection: None,
            })
            .is_err()
    );
    assert_eq!(read(&mut resumed)["accounts"]["prepared_accounts"], 0);
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);

    // The actual footer opens review first, then imports the reviewed selection.
    resumed
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    resumed.settle();
    resumed
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.accounts.navigation_step(), "review-saved");
            assert!(view.accounts.prepared_handle().is_none());
            assert!(has_control(view, "installer.account.import-saved"));
        })
        .unwrap();
    resumed
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    wait(&mut resumed, |state| {
        state["accounts"]["prepared_accounts"] == 1 && state["accounts"]["can_continue"] == true
    });
    let prepared = resumed
        .with_widget::<InstallerPreview, _>(|view| view.accounts.prepared_handle().unwrap())
        .unwrap();
    let store = ::accounts::AccountStore::load_in(&prepared.config_root());
    assert_eq!(store.len(), 1);
    let account = &store.all()[0];
    assert_eq!(
        account.config["server_url"],
        "https://files.example.test/dav"
    );
    let secret = ::accounts::SecretVault::open_in(&prepared.config_root())
        .get(&account.id)
        .unwrap();
    assert!(secret.password.is_empty());
    assert_eq!(
        account_custody::password_credential(&secret).as_str(),
        "fixture-app-password"
    );
    assert_eq!(read(&mut resumed)["page"], "accounts");
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    assert!(
        !read(&mut resumed)
            .to_string()
            .contains("fixture-app-password")
    );
}

fn verify_stale_account_generation(stale: &mut App, expected_key: [u8; 32]) {
    enter_password(stale, "Only-in-memory-password");
    let changed = wait(stale, |state| state["restart"]["state"] == "stale");
    assert_eq!(changed["page"], "password");
    assert_eq!(changed["accounts"]["prepared_accounts"], 0);
    assert_eq!(
        changed["prepared_identity"]["public_key"],
        hex::encode(expected_key)
    );
    stale
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.error, Some("installer-restart-changed"));
            assert!(view.accounts.prepared_handle().is_none());
            assert!(view.belongings.home.is_none());
            assert!(!view.pairing.busy());
        })
        .unwrap();
    paint_recovery_screen(
        stale,
        "installer-changed-preparation",
        &["installer.restart.retry"],
    );
    stale
        .effect(&InstallerPreviewEffect::RetryInstallerPreparation)
        .unwrap();
    wait(stale, |state| state["restart"]["state"] == "available");
    stale
        .effect(&InstallerPreviewEffect::ResumeInstallerPreparation)
        .unwrap();
    wait(stale, |state| {
        state["page"] == "password" && state["restart"]["state"] == "resumed"
    });
    enter_password(stale, "Only-in-memory-password");
    let state = wait(stale, |state| state["page"] == "devices");
    assert_eq!(state["accounts"]["prepared_accounts"], 0);
    assert_eq!(
        state["prepared_identity"]["public_key"],
        hex::encode(expected_key)
    );
}

fn paint_recovery_screen(app: &mut App, scenario: &str, controls: &[&str]) {
    for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
            app.harness()
                .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
            app.harness().resize(viewport).unwrap();
            super::visual_tests::paint_app(app, scenario, controls, viewport, tag);
        }
    }
}

#[test]
fn guided_reopen_authenticates_connections_before_advancing_and_empty_selection_supersedes_them() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    env.set("XDG_STATE_HOME", root.join("state"));
    env.set("XDG_CONFIG_HOME", root.join("config"));
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(root.join("config")).unwrap();
    env.set("HOME", &home);
    let picture = root.join("picture.png");
    std::fs::write(&picture, include!("../fixtures/avatar.rs")).unwrap();
    let mut original = App::launched(&["--installer"]).unwrap();
    let identity = prepare_with_migration(&mut original, &picture, true);
    let expected_key = identity.public_key();
    drop(identity);
    go_to_accounts(&mut original);
    await_checkpoint(&mut original, 0);
    let accounts = accounts::checkpoint_fixture_for_journey(&root.join("account-fixture"));
    let ids = accounts.prepared_handle().unwrap().selected_ids();
    original
        .with_widget::<InstallerPreview, _>(|view| view.accounts = accounts)
        .unwrap();
    await_checkpoint(&mut original, 2);
    original
        .effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    // Continue saves the account checkpoint and prepares belongings first. A
    // build without direct installation has no storage decision; Review offers
    // the USB installer.
    let advanced = wait(&mut original, |state| {
        state["page"] != "accounts" || state["media_preparation"]["state"] == "failed"
    });
    assert_eq!(advanced["page"], "review", "{advanced}");
    drop(original);
    let mut stale = open_at_saved_password();
    let mut resumed = open_at_saved_password();
    paint_recovery_screen(
        &mut resumed,
        "installer-unlock-preparation",
        &["installer.password", "installer.next", "installer.back"],
    );
    enter_password(&mut resumed, "wrong-checkpoint-password");
    wait(&mut resumed, |state| {
        state["account_checkpoint"]["failed"] == true
    });
    assert_eq!(read(&mut resumed)["page"], "password");
    assert_eq!(read(&mut resumed)["accounts"]["prepared_accounts"], 0);
    enter_password(&mut resumed, "Only-in-memory-password");
    wait(&mut resumed, |state| state["page"] == "devices");
    resumed
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.journey.password, "Only-in-memory-password");
            assert_eq!(view.journey.confirmation, view.journey.password);
        })
        .unwrap();
    assert_restored_grants(&mut resumed, &ids);
    go_to_accounts(&mut resumed);
    // Canonical Spotify removal has no OAuthCloud HTTP revocation branch;
    // this fixture exercises actual local custody removal without a network adapter.
    for (index, id) in ids.iter().enumerate() {
        resumed
            .effect(&InstallerPreviewEffect::AccountStep {
                step: "remove-prepared".into(),
                selection: Some(id.clone()),
            })
            .unwrap();
        resumed
            .effect(&InstallerPreviewEffect::AccountStep {
                step: "confirm".into(),
                selection: None,
            })
            .unwrap();
        await_checkpoint(&mut resumed, ids.len().checked_sub(index + 1).unwrap());
    }
    drop(resumed);
    verify_stale_account_generation(&mut stale, expected_key);
    drop(stale);
    let mut empty = open_at_saved_password();
    enter_password(&mut empty, "Only-in-memory-password");
    wait(&mut empty, |state| state["page"] == "devices");
    assert_eq!(read(&mut empty)["accounts"]["prepared_accounts"], 0);
    empty
        .with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.accounts.prepared_handle().unwrap().count(), 0);
            assert!(view.belongings.home.is_none());
        })
        .unwrap();
}
