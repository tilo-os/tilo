//! Settings and Files acceptance against the production socket service and Drive
//! adapter. Only OAuth/browser and the remote HTTP provider are local fixtures;
//! the writable-directory portal is the shared recording host, not a compositor.
use super::*;
use std::sync::Arc;
use std::time::Instant;

use accounts::cloud_types::FilesMode;
use cloud_source_gdrive::fake::{FakeDrive, SOURCE_TOKEN};
use file_explorer::resource::FileExplorerShell;
use fs::cloud::{CloudService, InProcessLauncher, ServiceConfig};
use ui::framework::testing::TestApp;

struct Browser;
impl BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        let url = url.to_owned();
        std::thread::spawn(move || tilo_oauth::fake::browse(&url).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ui::portal::LaunchOutcome::Opened).unwrap();
        rx
    }
}

#[track_caller]
fn wait(
    app: &mut TestApp<FileExplorerShell>,
    service: &mut CloudService,
    mut condition: impl FnMut(&mut TestApp<FileExplorerShell>) -> bool,
) {
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    loop {
        service.pump();
        app.settle();
        if condition(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "cloud interaction did not settle: progress={}",
            progress(app)
        );
        std::thread::yield_now();
    }
}

fn row(id: &str) -> String {
    format!("files.cloud-entry:{}", hex::encode(id))
}

fn select(app: &mut TestApp<FileExplorerShell>, id: &str) {
    let address = row(id);
    assert!(
        app.actions().unwrap().iter().any(|action| {
            action.address.resource.as_str() == address && action.address.action == "select"
        }),
        "cloud selection must be published"
    );
    app.call_on(&address, "select", serde_json::json!({}))
        .unwrap();
}

fn progress(app: &mut TestApp<FileExplorerShell>) -> serde_json::Value {
    app.call("cloud-progress", serde_json::json!({}))
        .unwrap()
        .unwrap()
}

fn completed(app: &mut TestApp<FileExplorerShell>) -> bool {
    let state = app.state();
    state.ops.entries().count() > 0 && state.ops.pending().count() == 0
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one chronological acceptance journey verifies sign-in, published actions, transfer receipts, failure, cancellation and undo on the same account"
)]
fn settings_import_connection_to_files_exports_independent_copies_and_undo() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let config = scratch.path().join("config");
    let runtime = scratch.path().join("runtime");
    for path in [&home, &config, &runtime] {
        std::fs::create_dir(path).unwrap();
    }
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &config);
    env.set("XDG_RUNTIME_DIR", &runtime);
    env.set("XDG_DATA_HOME", scratch.path().join("data"));
    env.set("XDG_CACHE_HOME", scratch.path().join("cache"));
    let root = AccountRoot::at(config.join("tilo"));
    let drive = FakeDrive::start();
    let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
    oauth.issue_access_token(SOURCE_TOKEN);
    let custodian = root
        .custodian()
        .with_provider(
            account_custody::OAuthCloud::GoogleDrive,
            oauth.provider(),
            "drive-client",
        )
        .with_storage_config(
            account_custody::OAuthCloud::GoogleDrive,
            serde_json::to_value(drive.config()).unwrap(),
        );
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(custodian.clone())
        .with_browser_opener(Arc::new(Browser));
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    // This is the same Settings form state SelectAccountServices changes,
    // not a hand-built account or a manually published handoff credential.
    pane.sign_in
        .as_ref()
        .unwrap()
        .select_services(true, false, FilesMode::Import)
        .unwrap();
    pane.command(AccountViewCommand::Submit);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    let store = root.store();
    assert_eq!(store.len(), 1);
    let account = store.all()[0].clone();
    assert_eq!(
        accounts::cloud_types::files_mode(&account),
        Some(FilesMode::Import)
    );
    assert!(!serde_json::to_string(&store).unwrap().contains("refresh-"));
    let view = pane.dir.views_of(AccountKind::GoogleDrive).remove(0);
    let spec = pane
        .files_handoff(&view)
        .expect("completed sign-in offers Files");
    let encoded = spec.build_args([]).unwrap();
    let handoff: ui::tilo_actions::remote::ApplicationLaunch =
        serde_json::from_slice(&encoded.payload).unwrap();
    let target = handoff.target("import-workspace");
    assert_eq!(
        target,
        ui::tilo_actions::remote::application_action_target(
            "import-workspace",
            "so.tilo.file-explorer",
            file_explorer::remote_accounts::resource_id(&account.id),
            file_explorer::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
        )
    );

    let ids = [
        drive.put_native(
            &drive.root_id(),
            "Essay",
            "application/vnd.google-apps.document",
        ),
        drive.put_native(
            &drive.root_id(),
            "Budget",
            "application/vnd.google-apps.spreadsheet",
        ),
        drive.put_native(
            &drive.root_id(),
            "Talk",
            "application/vnd.google-apps.presentation",
        ),
    ];
    let plain = drive.put_file(&drive.root_id(), "note.txt", b"cloud original");
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root.path().unwrap()),
            mounts_root: home.join("Remote"),
            state_root: scratch.path().join("state"),
            refresher: Arc::new(custodian),
            control_socket: None,
            file_socket: None,
        },
        Box::new(launcher.clone()),
    );
    service.reconcile();
    assert!(service.mounted().is_empty());
    assert!(launcher.volume(account.id.as_str()).is_none());
    service
        .serve_control(
            &runtime_fs::cloud::files_socket_in(&runtime),
            runtime_fs::cloud::CloudSocket::Files,
        )
        .unwrap();

    let (host, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&host);
    let mut app = TestApp::<FileExplorerShell>::launched(&[home.to_str().unwrap()]).unwrap();
    let account_row = file_explorer::remote_accounts::resource_id(&account.id);
    wait(&mut app, &mut service, |app| {
        app.actions().unwrap().iter().any(|a| {
            a.address.resource.as_str() == account_row
                && a.address.action == file_explorer::search_provider::REMOTE_ACCOUNT_OPEN_ACTION
        })
    });
    app.call_on(
        &target.address.resource,
        &target.address.action,
        serde_json::json!({}),
    )
    .unwrap();
    wait(&mut app, &mut service, |app| {
        app.actions()
            .unwrap()
            .iter()
            .any(|a| a.address.resource.as_str() == row(&ids[2]))
    });
    assert!(!home.join("Remote").exists());
    for id in &ids {
        select(&mut app, id);
    }
    select(&mut app, &plain);
    let destination = home.join("Chosen");
    std::fs::create_dir(&destination).unwrap();
    // Cancelling the portal must not create a queued history operation.
    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    app.call("import-cloud", serde_json::json!({})).unwrap();
    app.settle();
    assert_eq!(app.state().ops.entries().count(), 0);
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);

    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    app.call("import-cloud", serde_json::json!({})).unwrap();
    wait(&mut app, &mut service, completed);
    assert!(
        recorder
            .pick_requests()
            .iter()
            .all(ui::portal::PickRequest::is_writable_directory)
    );
    let exports = [
        ("Essay.docx", "word/document.xml", {
            mod fixture {
                include!("../../../../../crates/system/cloud-source-gdrive/tests/fixtures/docx.rs");
            }
            fixture::BYTES
        }),
        ("Budget.xlsx", "xl/workbook.xml", {
            mod fixture {
                include!("../../../../../crates/system/cloud-source-gdrive/tests/fixtures/xlsx.rs");
            }
            fixture::BYTES
        }),
        ("Talk.pptx", "ppt/presentation.xml", {
            mod fixture {
                include!("../../../../../crates/system/cloud-source-gdrive/tests/fixtures/pptx.rs");
            }
            fixture::BYTES
        }),
    ];
    for (name, member, expected) in exports {
        let bytes = std::fs::read(destination.join(name)).unwrap();
        assert_eq!(bytes, expected);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(
            archive.by_name(member).is_ok(),
            "{name} must be an Office export, not a provider link"
        );
    }
    let receipt = app.state();
    let (_, operation) = receipt.ops.entries().next().unwrap();
    let outcome = operation.settled.as_ref().unwrap();
    assert!(outcome.is_done());
    assert_eq!(outcome.result().len(), 4);
    for (id, name) in ids
        .iter()
        .zip(["Essay.docx", "Budget.xlsx", "Talk.pptx"])
        .chain(std::iter::once((&plain, "note.txt")))
    {
        use std::os::unix::fs::MetadataExt;
        let directory = std::fs::metadata(&destination).unwrap();
        let file = std::fs::metadata(destination.join(name)).unwrap();
        let directory_identity = serde_json::json!({
            "device": directory.dev(), "inode": directory.ino(), "modified": null,
        });
        let file_identity = serde_json::json!({
            "device": file.dev(), "inode": file.ino(),
            "modified": [file.mtime(), file.mtime_nsec(), file.len()],
        });
        assert!(
            outcome.result().iter().any(|change| matches!(change,
                file_explorer::file_ops::Change::CloudImported { from, to, destination: admitted, output }
                    if *from == format!("cloud:{}:{id}", account.id.as_str())
                        && std::path::Path::new(to) == destination.join(name)
                        && serde_json::to_value(admitted).unwrap() == directory_identity
                        && serde_json::to_value(output).unwrap() == file_identity
            )),
            "the outbox must settle with the actual independent output {name}"
        );
    }
    assert_eq!(progress(&mut app)["completed"], 4);
    let uploads = drive.stats().upload_puts;
    drive.write_file(&plain, b"later cloud edit");
    app.settle();
    assert_eq!(
        std::fs::read(destination.join("note.txt")).unwrap(),
        b"cloud original"
    );
    // Undo the untouched local outputs; remote edits do not change their identity.
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 4);
    assert!(app.undo());
    wait(&mut app, &mut service, completed);
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    for id in &ids {
        assert_eq!(drive.is_trashed(id), Some(false));
    }
    assert_eq!(drive.bytes(&plain).unwrap(), b"later cloud edit");

    // The selected revision was captured by Browse; changing it remotely
    // must surface a real daemon failure rather than create a stale copy.
    for id in &ids {
        select(&mut app, id);
    }
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    app.call("import-cloud", serde_json::json!({})).unwrap();
    wait(&mut app, &mut service, |app| {
        completed(app) && app.state().ops.entries().count() == 3
    });
    assert!(matches!(
        app.state()
            .ops
            .entries()
            .last()
            .unwrap()
            .1
            .settled
            .as_ref()
            .unwrap(),
        ui::tilo_resource::Settled::Failed { .. }
    ));
    assert!(!progress(&mut app)["failed"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    // Refresh the selected revision, then hold only the daemon's event pump
    // until the transfer reports preparation. Cancel is a published action;
    // the real service resumes to answer and close the outstanding read.
    app.call(
        "browse-cloud",
        serde_json::json!({"account": account.id.as_str()}),
    )
    .unwrap();
    wait(&mut app, &mut service, |app| {
        app.actions()
            .unwrap()
            .iter()
            .any(|a| a.address.resource.as_str() == row(&plain))
    });
    select(&mut app, &plain);
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    app.call("import-cloud", serde_json::json!({})).unwrap();
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    loop {
        app.settle();
        let snapshot = progress(&mut app);
        if snapshot["phase"] == "preparing" && snapshot["failed"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "transfer never reported preparation"
        );
        std::thread::yield_now();
    }
    app.call("cancel-cloud", serde_json::json!({})).unwrap();
    wait(&mut app, &mut service, |app| {
        completed(app) && app.state().ops.entries().count() == 4
    });
    let state = app.state();
    let cancelled = state
        .ops
        .entries()
        .last()
        .unwrap()
        .1
        .settled
        .as_ref()
        .unwrap();
    assert!(
        matches!(cancelled, ui::tilo_resource::Settled::Failed { reason, partial } if reason.contains("cancel") && partial.is_empty())
    );
    let visible = progress(&mut app).to_string();
    assert!(visible.to_lowercase().contains("cancel"), "{visible}");
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    assert_eq!(drive.bytes(&plain).unwrap(), b"later cloud edit");
    // Retry the cancelled plain-file selection, then edit only the independent copy.
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    app.call("import-cloud", serde_json::json!({})).unwrap();
    wait(&mut app, &mut service, |app| {
        completed(app) && app.state().ops.entries().count() == 5
    });
    let state = app.state();
    let retried = state
        .ops
        .entries()
        .last()
        .unwrap()
        .1
        .settled
        .as_ref()
        .unwrap();
    assert!(retried.is_done(), "{retried:?}");
    assert_eq!(retried.result().len(), 1);
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
    assert_eq!(
        std::fs::read(destination.join("note.txt")).unwrap(),
        b"later cloud edit"
    );
    std::fs::write(destination.join("note.txt"), b"independent local edit").unwrap();
    app.settle();
    assert_eq!(
        std::fs::read(destination.join("note.txt")).unwrap(),
        b"independent local edit"
    );
    assert_eq!(drive.bytes(&plain).unwrap(), b"later cloud edit");
    assert_eq!(drive.stats().upload_puts, uploads);
    app.call("reveal-cloud", serde_json::json!({})).unwrap();
    assert_eq!(
        app.with_surface(
            |files: &mut file_explorer::files_app::FileExplorerWidget| files.current_path()
        )
        .unwrap(),
        destination
    );
    service.shutdown();
}

#[test]
fn existing_sync_import_prepares_and_restores_on_cancel_or_failed_save() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let config = scratch.path().join("config");
    let runtime = scratch.path().join("runtime");
    for path in [&home, &config, &runtime] {
        std::fs::create_dir(path).unwrap();
    }
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &config);
    env.set("XDG_RUNTIME_DIR", &runtime);
    env.set("XDG_DATA_HOME", scratch.path().join("data"));
    env.set("XDG_CACHE_HOME", scratch.path().join("cache"));
    let root = AccountRoot::at(config.join("tilo"));
    let drive = FakeDrive::start();
    let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
    oauth.issue_access_token(SOURCE_TOKEN);
    let custodian = root
        .custodian()
        .with_provider(
            account_custody::OAuthCloud::GoogleDrive,
            oauth.provider(),
            "drive-client",
        )
        .with_storage_config(
            account_custody::OAuthCloud::GoogleDrive,
            serde_json::to_value(drive.config()).unwrap(),
        );
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(custodian.clone())
        .with_browser_opener(Arc::new(Browser));
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    // This is the same Settings form state SelectAccountServices changes,
    // not a hand-built account or a manually published handoff credential.
    pane.sign_in
        .as_ref()
        .unwrap()
        .select_services(true, false, FilesMode::Sync)
        .unwrap();
    pane.command(AccountViewCommand::Submit);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    let store = root.store();
    assert_eq!(store.len(), 1);
    let account = store.all()[0].clone();
    assert_eq!(
        accounts::cloud_types::files_mode(&account),
        Some(FilesMode::Sync)
    );
    assert!(!serde_json::to_string(&store).unwrap().contains("refresh-"));

    // The fake token endpoint omits scope metadata. Supply the approved
    // scopes so this fixture exercises the already-authorized mode-only path.
    let mut vault = root.vault();
    let mut secret = vault.get(&account.id).unwrap();
    let mut grant: serde_json::Value =
        serde_json::from_str(secret.token.as_deref().unwrap()).unwrap();
    grant["tokens"]["scope"] = "files offline".into();
    secret.token = Some(serde_json::to_string(&grant).unwrap());
    vault.put(&account.id, &secret);
    vault.try_save().unwrap();
    custodian.signed_in(&account.id).unwrap();
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root.path().unwrap()),
            mounts_root: home.join("Remote"),
            state_root: scratch.path().join("state"),
            refresher: Arc::new(custodian),
            control_socket: None,
            file_socket: None,
        },
        Box::new(launcher.clone()),
    );
    service.reconcile();
    assert_eq!(service.mounted().len(), 1);
    authorization_keeps_sync_writable(&mut pane, &root, &account, &launcher, &drive);
    service
        .serve_control(
            &runtime_fs::cloud::socket_in(&runtime),
            runtime_fs::cloud::CloudSocket::Accounts,
        )
        .unwrap();
    exercise_import_transitions(&mut pane, &root, &mut service, &account);
}

struct HoldingBrowser(std::sync::mpsc::SyncSender<String>);
impl BrowserOpener for HoldingBrowser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.0.send(url.to_owned()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ui::portal::LaunchOutcome::Opened).unwrap();
        rx
    }
}

fn authorization_keeps_sync_writable(
    pane: &mut AccountsPane,
    root: &AccountRoot,
    account: &accounts::Account,
    launcher: &InProcessLauncher,
    drive: &FakeDrive,
) {
    let mut vault = root.vault();
    let approved = vault.get(&account.id).unwrap();
    let mut needs_authorization = approved.clone();
    let mut grant: serde_json::Value =
        serde_json::from_str(needs_authorization.token.as_deref().unwrap()).unwrap();
    grant["tokens"]["scope"] = serde_json::Value::Null;
    needs_authorization.token = Some(serde_json::to_string(&grant).unwrap());
    vault.put(&account.id, &needs_authorization);
    vault.try_save().unwrap();
    let id = drive.put_file(&drive.root_id(), "during-authorization.txt", b"original");
    let volume = launcher.volume(account.id.as_str()).unwrap();
    volume.sync_once().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    pane.opener = Arc::new(HoldingBrowser(tx));
    pane.open_sign_in(AccountKind::GoogleDrive, Some(account));
    let flow = pane.sign_in.as_mut().unwrap();
    flow.select_services(true, false, FilesMode::Import)
        .unwrap();
    flow.submit();
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    loop {
        flow.poll();
        if rx.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Import froze Sync before opening authorization: {:?}",
            flow.message()
        );
        std::thread::yield_now();
    }
    // The browser can take arbitrarily longer than a preparation TTL. There
    // must not be any freeze to expire while it owns the authorization step.
    assert!(flow.busy());
    let ino = volume.resolve("during-authorization.txt").unwrap();
    let writer = volume.open_file(ino, true, false).unwrap();
    volume.write(writer, 0, b"retained").unwrap();
    volume.release(writer).unwrap();
    assert_eq!(volume.pending(), 1);
    pane.close_sign_in();
    volume.upload_pending().unwrap();
    assert_eq!(drive.bytes(&id).unwrap(), b"retained");
    vault.put(&account.id, &approved);
    vault.try_save().unwrap();
}

struct NoBrowser;
impl BrowserOpener for NoBrowser {
    fn open(&self, _: &str) -> ui::portal::LaunchChannel {
        panic!("a mode-only change must not request another login");
    }
}

fn exercise_import_transitions(
    pane: &mut AccountsPane,
    root: &AccountRoot,
    service: &mut CloudService,
    account: &accounts::Account,
) {
    pane.opener = Arc::new(NoBrowser);
    for outcome in [0, 1, 2] {
        pane.open_sign_in(AccountKind::GoogleDrive, Some(account));
        pane.sign_in
            .as_ref()
            .unwrap()
            .select_services(true, false, FilesMode::Import)
            .unwrap();
        pane.sign_in.as_mut().unwrap().submit();
        // Drive the real daemon while the Settings preparation worker waits.
        let deadline = Instant::now() + tilo_test_env::hang_bound();
        let completed = loop {
            service.pump();
            let (completed, _) = pane.sign_in.as_mut().unwrap().poll();
            if let Some(completed) = completed {
                break completed;
            }
            assert!(
                Instant::now() < deadline,
                "{:?}",
                pane.sign_in_message_for_test()
            );
            std::thread::yield_now();
        };
        assert_eq!(completed.config["files_mode"], "import");
        assert!(service.mounted().is_empty());
        assert!(
            runtime_fs::cloud::import_writer_lock(&root.path().unwrap(), account.id.as_str())
                .is_err(),
            "Settings must fence the pinned metadata commit until completion or abort"
        );
        if outcome == 2 {
            let id = AccountsPane::save_signed_in(root, &completed).unwrap();
            pane.sign_in.as_mut().unwrap().finish_async(id);
            while pane.sign_in.is_some() {
                service.pump();
                pane.poll_sign_in();
                assert!(
                    Instant::now() < deadline,
                    "{:?}",
                    pane.sign_in_message_for_test()
                );
                std::thread::yield_now();
            }
            service.reconcile();
            assert!(service.mounted().is_empty());
            assert_eq!(
                accounts::cloud_types::files_mode(root.store().get(&account.id).unwrap()),
                Some(FilesMode::Import)
            );
            break;
        }
        if outcome == 1 {
            pane.sign_in
                .as_mut()
                .unwrap()
                .save_failed("metadata write refused");
        } else {
            pane.close_sign_in();
        }
        // Abort is deliberately off the UI thread. The daemon can remount
        // before Settings reads the abort response and drops its writer fence.
        // Probe both conditions, in the same order as Settings preparation, so
        // the next form cannot race the previous abort worker's lock release.
        let (tx, rx) = std::sync::mpsc::channel();
        let socket = runtime_fs::cloud::default_socket().unwrap();
        let config_root = root.path().unwrap();
        let id = account.id.to_string();
        std::thread::spawn(move || {
            let mut client = runtime_fs::cloud::CloudClient::connect(socket).unwrap();
            loop {
                if let Ok(writer) = runtime_fs::cloud::import_writer_lock(&config_root, &id)
                    && let Ok(token) = client.prepare_import(&id)
                {
                    client.abort_import(&id, &token).unwrap();
                    drop(writer);
                    tx.send(()).unwrap();
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
        });
        loop {
            service.pump();
            if rx.try_recv().is_ok() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            accounts::cloud_types::files_mode(root.store().get(&account.id).unwrap()),
            Some(FilesMode::Sync)
        );
    }
}
