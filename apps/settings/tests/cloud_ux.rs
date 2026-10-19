//! Production hosted frames for the provider connection and Files import journey.
//! Register this module in the isolated-XDG test binary, not the shared UI binary.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Instant;

use settings::SettingsShell;
use settings::accounts::AccountRoot;
use settings::panes::accounts::{AccountsPane, BrowserOpener};
use settings::panes::model::PaneModel;
use ui::framework::testing::TestApp;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
#[cfg(target_os = "linux")]
type Files = TestApp<file_explorer::FileExplorerShell>;

struct Browser;
impl BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        let url = url.to_owned();
        std::thread::spawn(move || {
            let _ = tilo_oauth::fake::browse(&url);
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::LaunchOutcome::Opened);
        rx
    }
}

fn capture<A: ui::framework::ResourceApp>(app: &mut TestApp<A>, name: &str) -> TestResult {
    app.settle();
    app.harness().relayout();
    app.settle();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/ux-cloud")
        .join(format!("{name}.png"));
    app.harness().render_png(&output);
    assert_captured(&output);
    let inspection = app.harness().inspection_json()?;
    let bindings = if name.starts_with("files-") {
        app.surface::<file_explorer::FileExplorerWidget, _>(|surface| {
            ui::widget::controls::control_action_bindings(surface)
        })
        .ok_or("mounted Files surface")?
    } else {
        app.surface::<settings::SettingsApp, _>(|surface| {
            ui::widget::controls::control_action_bindings(surface)
        })
        .ok_or("mounted Settings surface")?
    };
    assert_frame(app, name, &inspection, &bindings);
    std::fs::write(output.with_extension("json"), inspection)?;
    println!("cloud UX frame: {}", output.display());
    Ok(())
}

fn assert_captured(output: &std::path::Path) {
    assert!(output.is_file(), "no captured frame: {}", output.display());
}

fn assert_frame<A: ui::framework::ResourceApp>(
    app: &mut TestApp<A>,
    name: &str,
    inspection: &str,
    bindings: &[ui::widget::ControlBindingMetadata],
) {
    if name.starts_with("files-") {
        assert!(
            !inspection.contains("This folder is empty"),
            "cloud browser is not a local empty folder"
        );
        assert!(
            !inspection.contains("0 items"),
            "count must describe the visible cloud entries"
        );
        assert!(
            !inspection.contains("Syncing"),
            "import-only sources do not sync"
        );
        assert!(!inspection.contains("Essay.docx: Essay.docx:"));
        assert!(!inspection.contains("note.txt: note.txt:"));
        for id in [
            "files.cloud.root",
            "files.cloud.import",
            "files.cloud.cancel",
            "files.cloud.retry",
            "files.cloud.reveal",
        ] {
            if let Some(binding) = bindings.iter().find(|binding| binding.control_id == id) {
                assert!(
                    matches!(binding.kind, ui::widget::ControlBindingKind::Action(_)),
                    "{id} uses the shared action door"
                );
            }
            assert!(
                !matches!(id, "files.cloud.root" | "files.cloud.import")
                    || bindings.iter().any(|binding| binding.control_id == id),
                "missing cloud control {id}"
            );
        }
        let running = name == "files-import-preparing";
        let retry = matches!(
            name,
            "files-import-cancelled-retry" | "files-import-error-retry"
        );
        for (label, visible) in [("Cancel import", running), ("Retry failed", retry)] {
            assert_eq!(
                app.harness()
                    .a11y_nodes()
                    .iter()
                    .filter(|(_, node)| node.role() == ui::accesskit::Role::Button
                        && node.label() == Some(label))
                    .count(),
                usize::from(visible),
                "{label} visibility in {name}"
            );
        }
        assert_eq!(
            app.harness()
                .a11y_nodes()
                .iter()
                .filter(|(_, node)| node.role() == ui::accesskit::Role::ProgressIndicator)
                .count(),
            usize::from(running),
            "activity only while running"
        );
    } else if name == "google-mail-only" {
        assert!(inspection.contains("ada@example.org"));
        assert!(inspection.contains("Gmail"));
        assert!(!inspection.contains("Google Drive files"));
        assert!(!inspection.contains("No Email accounts yet"));
        assert!(!inspection.contains("No Google accounts yet"));
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.control_id.starts_with("settings.accounts.files."))
        );
    } else if name.starts_with("google-connected") {
        assert_connected_frame(app, inspection, bindings);
    } else {
        assert!(inspection.contains("Connect Google"));
        assert!(!inspection.contains("Connect Google Drive"));
        for (id, action) in [
            ("settings.accounts.form.save", "save-account-form"),
            ("settings.accounts.form.cancel", "cancel-account-form"),
        ] {
            assert!(
                bindings.iter().any(|binding| binding.control_id == id
                    && matches!(&binding.kind,
                        ui::widget::ControlBindingKind::Action(address) if address.action == action
                    )),
                "{id} is bound to {action}"
            );
        }
    }
}

fn assert_connected_frame<A: ui::framework::ResourceApp>(
    app: &mut TestApp<A>,
    inspection: &str,
    bindings: &[ui::widget::ControlBindingMetadata],
) {
    assert!(inspection.contains("ada@example.org"));
    assert!(!inspection.contains("mail.google.com"));
    assert!(!inspection.contains("drive.google.com"));
    assert!(inspection.contains("Import only"));
    assert!(inspection.contains("No ongoing sync"));
    assert!(inspection.contains("Connected, not synced yet"));
    assert!(!inspection.contains("Not checked yet"));
    assert!(!inspection.contains("No synchronization status"));
    for empty in [
        "No Email accounts",
        "No Calendar accounts",
        "No Nextcloud accounts",
        "Tilo browser identities",
    ] {
        assert!(
            !inspection.contains(empty),
            "empty provider omitted: {empty}"
        );
    }
    assert!(
        matches!(
            (inspection.find("Google"), inspection.find("Add account")),
            (Some(google), Some(add)) if google < add
        ),
        "Google provider is shown before the provider chooser"
    );
    assert!(inspection.contains("Google Drive files"));
    assert!(inspection.contains("Gmail"));
    assert!(
        inspection.find("Add account") < inspection.find("Import Thunderbird account settings")
    );
    assert!(!inspection.contains("google-drive://"));
    assert!(!inspection.contains("No Mail accounts"));
    assert!(!inspection.contains("Connect Google"));
    for kind in settings::accounts::AccountKind::ALL {
        let id = format!("settings.accounts.add.{}", kind.slug());
        assert!(
            bindings.iter().any(|binding| binding.control_id == id
                && matches!(&binding.kind, ui::widget::ControlBindingKind::Action(address)
                if address.action == "add-account")),
            "provider remains reachable: {id}"
        );
    }
    let nodes = app.harness().a11y_nodes();
    assert_eq!(
        nodes
            .iter()
            .filter(|(_, node)| node.label() == Some("Google"))
            .count(),
        1,
        "one Google provider card"
    );
    for label in [
        "Remove Files",
        "Disconnect Mail",
        "Sign in again — Files",
        "Sign in again — Mail",
    ] {
        assert_button(app, label);
    }
    for (id, action) in [
        ("settings.accounts.reauth.ada@example.org", "sign-in-again"),
        ("settings.accounts.remove.ada@example.org", "remove-account"),
        ("settings.accounts.edit.ada@example.org", "edit-account"),
        ("settings.accounts.test.ada@example.org", "test-account"),
    ] {
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| binding.control_id == id
                    && matches!(&binding.kind, ui::widget::ControlBindingKind::Action(address)
                    if address.action == action && address.resource == "settings.account:ada@example.org"))
                .count(),
            1,
            "linked Gmail keeps {id}"
        );
    }
}

#[cfg(target_os = "linux")]
fn wait(
    app: &mut TestApp<file_explorer::FileExplorerShell>,
    service: &mut fs::cloud::CloudService,
    ready: impl Fn(&mut Files) -> TestResult<bool>,
) -> TestResult {
    let started = Instant::now();
    loop {
        service.pump();
        app.settle();
        if ready(app)? {
            return Ok(());
        }
        assert_within_bound(started, "cloud journey did not settle");
        std::thread::yield_now();
    }
}

fn assert_button<A: ui::framework::ResourceApp>(app: &mut TestApp<A>, label: &str) {
    let nodes = app.harness().a11y_nodes();
    assert_eq!(
        nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(label)
                && node.role() == ui::accesskit::Role::Button)
            .count(),
        1,
        "one accessible {label} button"
    );
}

#[cfg(target_os = "linux")]
fn wait_phase(
    app: &mut TestApp<file_explorer::FileExplorerShell>,
    service: &mut fs::cloud::CloudService,
    phase: &str,
) -> TestResult {
    wait(app, service, |app| {
        Ok(progress_phase(app)? == phase
            && app.state().ops.entries().count() > 0
            && app
                .state()
                .ops
                .entries()
                .all(|(_, op)| op.settled.is_some()))
    })
}

#[cfg(target_os = "linux")]
fn progress_phase(app: &mut Files) -> TestResult<String> {
    let progress = app
        .call("cloud-progress", serde_json::json!({}))?
        .ok_or("missing progress value")?;
    Ok(progress
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing progress phase")?
        .to_owned())
}

struct Profile {
    _env: tilo_test_env::ScopedEnv,
    /// Keeps the isolated profile on disk for the test's lifetime.
    _scratch: tempfile::TempDir,
    // Only the Linux import journey runs a cloud service inside the profile.
    #[cfg(target_os = "linux")]
    home: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    runtime: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    state: std::path::PathBuf,
    root: AccountRoot,
}

impl Profile {
    fn new() -> TestResult<Self> {
        let mut env = tilo_test_env::ScopedEnv::new();
        let scratch = tempfile::tempdir()?;
        for name in ["home", "config", "runtime", "data", "cache"] {
            std::fs::create_dir(scratch.path().join(name))?;
        }
        for (key, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            env.set(key, scratch.path().join(name));
        }
        let home = scratch.path().join("home");
        let runtime = scratch.path().join("runtime");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
        let home_entries = std::fs::read_dir(&home)?.count();
        let root = AccountRoot::at(scratch.path().join("config/tilo"));
        assert_empty_profile(home_entries, &root);

        Ok(Self {
            _env: env,
            #[cfg(target_os = "linux")]
            state: scratch.path().join("state"),
            _scratch: scratch,
            #[cfg(target_os = "linux")]
            home,
            #[cfg(target_os = "linux")]
            runtime,
            root,
        })
    }

    #[cfg(target_os = "linux")]
    fn service(
        &self,
        custodian: account_custody::Custodian,
    ) -> TestResult<fs::cloud::CloudService> {
        let mut service = fs::cloud::CloudService::new(
            fs::cloud::ServiceConfig {
                config_root: Some(self.root.path().ok_or("isolated account root")?),
                mounts_root: self.home.join("Remote"),
                state_root: self.state.clone(),
                refresher: Arc::new(custodian),
                control_socket: None,
                file_socket: None,
            },
            Box::new(fs::cloud::InProcessLauncher::default()),
        );
        service.reconcile();
        assert_import_only(&service);
        service.serve_control(
            &runtime_fs::cloud::files_socket_in(&self.runtime),
            runtime_fs::cloud::CloudSocket::Files,
        )?;

        Ok(service)
    }
}

fn open_settings(
    root: &AccountRoot,
    custodian: &account_custody::Custodian,
) -> TestResult<TestApp<SettingsShell>> {
    let mut settings = TestApp::<SettingsShell>::open_data_dir(
        &root.path().ok_or("explicit isolated Settings profile")?,
    )?;
    settings
        .surface::<settings::SettingsApp, _>(|app| {
            // Preserve the live pane's host action door while replacing only
            // its provider dependencies with deterministic local services.
            let mut model = app.accounts().model().borrow_mut();
            let hosted = std::mem::replace(&mut *model, AccountsPane::in_root(root.clone()));
            *model = hosted
                .with_custodian(custodian.clone())
                .with_browser_opener(Arc::new(Browser));
        })
        .ok_or("mounted Settings surface")?;
    settings.effect_on(
        &settings::effects::pane_row_id(settings::categories::Category::ExternalAccounts),
        &settings::effects::open_pane(settings::categories::Category::ExternalAccounts),
    )?;
    settings.call_on(
        "settings.root",
        "add-account",
        serde_json::json!({"kind": "google-drive"}),
    )?;
    Ok(settings)
}

#[cfg(target_os = "linux")]
fn capture_provider_form(settings: &mut TestApp<SettingsShell>) -> TestResult {
    for (theme_name, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
        settings.harness().set_theme(theme);
        for size in [(1100, 800), (640, 800)] {
            settings.harness().resize(size)?;
            capture(
                settings,
                &format!("google-services-{theme_name}-{}", size.0),
            )?;
            assert_provider_controls(settings);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sign_in(
    settings: &mut TestApp<SettingsShell>,
    root: &AccountRoot,
) -> TestResult<accounts::Account> {
    settings.call_on(
        "settings.root",
        "select-account-services",
        serde_json::json!({
            "files": true, "mail": false, "files_mode": "import"
        }),
    )?;
    capture(settings, "google-services-import-selected")?;
    settings.call_on("settings.root", "save-account-form", serde_json::json!({}))?;
    let finished = settings
        .surface::<settings::SettingsApp, _>(|app| {
            app.accounts()
                .model()
                .borrow_mut()
                .finish_sign_in_step_for_test(tilo_test_env::hang_bound())
        })
        .ok_or("sign-in result")?;
    let store = root.store();
    assert_sign_in_finished(finished, store.len());
    let account = store.all().first().ok_or("signed-in account")?.clone();
    assert_import_account(&account);
    Ok(account)
}

#[cfg(target_os = "linux")]
fn row(id: &str) -> String {
    format!("files.cloud-entry:{}", hex::encode(id))
}

#[cfg(target_os = "linux")]
fn browse_and_select(
    files: &mut Files,
    service: &mut fs::cloud::CloudService,
    account: &accounts::Account,
    document: &str,
    plain: &str,
) -> TestResult {
    files.call(
        "browse-cloud",
        serde_json::json!({"account": account.id.as_str()}),
    )?;
    wait(files, service, |app| {
        Ok(app
            .actions()?
            .iter()
            .any(|action| action.address.resource.as_str() == row(document)))
    })?;
    capture(files, "files-import-browser")?;
    assert_button(files, "Import once…");
    let bindings = files
        .surface::<file_explorer::FileExplorerWidget, _>(|surface| {
            ui::widget::controls::control_action_bindings(surface)
        })
        .ok_or("mounted Files surface")?;
    assert_action_bindings(&bindings);
    for id in [document, plain] {
        files.call_on(&row(id), "select", serde_json::json!({}))?;
    }
    capture(files, "files-selected-export")?;
    for id in [document, plain] {
        let actions = files.actions()?;
        assert_read_only_row(&actions, id);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn cancel_import(
    files: &mut Files,
    service: &mut fs::cloud::CloudService,
    recorder: &ui::host::HostRecorder,
    destination: &std::path::Path,
) -> TestResult {
    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    files.call("import-cloud", serde_json::json!({}))?;
    files.settle();
    capture(files, "files-destination-cancelled")?;
    assert_no_import_job(files);
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.to_path_buf()));
    files.call("import-cloud", serde_json::json!({}))?;
    let started = Instant::now();
    loop {
        files.settle();
        let phase = progress_phase(files)?;
        if matches!(phase.as_str(), "preparing" | "exporting" | "copying") {
            break;
        }
        assert_within_bound(started, "import did not start preparing");
        std::thread::yield_now();
    }
    capture(files, "files-import-preparing")?;
    assert_button(files, "Cancel import");
    files.call("cancel-cloud", serde_json::json!({}))?;
    wait_phase(files, service, "cancelled")?;
    capture(files, "files-import-cancelled-retry")?;
    assert_button(files, "Retry failed");
    assert_no_imported_files(destination);

    Ok(())
}

#[cfg(target_os = "linux")]
fn recover_import(
    files: &mut Files,
    service: &mut fs::cloud::CloudService,
    drive: &cloud_source_gdrive::fake::FakeDrive,
) -> TestResult {
    drive.set_offline(true);
    files.call("retry-cloud", serde_json::json!({}))?;
    wait_phase(files, service, "failed")?;
    capture(files, "files-import-error-retry")?;
    assert_button(files, "Retry failed");
    drive.set_offline(false);
    files.call("retry-cloud", serde_json::json!({}))?;
    wait_phase(files, service, "done")?;
    wait(files, service, |app| {
        Ok(app.state().ops.entries().count() > 0
            && app
                .state()
                .ops
                .entries()
                .all(|(_, op)| op.settled.is_some()))
    })?;
    capture(files, "files-import-complete")?;
    assert_button(files, "Show destination");

    Ok(())
}

#[cfg(target_os = "linux")]
fn save_imported_data(
    recorder: &ui::host::HostRecorder,
    destination: &std::path::Path,
    home: &std::path::Path,
) -> TestResult {
    let text = std::fs::read(destination.join("note.txt"))?;
    let docx = std::fs::read(destination.join("Essay.docx"))?;
    assert_imported_data(recorder, destination, home, &text, &docx);
    let imported =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ux-cloud/imported");
    std::fs::create_dir_all(&imported)?;
    std::fs::write(imported.join("Essay.docx"), &docx)?;
    std::fs::copy(destination.join("note.txt"), imported.join("note.txt"))?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn assert_imported_data(
    recorder: &ui::host::HostRecorder,
    destination: &std::path::Path,
    home: &std::path::Path,
    text: &[u8],
    docx: &[u8],
) {
    assert!(
        recorder
            .pick_requests()
            .iter()
            .all(ui::portal::PickRequest::is_writable_directory)
    );
    assert!(
        destination.join("Essay.docx").is_file(),
        "native document exported to Office"
    );
    assert_eq!(text, b"independent cloud original");
    assert_eq!(docx, {
        mod fixture {
            include!("../../../crates/system/cloud-source-gdrive/tests/fixtures/docx.rs");
        }
        fixture::BYTES
    });

    assert!(
        !home.join("Downloads").exists(),
        "no implicit Downloads destination"
    );
}

fn assert_google_completion(
    finished: bool,
    store: &accounts::AccountStore,
    gmail: &test_http::Responder,
    count: usize,
) {
    assert!(finished);
    assert_eq!(store.len(), count);
    assert_eq!(gmail.next_request().target, "/profile");
}

fn assert_google_link(files: &accounts::Account) {
    assert_eq!(
        files
            .config
            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
            .and_then(serde_json::Value::as_str),
        Some("ada@example.org")
    );
}

fn assert_reauth_cancelled(current: &accounts::AccountStore, previous: &accounts::AccountStore) {
    assert_eq!(current, previous);
}

fn assert_mail_remaining(store: &accounts::AccountStore, inspection: &str) {
    assert_eq!(store.len(), 1);
    let mail = store.all().first();
    assert_eq!(mail.map(|mail| mail.type_id.as_str()), Some("email"));
    assert_eq!(
        mail.and_then(|mail| mail.config.get("email"))
            .and_then(serde_json::Value::as_str),
        Some("ada@example.org")
    );
    assert!(inspection.contains("ada@example.org"));
    assert!(!inspection.contains("Google Drive files"));
}

#[test]
fn google_connected_files_and_gmail_have_one_provider_card() -> TestResult {
    let profile = Profile::new()?;
    let drive = cloud_source_gdrive::fake::FakeDrive::start();
    let auth = tilo_oauth::fake::FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_gdrive::fake::SOURCE_TOKEN);
    let gmail = test_http::Responder::start(vec![
        test_http::Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"emailAddress":"ada@example.org"}"#),
    ]);
    // Both services share one registration, as required for one consent.
    let mut clients = account_custody::ClientTable::default();
    clients.google.client_id = "google-services-client".into();
    clients.google.scopes.files = vec!["https://www.googleapis.com/auth/drive".into()];
    clients.google.scopes.mail = vec!["https://mail.google.com/".into()];
    let custodian = profile
        .root
        .custodian()
        .with_client_table(clients)
        .with_issuer_endpoints(account_custody::OAuthIssuer::Google, auth.provider())
        .with_gmail_profile_endpoint(format!("{}/profile", gmail.url()))
        .with_storage_config(
            account_custody::OAuthCloud::GoogleDrive,
            serde_json::to_value(drive.config())?,
        );
    let mut settings = open_settings(&profile.root, &custodian)?;
    settings.call_on(
        "settings.root",
        "select-account-services",
        serde_json::json!({
            "files": true, "mail": true, "files_mode": "import"
        }),
    )?;
    settings.call_on("settings.root", "save-account-form", serde_json::json!({}))?;
    let finished = settings
        .surface::<settings::SettingsApp, _>(|app| {
            app.accounts()
                .model()
                .borrow_mut()
                .finish_sign_in_step_for_test(tilo_test_env::hang_bound())
        })
        .ok_or("sign-in result")?;
    let store = profile.root.store();
    assert_google_completion(finished, &store, &gmail, 2);
    let files = store
        .list(&account_custody::providers::GOOGLE_DRIVE_TYPE.into())
        .first()
        .copied()
        .ok_or("connected Files service")?;
    assert_google_link(files);
    settings.harness().resize((1100, 1500))?;
    for (name, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
        settings.harness().set_theme(theme);
        capture(
            &mut settings,
            &format!("google-connected-files-gmail-{name}"),
        )?;
        settings.harness().resize((640, 1800))?;
        capture(
            &mut settings,
            &format!("google-connected-files-gmail-{name}-640"),
        )?;
        settings.harness().resize((1100, 1500))?;
    }
    settings.call_on(
        "settings.account:ada@example.org",
        "sign-in-again",
        serde_json::json!({}),
    )?;
    settings.call_on(
        "settings.root",
        "cancel-account-form",
        serde_json::json!({}),
    )?;
    assert_reauth_cancelled(&profile.root.store(), &store);
    // Files removal must leave the independently bound Gmail service intact.
    settings.call_confirmed_on(
        &format!("settings.account:{}", files.id),
        "remove-account",
        serde_json::json!({}),
    )?;
    // Removal returns while its vault worker is running. Wait for the pane's
    // work (including its queued reload), not for the expected account count:
    // a completed failure must still reach the assertions below.
    let started = Instant::now();
    loop {
        settings.settle();
        let busy = settings
            .surface::<settings::SettingsApp, _>(|app| app.accounts().model().borrow().busy())
            .ok_or("mounted Settings surface")?;
        if !busy {
            break;
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "account removal did not finish"
        );
        std::thread::yield_now();
    }
    assert_mail_remaining(
        &profile.root.store(),
        &settings.harness().inspection_json()?,
    );
    Ok(())
}

#[test]
fn google_mail_only_is_not_presented_as_drive() -> TestResult {
    let profile = Profile::new()?;
    let auth = tilo_oauth::fake::FakeAuthorizationServer::start();
    let gmail = test_http::Responder::start(vec![
        test_http::Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"emailAddress":"ada@example.org"}"#),
    ]);
    let custodian = profile
        .root
        .custodian()
        .with_issuer_endpoints(account_custody::OAuthIssuer::Google, auth.provider())
        .with_gmail_profile_endpoint(format!("{}/profile", gmail.url()));
    let mut settings = open_settings(&profile.root, &custodian)?;
    settings.call_on(
        "settings.root",
        "select-account-services",
        serde_json::json!({
            "files": false, "mail": true, "files_mode": "import"
        }),
    )?;
    settings.call_on("settings.root", "save-account-form", serde_json::json!({}))?;
    let finished = settings
        .surface::<settings::SettingsApp, _>(|app| {
            app.accounts()
                .model()
                .borrow_mut()
                .finish_sign_in_step_for_test(tilo_test_env::hang_bound())
        })
        .ok_or("sign-in result")?;
    let store = profile.root.store();
    assert_google_completion(finished, &store, &gmail, 1);
    assert_mail_remaining(&store, &settings.harness().inspection_json()?);
    settings.harness().resize((1100, 1500))?;
    capture(&mut settings, "google-mail-only")?;
    Ok(())
}

// The import journey runs on the FUSE-backed cloud service, which exists
// only on Linux; the Settings-only journeys above stay portable.
#[test]
#[cfg(target_os = "linux")]
fn google_services_and_files_import_have_real_hosted_frames() -> TestResult {
    let profile = Profile::new()?;
    let drive = cloud_source_gdrive::fake::FakeDrive::start();
    let auth = tilo_oauth::fake::FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_gdrive::fake::SOURCE_TOKEN);
    let custodian = profile
        .root
        .custodian()
        .with_provider(
            account_custody::OAuthCloud::GoogleDrive,
            auth.provider(),
            "drive-client",
        )
        .with_storage_config(
            account_custody::OAuthCloud::GoogleDrive,
            serde_json::to_value(drive.config())?,
        );
    let mut settings = open_settings(&profile.root, &custodian)?;
    capture_provider_form(&mut settings)?;
    let account = sign_in(&mut settings, &profile.root)?;
    let document = drive.put_native(
        &drive.root_id(),
        "Essay",
        "application/vnd.google-apps.document",
    );
    let plain = drive.put_file(&drive.root_id(), "note.txt", b"independent cloud original");
    let mut service = profile.service(custodian)?;
    let (host, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&host);
    let mut files = Files::launched(&[profile.home.to_str().ok_or("home path")?])?;
    browse_and_select(&mut files, &mut service, &account, &document, &plain)?;
    let destination = profile.home.join("Chosen folder");
    std::fs::create_dir(&destination)?;
    cancel_import(&mut files, &mut service, &recorder, &destination)?;
    recover_import(&mut files, &mut service, &drive)?;
    save_imported_data(&recorder, &destination, &profile.home)?;
    service.shutdown();
    Ok(())
}

#[cfg(target_os = "linux")]
fn assert_within_bound(started: Instant, message: &str) {
    assert!(started.elapsed() < tilo_test_env::hang_bound(), "{message}");
}

fn assert_empty_profile(home_entries: usize, root: &AccountRoot) {
    assert_eq!(home_entries, 0);
    assert!(
        root.store().is_empty(),
        "fixture begins before authorization"
    );
}

#[cfg(target_os = "linux")]
fn assert_import_only(service: &fs::cloud::CloudService) {
    assert!(
        service.mounted().is_empty(),
        "import does not create a synced mount"
    );
}

#[cfg(target_os = "linux")]
fn assert_provider_controls(settings: &mut TestApp<SettingsShell>) {
    let nodes = settings.harness().a11y_nodes();
    for (name, role) in [
        ("Files", ui::accesskit::Role::CheckBox),
        ("Mail", ui::accesskit::Role::CheckBox),
        ("Continue", ui::accesskit::Role::Button),
        ("Cancel", ui::accesskit::Role::Button),
    ] {
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, node)| node.label() == Some(name) && node.role() == role)
                .count(),
            1,
            "one accessible {name} control in the real form"
        );
    }
}

#[cfg(target_os = "linux")]
fn assert_sign_in_finished(finished: bool, account_count: usize) {
    assert!(finished);
    assert_eq!(account_count, 1);
}

#[cfg(target_os = "linux")]
fn assert_import_account(account: &accounts::Account) {
    assert_eq!(
        accounts::cloud_types::files_mode(account),
        Some(accounts::cloud_types::FilesMode::Import)
    );
}

#[cfg(target_os = "linux")]
fn assert_action_bindings(bindings: &[ui::widget::ControlBindingMetadata]) {
    assert!(
        bindings
            .iter()
            .any(|binding| matches!(binding.kind, ui::widget::ControlBindingKind::Action(_))),
        "cloud controls publish action bindings"
    );
}

#[cfg(target_os = "linux")]
fn assert_read_only_row(actions: &[tilo_actions::ActionDescriptor], id: &str) {
    let row_actions = actions
        .iter()
        .filter(|action| action.address.resource.as_str() == row(id));
    assert!(
        row_actions
            .clone()
            .any(|action| action.address.action == "select")
    );
    assert!(
        row_actions.clone().all(|action| !matches!(
            action.address.action.as_str(),
            "write" | "rename" | "delete"
        )),
        "the import browser must not offer cloud mutations"
    );
}

#[cfg(target_os = "linux")]
fn assert_no_import_job(files: &Files) {
    assert_eq!(
        files.state().ops.entries().count(),
        0,
        "chooser cancellation creates no job"
    );
}

#[cfg(target_os = "linux")]
fn assert_no_imported_files(destination: &std::path::Path) {
    assert!(!destination.join("Essay.docx").exists());
    assert!(!destination.join("note.txt").exists());
}
