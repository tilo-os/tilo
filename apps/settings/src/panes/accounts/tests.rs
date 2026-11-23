use super::*;
use crate::accounts::sample_directory;
use crate::effects::page_of;
use crate::resource::SettingsShell;
use crate::testing::actions_of;
use tilo_test_env::SpawnUnderEnvLock as _;
use ui::framework::testing::TestApp;

struct RejectCandidate;

struct AcceptSavedCredential;

impl crate::account_probe::AccountTester for AcceptSavedCredential {
    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        if account.id.as_str() != "connection@example.test"
            || secret.password != "test-only-password"
        {
            return Err("the probe did not receive the saved account credential".into());
        }
        Ok("The server accepted the saved sign-in".into())
    }
}

#[test]
fn account_check_returns_its_saved_credential_probe_result() -> Result<(), String> {
    let (_dir, root) = scratch_root();
    let id = accounts::AccountId::from("connection@example.test");
    let account = accounts::Account::new(id.clone(), "email");
    let mut store = accounts::person::AccountList::default();
    store.keep(&account).unwrap();
    root.save_store(&store).map_err(|error| error.to_string())?;
    let mut vault = root.vault();
    vault.put(&id, &accounts::Secret::password("test-only-password"));
    vault.try_save()?;
    let pane = AccountsPane::in_root(root.clone())
        .with_account_tester(std::sync::Arc::new(AcceptSavedCredential));
    let check = pane.connection_check(id.as_str())?;
    let Effected::Done(output) = complete_account_check(check())? else {
        return Err("the finished account check remained pending".into());
    };
    let effect = AccountsEffect::TestAccount {
        account: id.as_str().into(),
    };
    let spec = effect.bound();
    spec.contract.result.validate(&output)?;
    if output.value != Some(serde_json::json!("The server accepted the saved sign-in"))
        || !output.resources.is_empty()
    {
        return Err("the account check must return its exact status without resources".into());
    }
    if root.vault().get(&id) != vault.get(&id) {
        return Err("checking the account must preserve its saved credential".into());
    }
    let rejecting =
        AccountsPane::in_root(root).with_account_tester(std::sync::Arc::new(RejectCandidate));
    let check = rejecting.connection_check(id.as_str())?;
    if complete_account_check(check()) != Err("server rejected candidate".into()) {
        return Err("the account check must propagate the probe's rejection".into());
    }
    Ok(())
}

impl crate::account_probe::AccountTester for RejectCandidate {
    fn test(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<String, String> {
        Err("server rejected candidate".into())
    }
}

/// A drive that `tilo migrate connect` made, in the workspace's accounts record: it shows in
/// Settings with its sign-in-again and remove actions, whether or not it is mounted.
#[test]
fn a_drive_connected_elsewhere_shows_with_its_sign_in_and_remove_actions() {
    let (_dir, root) = scratch_root();
    let mut account = accounts::Account::new("drive-1", accounts::cloud_types::GOOGLE_DRIVE);
    account.display_name = "Drive".into();
    account.config = serde_json::json!({
        "provider": "google-drive",
        "files_mode": "import",
    });
    let mut store = accounts::person::AccountList::default();
    store.keep(&account).unwrap();
    root.save_store(&store).unwrap();
    let pane = AccountsPane::in_root(root);
    let mut app = window_showing(pane);
    let published: Vec<(String, Vec<String>, String)> = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings)
                .unwrap()
                .rows()
                .into_iter()
                .map(|published| {
                    let id = published.id().to_string();
                    let resource = published.into_resource();
                    let label = resource.label.clone();
                    let actions = resource
                        .actions
                        .into_iter()
                        .map(|spec| spec.id.clone())
                        .collect();
                    (id, actions, label)
                })
                .collect()
        })
        .unwrap();
    let (_, actions, label) = published
        .iter()
        .find(|(id, _, _)| id == "settings.account:drive-1")
        .expect("the drive's row is published");
    assert_eq!(label, "Drive");
    for wanted in ["sign-in-again", "remove-account"] {
        assert!(
            actions.iter().any(|action| action == wanted),
            "{wanted} is offered: {actions:?}"
        );
    }
    assert!(
        actions.iter().all(|action| action != "sync-now"),
        "an import-only drive has no mount to sync: {actions:?}"
    );
}

/// A private config root: the stores, vault and mutation journal of one
/// test never meet another's, and nothing reaches the user's own.
fn scratch_root() -> (tempfile::TempDir, AccountRoot) {
    let dir = tempfile::tempdir().expect("scratch config root");
    let root = AccountRoot::scratch_at(dir.path().join("tilo"));
    (dir, root)
}

tilo_test_env::scenario! {
    fn the_accounts_model_keeps_store_vault_and_journal_in_step;

    phase selected_browser_sessions_use_device_inventory_not_public_identity_resources;
    {
        let (_scratch, root) = scratch_root();
        let mut store = accounts::person::AccountList::default();
        store.keep(&accounts::Account::new("selected", browser_sessions::TYPE)).unwrap();
        store.keep(&accounts::Account::new("other", "email")).unwrap();
        root.save_store(&store).unwrap();
        let pane = AccountsPane::from_dir_in(root.clone(), AccountDirectory::default(), false);
        let selections = pane.browser_service_sessions();
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].id.as_str(), "selected");
        assert!(
            AccountsPane::from_dir_in(root, AccountDirectory::default(), true)
                .browser_service_sessions()
                .is_empty()
        );
    }

    phase account_edits_preserve_config_and_probe_before_persisting;
    {
        {
            let (_dir, root) = scratch_root();
            let id = accounts::AccountId::from("nextcloud-stable-id");
            let mut original = accounts::Account::new(id.clone(), crate::accounts::NEXTCLOUD_TYPE);
            original.display_name = "Old label".into();
            original.enabled = false;
            original.tint = 7;
            original.config = serde_json::json!({
                "id": "nextcloud-stable-id",
                "label": "Old label",
                "server_url": "https://old.example",
                "username": "old-user",
                "use_files": false,
                "use_calendar": true,
                "use_mail": false,
                "future_provider_field": {"kept": true}
            });
            let mut store = accounts::person::AccountList::default();
            store.keep(&original).unwrap();
            root.save_store(&store).expect("seed canonical account");
            let mut vault = root.vault();
            vault.put(
                &id,
                &accounts::Secret {
                    password: "old-password".into(),
                    token: Some("future-provider-token".into()),
                },
            );
            vault.try_save().expect("seed credential");

            AccountsPane::save_account(
                &root,
                AccountKind::Nextcloud,
                Some(id.as_str()),
                "New label",
                "new-user",
                "",
                "https://old.example/",
                "",
            )
            .expect("edit account");

            let saved = root.store().get(&id).expect("saved account");
            assert!(!saved.enabled);
            assert_eq!(saved.tint, 7);
            assert_eq!(saved.config["use_files"], false);
            assert_eq!(saved.config["use_calendar"], true);
            assert_eq!(saved.config["use_mail"], false);
            assert_eq!(saved.config["future_provider_field"]["kept"], true);
            assert_eq!(saved.config["server_url"], "https://old.example");
            assert_eq!(saved.config["username"], "new-user");
            assert_eq!(
                root.vault().get(&id),
                Some(accounts::Secret {
                    password: "old-password".into(),
                    token: Some("future-provider-token".into()),
                })
            );
        }

        {
            let (_dir, root) = scratch_root();
            let id = accounts::AccountId::from("reauth@example.test");
            let mut account = accounts::Account::new(id.clone(), "email");
            account.display_name = "Reauth".into();
            account.config = serde_json::json!({
                "email": id.as_str(), "username": id.as_str(),
                "imap_host": "imap.example.test", "imap_port": 993
            });
            let mut store = accounts::person::AccountList::default();
            store.keep(&account).unwrap();
            root.save_store(&store).expect("seed account");
            let mut vault = root.vault();
            vault.put(&id, &accounts::Secret::password("old-password"));
            vault.try_save().expect("seed credential");
            let mut pane = AccountsPane::in_root(root.clone())
                .with_account_tester(std::sync::Arc::new(RejectCandidate));
            pane.command(AccountViewCommand::Reauthenticate(id.to_string()));
            pane.set_reauth_password_for_test("candidate-password");
            pane.command(AccountViewCommand::SubmitReauthentication);
            settle_probes(&mut pane);
            assert_eq!(
                pane.reauth_error_for_test(),
                Some("server rejected candidate")
            );
            assert_eq!(
                root.vault().get(&id).map(|secret| secret.password),
                Some("old-password".into()),
                "rejected candidate must never replace the stored credential"
            );
        }
    }

    phase the_area_builds_its_model_from_the_read_it_started;
    {
        let (dir, root) = scratch_root();
        let id = accounts::AccountId::from("warm@example.test");
        let mut store = accounts::person::AccountList::default();
        store.keep(&accounts::Account::new(id.clone(), "email")).unwrap();
        root.save_store(&store).expect("the store is saved");
        let mut vault = root.vault();
        vault.put(&id, &accounts::Secret::password("test-only-password"));
        vault.try_save().expect("the vault is saved");
        let cx = crate::area::AreaContext::in_profile(dir.path().to_path_buf());
        let area = AccountsArea::new(&cx).with_account_root(root.clone());
        assert!(area.start_read(), "the first read starts");
        assert!(!area.start_read(), "one read at a time");
        assert!(!area.is_open(), "a running read publishes nothing yet");
        until_finished(|| area.read.is_finished());
        let model = area.model().borrow();
        assert!(area.is_open());
        assert!(vault.get(&id).is_some(), "the credential was saved");
        assert_eq!(
            model.vault.as_ref().and_then(|held| held.get(&id)),
            vault.get(&id),
            "the vault read is held"
        );
        assert_eq!(
            model.directory(),
            AccountsPane::in_root(root).directory(),
            "the directory is the one reading in place shows"
        );
        drop(model);
        assert!(!area.start_read(), "a built model is not read again");
    }

    phase a_pending_mutation_is_seen;
    {
        let (_dir, root) = scratch_root();
        assert!(!root.mutation_pending());
        let config = root.path().expect("an explicit root");
        let accounts_dir = accounts::accounts_dir_in(&config);
        std::fs::create_dir_all(&accounts_dir).expect("the accounts directory");
        std::fs::write(accounts_dir.join("mutation-journal.json"), b"{}").expect("an intent");
        assert!(root.mutation_pending());
    }
}

/// Drive the host's polling until the pending account probe reports,
/// however long the probe thread takes to be scheduled.
/// A credential is entered for the server its account names then. An edit
/// that names another server and enters none is refused, in words that ask
/// for the one of that server: the kept one is sent nowhere else.
#[test]
fn an_edit_that_names_another_server_needs_its_credential_entered() {
    let (_scratch, root) = scratch_root();
    let save = |existing: Option<&str>, name: &str, server: &str, password: &str| {
        AccountsPane::save_account(
            &root,
            AccountKind::CalDav,
            existing,
            name,
            "me@example.test",
            "",
            server,
            password,
        )
    };
    save(
        None,
        "Work",
        "https://dav.example.test/dav/",
        "entered-for-dav-example",
    )
    .expect("add account");
    let id = root.store().all()[0].id.clone();
    let named = |root: &AccountRoot| {
        root.store().get(&id).expect("the account is kept").config["server_url"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let kept = named(&root);

    save(
        Some(id.as_str()),
        "Work calendar",
        "https://dav.example.test/dav/",
        "",
    )
    .expect("an edit that keeps its server keeps its credential");
    let refused = save(
        Some(id.as_str()),
        "Work calendar",
        "https://dav.elsewhere.example/dav/",
        "",
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("another server")),
        "an edit to another server took the credential entered for {kept} with it: {refused:?}"
    );
    assert_eq!(named(&root), kept, "a refused edit changes nothing");

    save(
        Some(id.as_str()),
        "Work calendar",
        "https://dav.elsewhere.example/dav/",
        "entered-for-elsewhere",
    )
    .expect("with its own credential entered the other server is saved");
    assert_ne!(named(&root), kept);
    assert_eq!(
        root.vault().get(&id).map(|secret| secret.password),
        Some("entered-for-elsewhere".to_owned())
    );
}

fn settle_probes(pane: &mut AccountsPane) {
    let deadline = ui::time::now() + ProbeJob::<String>::DEADLINE;
    loop {
        let _ = pane.poll();
        if pane.form_probe.is_none()
            && pane.reauth_probe.is_none()
            && !pane.form_saving
            && !pane.reauth_saving
            && pane.queue.is_idle()
        {
            return;
        }
        assert!(ui::time::now() < deadline, "account probe never reported");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn access_nodes(w: &dyn Widget, nodes: &mut Vec<ui::a11y::AccessNode>) {
    if let Some(node) = w.accessibility() {
        nodes.push(node);
    }
    for child in w.children() {
        access_nodes(child, nodes);
    }
}

pub(super) fn window_showing(pane: AccountsPane) -> TestApp<SettingsShell> {
    crate::area::replace_in_next_window(move |context| AccountsArea::holding(context, pane));
    let mut app = TestApp::<SettingsShell>::new().expect("Settings opens headlessly");
    crate::effects::show_pane(&mut app, crate::categories::Category::ExternalAccounts);
    app.settle();
    app
}

pub(super) fn on_page<T>(pane: AccountsPane, read: impl FnOnce(&dyn Widget) -> T) -> T {
    let mut app = window_showing(pane);
    read(page_of(app.harness().root_widget()))
}

pub(super) fn change<T>(
    app: &mut TestApp<SettingsShell>,
    change: impl FnOnce(&mut AccountsPane) -> T,
) -> T {
    let changed = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            let mut pane = area(settings)
                .expect("the window holds the area")
                .model()
                .borrow_mut();
            let changed = change(&mut pane);
            pane.changed = true;
            changed
        })
        .expect("the Settings window is mounted");
    app.settle();
    changed
}

fn page_actions(pane: AccountsPane) -> Vec<String> {
    on_page(pane, |page| {
        let mut actions = Vec::new();
        actions_of(page, &mut actions);
        actions
    })
}

pub(super) fn announced(app: &mut TestApp<SettingsShell>) -> Vec<String> {
    provider_settings_ui::widgets::announced_text(page_of(app.harness().root_widget()))
}

#[test]
fn adding_an_account_opens_its_form_where_add_account_was() {
    let (_scratch, root) = scratch_root();
    let mut app = window_showing(AccountsPane::from_dir_in(root, sample_directory(), true));
    let before = announced(&mut app);
    let add = before
        .iter()
        .position(|line| *line == t("accounts-add"))
        .expect("Add account is on the page");
    change(&mut app, |pane| {
        pane.form_kind = Some(AccountKind::Nextcloud);
    });
    let after = announced(&mut app);
    let title = t_args(
        "accounts-form-add",
        &[("kind", kind_shown(AccountKind::Nextcloud).into())],
    );
    let form = after
        .iter()
        .position(|line| *line == title)
        .expect("the form is on the page");
    assert_eq!(
        form, add,
        "the form opens in Add account's place, not at the top: {after:#?}"
    );
    assert!(
        !after.iter().any(|line| *line == t("accounts-add")),
        "the chooser gives way to the form it opened: {after:#?}"
    );
}

#[test]
fn accounts_pane_renders_only_real_accounts_as_removable() {
    {
        let (_scratch, root) = scratch_root();
        let pane = AccountsPane::from_dir_in(root, sample_directory(), true);
        let labels = page_actions(pane);
        assert_eq!(
            labels.len(),
            AccountKind::ALL.len() + 1,
            "every provider kind can be added and Migrations opened over the sample: {labels:?}"
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| AccountKind::ALL.into_iter().any(|kind| **label
                    == t_args(
                        "accounts-add-provider",
                        &[("kind", kind_shown(kind).into())]
                    )))
                .count(),
            AccountKind::ALL.len(),
            "one Add action per provider kind"
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| **label == t("accounts-migrate-open"))
                .count(),
            1,
            "one action opens Migrations"
        );
        assert!(
            labels.iter().all(|label| *label != t("accounts-remove")),
            "sample accounts cannot be removed"
        );
    }

    {
        // Without stored passwords, real rows offer removal but do not invent
        // a reauthentication action.
        let dir = sample_directory(); // 2 mail + 1 cal + 1 nextcloud = 4 accounts
        let (_scratch, root) = scratch_root();
        let pane = AccountsPane::from_dir_in(root, dir, false);
        let actions = page_actions(pane);
        assert_eq!(
            actions.len(),
            AccountKind::ALL.len() + 1 + 16 + 1,
            "{actions:?}"
        );
    }

    {
        // Real accounts get an explicit Remove action rather than a lone
        // trash glyph. The confirmation flow still owns the mutation; this
        // assertion guards the decision point users see before it.
        let (_scratch, root) = scratch_root();
        let pane = AccountsPane::from_dir_in(root, sample_directory(), false);
        let labels = page_actions(pane);
        assert_eq!(
            labels
                .iter()
                .filter(|label| [
                    "accounts-remove",
                    "accounts-remove-files",
                    "accounts-disconnect-mail"
                ]
                .into_iter()
                .any(|key| **label == t(key)))
                .count(),
            4,
            "one explicit Remove action per real account"
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| AccountKind::ALL.into_iter().any(|kind| **label
                    == t_args(
                        "accounts-add-provider",
                        &[("kind", kind_shown(kind).into())]
                    )))
                .count(),
            AccountKind::ALL.len(),
            "the single chooser retains every provider's add action"
        );
    }

    {
        let (_dir, root) = scratch_root();
        let pane = AccountsPane::in_root(root);
        assert!(!pane.is_sample(), "the live pane never flags the sample");
        assert!(
            pane.directory().is_empty(),
            "an empty store renders empty — no fabricated ada@example.com rows"
        );
        for kind in AccountKind::ALL {
            assert!(
                pane.directory().views_of(kind).is_empty(),
                "{kind:?} must be empty"
            );
        }
    }
}

#[test]
fn removal_asks_first_and_checks_run_as_jobs() {
    let declarations = AccountsEffect::declarations();
    let declared = |id: &str| {
        declarations
            .iter()
            .find(|declaration| declaration.id == id)
            .unwrap()
    };
    assert_eq!(
        declared(AccountsEffect::REMOVE_ACCOUNT.id).risk,
        ui::tilo_actions::RiskLevel::Destructive
    );
    assert!(matches!(
        declared(AccountsEffect::TEST_ACCOUNT.id).answers,
        ui::tilo_resource::effect::Answers::Later { .. }
    ));
}

#[test]
fn account_form_drafts_are_local_accessible_and_validated() {
    {
        let mut pane = AccountsPane::from_dir(sample_directory(), true);
        assert!(!pane.form_open());
        pane.command(AccountViewCommand::Add(AccountKind::Mail));
        assert!(pane.form_open(), "Add opens the form");
        assert!(pane.poll(), "opening the form requests a rebuild");
        pane.command(AccountViewCommand::Cancel);
        assert!(!pane.form_open(), "Cancel closes the form");
    }

    {
        let mut pane = AccountsPane::from_dir(sample_directory(), true);
        pane.command(AccountViewCommand::Add(AccountKind::WebDav));
        pane.set_form_fields_for_test("Home files", "", "ada", "https://files.example.test");
        pane.set_form_password_for_test("never-collaborate-this");
        let _ = pane.poll();
        let nodes = on_page(pane, |page| {
            let mut nodes = Vec::new();
            access_nodes(page, &mut nodes);
            nodes
        });
        for name in [
            "Display name",
            "Username",
            "Server URL",
            "Password or app password",
        ] {
            assert!(
                nodes.iter().any(|node| {
                    node.role == ui::a11y::Role::TextInput && node.name.as_deref() == Some(name)
                }),
                "missing named text input: {name}"
            );
        }
        let password = nodes
            .iter()
            .find(|node| {
                node.role == ui::a11y::Role::TextInput
                    && node.name.as_deref() == Some("Password or app password")
            })
            .expect("password field semantics");
        assert!(password.state.protected);
        assert_eq!(
            password.value, None,
            "screen readers must not receive secrets"
        );
    }

    {
        // Guard: saving a blank address must not create an account (and must
        // not touch disk). We assert the pure guard directly.
        let (_dir, root) = scratch_root();
        assert!(
            AccountsPane::save_account(&root, AccountKind::Mail, None, "Nobody", "  ", "", "", "")
                .is_err()
        );
        assert!(root.store().is_empty());
    }

    {
        let (_dir, root) = scratch_root();
        let mut pane = AccountsPane::in_root(root.clone())
            .with_account_tester(std::sync::Arc::new(RejectCandidate));
        pane.command(AccountViewCommand::Add(AccountKind::Mail));
        pane.set_form_fields_for_test(
            "Rejected",
            "rejected@example.test",
            "rejected@example.test",
            "",
        );
        pane.set_form_password_for_test("candidate-password");
        pane.command(AccountViewCommand::Submit);
        settle_probes(&mut pane);
        assert_eq!(
            pane.form_error.as_deref(),
            Some("server rejected candidate")
        );
        assert!(pane.form_open(), "a rejected draft remains editable");
        assert!(root.store().is_empty());
        assert!(
            root.vault()
                .get(&accounts::AccountId::from("rejected@example.test"))
                .is_none(),
            "a rejected credential never reaches the vault"
        );
    }
}

#[test]
fn vault_save_failure_preserves_existing_account_metadata() {
    const CHILD: &str = "TILO_SETTINGS_VAULT_FAILURE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "panes::accounts::tests::vault_save_failure_preserves_existing_account_metadata",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PATH", "")
            .status_locked()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (_dir, account_root) = scratch_root();
    let root = account_root.path().unwrap();
    let old_id = accounts::AccountId::from("existing@example.test");
    let mut store = accounts::person::AccountList::default();
    store
        .keep(&accounts::Account::new(old_id.clone(), "email"))
        .unwrap();
    account_root.save_store(&store).unwrap();
    let mut vault = accounts::SecretVault::open_in(&root);
    vault.put(&old_id, &accounts::Secret::password("old-password"));
    vault.try_save().unwrap();
    assert_eq!(
        accounts::SecretVault::open_in(&root).backend(),
        accounts::VaultBackend::LocalFile
    );
    let dir = accounts::vault_dir_in(&root);
    let ciphertext_before = std::fs::read(dir.join("vault.enc")).unwrap();
    let malformed_key = [7u8; 31];
    std::fs::write(dir.join("vault.key"), malformed_key).unwrap();

    let error = AccountsPane::save_account(
        &account_root,
        AccountKind::Mail,
        None,
        "New account",
        "new@example.test",
        "new@example.test",
        "",
        "new-password",
    )
    .unwrap_err();
    assert!(
        error.starts_with("could not save credential:"),
        "must reach vault save, not fail account intent: {error}"
    );
    assert_eq!(std::fs::read(dir.join("vault.key")).unwrap(), malformed_key);
    assert_eq!(
        std::fs::read(dir.join("vault.enc")).unwrap(),
        ciphertext_before
    );
    let saved = account_root.store();
    assert_eq!(saved.all().len(), 1);
    assert!(saved.get(&old_id).is_some());
    assert!(
        saved
            .get(&accounts::AccountId::from("new@example.test"))
            .is_none()
    );
}

#[test]
fn account_actions_include_sync_now_for_mounted_accounts() {
    use runtime_fs::cloud::{CloudCapabilities, CloudMount, MountHealth};
    let mount = |account: &str, paused: bool| CloudMount {
        account: account.into(),
        mount_point: std::path::PathBuf::from("/home/ada/Remote").join(account),
        health: Some(MountHealth::Healthy),
        paused,
        capabilities: CloudCapabilities::default(),
        signed_in: false,
    };
    for paused in [false, true] {
        let (_scratch, root) = scratch_root();
        let pane = AccountsPane::from_dir_in(root, sample_directory(), false);
        let files = pane.directory().views_of(AccountKind::Nextcloud)[0]
            .id
            .clone();
        let mail = pane.directory().views_of(AccountKind::Mail)[0].id.clone();
        let pane = pane.with_cloud_mounts(CloudMounts::known(vec![mount(&files, paused)]));
        let mut app = window_showing(pane);
        let mut labels = Vec::new();
        actions_of(page_of(app.harness().root_widget()), &mut labels);
        let published: Vec<(String, Vec<String>)> = app
            .with_widget::<crate::window::Inspected, _>(|settings| {
                area(settings)
                    .unwrap()
                    .rows()
                    .into_iter()
                    .map(|published| {
                        let id = published.id().to_string();
                        let actions = published
                            .into_resource()
                            .actions
                            .into_iter()
                            .map(|spec| spec.id.clone())
                            .collect();
                        (id, actions)
                    })
                    .collect()
            })
            .unwrap();
        let actions = |account: &str| -> Vec<String> {
            let row = format!("settings.account:{account}");
            published
                .iter()
                .find(|(id, _)| *id == row)
                .map(|(_, actions)| actions.clone())
                .unwrap_or_default()
        };
        let mounted = actions(&files);
        assert!(mounted.contains(&"sync-now".to_owned()), "{mounted:?}");
        let (offered, withheld) = if paused {
            ("resume-sync", "pause-sync")
        } else {
            ("pause-sync", "resume-sync")
        };
        assert!(mounted.contains(&offered.to_owned()), "{mounted:?}");
        assert!(!mounted.contains(&withheld.to_owned()), "{mounted:?}");
        let unmounted = actions(&mail);
        assert!(
            unmounted.contains(&"test-account".to_owned()),
            "{unmounted:?}"
        );
        assert!(
            unmounted
                .iter()
                .all(|id| !matches!(id.as_str(), "sync-now" | "pause-sync" | "resume-sync")),
            "an account the service has not mounted offers no sync control: {unmounted:?}"
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.as_str() == "Sync now")
                .count(),
            1,
            "{labels:?}"
        );
        let toggle = if paused { "Resume sync" } else { "Pause sync" };
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.as_str() == toggle)
                .count(),
            1,
            "{labels:?}"
        );
    }
}

/// An account whose mode change was interrupted says so and offers its recovery, through the
/// action the row publishes.
#[test]
fn an_interrupted_mode_change_offers_its_recovery_on_the_row() {
    let (_dir, root) = scratch_root();
    let mut account = accounts::Account::new("drive-1", accounts::cloud_types::GOOGLE_DRIVE);
    account.display_name = "Drive".into();
    account.config = serde_json::json!({
        "provider": "google-drive",
        "files_mode": "sync",
    });
    let mut store = accounts::person::AccountList::default();
    store.keep(&account).unwrap();
    root.save_store(&store).unwrap();
    root.sync_reporter(account.id.clone())
        .failed()
        .expect("the interrupted change is recorded");
    let pane = AccountsPane::in_root(root).with_cloud_mounts(CloudMounts::known(Vec::new()));
    let mut app = window_showing(pane);
    let published = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings)
                .unwrap()
                .rows()
                .into_iter()
                .map(|published| {
                    let id = published.id().to_string();
                    let actions: Vec<String> = published
                        .into_resource()
                        .actions
                        .into_iter()
                        .map(|spec| spec.id.clone())
                        .collect();
                    (id, actions)
                })
                .collect::<Vec<_>>()
        })
        .unwrap();
    let actions = published
        .iter()
        .find(|(id, _)| id == "settings.account:drive-1")
        .map(|(_, actions)| actions.clone())
        .expect("the drive's row is published");
    assert!(
        actions.iter().any(|action| action == "recover-cloud-mode"),
        "recovering the interrupted change is offered: {actions:?}"
    );
}

fn until_finished(finished: impl Fn() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !finished() {
        assert!(std::time::Instant::now() < deadline, "the read ends");
        std::thread::yield_now();
    }
}

#[test]
fn a_stalled_vault_never_holds_the_window() {
    use std::sync::Arc;
    use std::time::Instant;

    let dir = tempfile::tempdir().expect("scratch config root");
    let config = dir.path().join("tilo");
    let id = accounts::AccountId::from("stalled@example.test");
    let hold = crate::stall::Stall::new();
    let window = {
        let (id, config) = (id.clone(), config.clone());
        std::thread::spawn(move || {
            // The workspace's accounts record is the window's: it is opened,
            // seeded and read on the window's thread.
            let root = AccountRoot::scratch_at(config);
            let mut store = accounts::person::AccountList::default();
            store
                .keep(&accounts::Account::new(id.clone(), "email"))
                .unwrap();
            root.save_store(&store).expect("the store is saved");
            let mut vault = root.vault();
            vault.put(&id, &accounts::Secret::password("test-only-password"));
            vault.try_save().expect("the vault is saved");
            let read = AccountsRead::of(&root);
            ui::rt::mark_ui_thread();
            let stalled = Arc::clone(&hold);
            let root = root.with_vault_hold(move || {
                stalled.wait();
            });
            let dir = AccountDirectory::load_in(&root);
            let mut pane = AccountsPane::from_read(root.clone(), dir, read);

            let removed = Rc::new(std::cell::RefCell::new(None));
            let landed = Rc::clone(&removed);
            pane.remove_then(id.as_str(), move |result| {
                *landed.borrow_mut() = Some(result);
            });
            let check = pane.connection_check(id.as_str());
            let _ = pane.poll();
            // The vault answers nothing until the hold is released below:
            // clicks answered while it is held did not wait for it.
            hold.assert_held("the clicks");
            assert!(check.is_ok(), "the check reads the vault on its worker");
            assert!(
                removed.borrow().is_none(),
                "the removal waits for the vault"
            );
            assert!(pane.busy(), "the pane keeps polling while the vault is out");
            assert!(
                root.store().get(&id).is_some(),
                "the account is kept until the vault took its removal"
            );

            hold.release();
            let deadline = tilo_test_env::hang_deadline();
            while removed.borrow().is_none() || !pane.queue.is_idle() {
                assert!(Instant::now() < deadline, "the removal lands");
                let _ = pane.poll();
                std::thread::yield_now();
            }
            assert_eq!(removed.take(), Some(Ok(())));
            assert!(root.store().get(&id).is_none(), "the metadata is gone");
        })
    };
    if let Err(panic) = window.join() {
        std::panic::resume_unwind(panic);
    }
    let root = AccountRoot::at(config);
    assert!(
        root.vault().get(&id).is_none(),
        "the credential went with the account"
    );
    assert!(!root.mutation_pending(), "the removal's journal is cleared");
}

#[test]
fn a_stalled_vault_never_holds_the_mail_grant_status() {
    use std::time::Instant;

    let (_dir, root) = scratch_root();
    let id = accounts::AccountId::from("granted@example.test");
    let mut account = accounts::Account::new(id.clone(), "email");
    account.config = serde_json::to_value(accounts::email::AccountConfig {
        email: "granted@example.test".into(),
        imap_host: "imap.example.test".into(),
        ..accounts::email::AccountConfig::default()
    })
    .expect("the config serialises");
    let mut store = accounts::person::AccountList::default();
    store.keep(&account).unwrap();
    root.save_store(&store).expect("the store is saved");
    let dir = AccountDirectory::load_in(&root);
    assert_eq!(
        dir.of(AccountKind::Mail).count(),
        1,
        "the pane lists a Mail account"
    );
    root.credential_store()
        .seal_oauth(
            &id,
            &account_credentials::TokenSet {
                access_token: "test-only-access".into(),
                scope: None,
                refresh_token: "test-only-refresh".into(),
                expires_in: 3600,
                obtained_at: 0,
            },
        )
        .expect("the grant is sealed");
    let read = AccountsRead::of(&root);
    let hold = crate::stall::Stall::new();
    // A window that waited on the vault would sit until the stall gives up,
    // which `assert_held` reports.
    let stalled = std::sync::Arc::clone(&hold);
    let root = root.with_vault_hold(move || {
        stalled.wait();
    });
    let dir = AccountDirectory::load_in(&root);
    let pane = AccountsPane::from_read(root, dir, read);
    let granted = t("accounts-status-mail-granted");
    let shows_granted = |app: &mut TestApp<SettingsShell>| {
        announced(app).iter().any(|line| line.contains(&granted))
    };

    let mut app = window_showing(pane);
    let shown = shows_granted(&mut app);
    hold.assert_held("composing the accounts");
    assert!(!shown, "the grant is not known before the vault answers");
    assert!(
        app.with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings).unwrap().model().borrow().busy()
        })
        .unwrap(),
        "the pane keeps polling while the vault is out"
    );

    hold.release();
    let deadline = tilo_test_env::hang_deadline();
    while !shows_granted(&mut app) {
        assert!(Instant::now() < deadline, "the grant is shown");
        app.settle();
        std::thread::yield_now();
    }
}

struct Bridged(std::cell::RefCell<ui::linked_accounts::LinkState>);

fn ada() -> ui::linked_accounts::AccountName {
    ui::linked_accounts::AccountName {
        user_name: "ada.l".into(),
        display_name: Some("Ada L".into()),
    }
}

impl ui::linked_accounts::LinkedAccountsService for Bridged {
    fn accounts(&self) -> Vec<ui::linked_accounts::LinkedAccount> {
        vec![ui::linked_accounts::LinkedAccount {
            app: "so.tilo.discord".into(),
            service: "Discord".into(),
            state: self.0.borrow().clone(),
            problem: None,
        }]
    }

    fn revision(&self) -> u64 {
        0
    }

    fn refresh(&self) {}

    fn link(&self, _app: &str) {
        *self.0.borrow_mut() = ui::linked_accounts::LinkState::Connecting;
    }

    fn unlink(&self, _app: &str) {
        *self.0.borrow_mut() = ui::linked_accounts::LinkState::Unlinked {
            connect: true,
            elsewhere: None,
        };
    }

    fn accept(&self, _app: &str) {
        *self.0.borrow_mut() = ui::linked_accounts::LinkState::Linked(ada());
    }
}

fn said(app: &mut TestApp<SettingsShell>) -> Vec<String> {
    fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = widget.accessibility() {
            out.extend(node.name);
            out.extend(node.value);
            out.extend(node.description);
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    change(app, |_| ());
    let mut out = Vec::new();
    walk(page_of(app.harness().root_widget()), &mut out);
    out
}

#[test]
fn linked_accounts_show_where_the_workspace_is_connected_to_a_service() {
    let (_scratch, root) = scratch_root();
    let mut app = window_showing(AccountsPane::from_dir_in(
        root.clone(),
        AccountDirectory::default(),
        false,
    ));
    assert!(
        !said(&mut app).contains(&t("accounts-linked-title")),
        "no section where no host lends linked accounts"
    );

    let host = Rc::new(Bridged(std::cell::RefCell::new(
        ui::linked_accounts::LinkState::Unlinked {
            connect: true,
            elsewhere: Some("In the Discord server, type /link to link from there.".into()),
        },
    )));
    let linked = ui::linked_accounts::LinkedAccounts::new(host.clone());
    let mut app = window_showing(
        AccountsPane::from_dir_in(root, AccountDirectory::default(), false)
            .with_linked(linked.clone()),
    );
    let shown = said(&mut app);
    assert!(shown.contains(&t("accounts-linked-title")), "{shown:?}");
    assert!(shown.contains(&"Connect Discord".to_owned()), "{shown:?}");
    assert!(
        shown.iter().any(|line| line.contains("type /link")),
        "how to link from Discord's side: {shown:?}"
    );

    linked.link("so.tilo.discord");
    let shown = said(&mut app);
    assert!(
        shown.iter().any(|line| line.contains("Opening Discord")),
        "{shown:?}"
    );

    *host.0.borrow_mut() =
        ui::linked_accounts::LinkState::Offered(ui::linked_accounts::OfferReading {
            service: "Discord".into(),
            account: ada(),
            started_here: false,
        });
    let shown = said(&mut app);
    assert!(
        shown
            .iter()
            .any(|line| line.contains("Link Discord Ada L · @ada.l to you?")),
        "{shown:?}"
    );
    assert!(shown.contains(&t("accounts-linked-decline")), "{shown:?}");

    linked.accept("so.tilo.discord");
    let shown = said(&mut app);
    assert!(
        shown.contains(&"Linked as Ada L · @ada.l".to_owned()),
        "{shown:?}"
    );
    assert!(shown.contains(&t("accounts-linked-unlink")), "{shown:?}");
}

#[test]
fn the_mail_form_names_its_fields_to_assistive_technology() {
    let mut pane = AccountsPane::from_dir(sample_directory(), true);
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    assert!(pane.form_open(), "Add opens the inline form");
    let mut app = window_showing(pane);
    let controls = app.harness().controls();
    let names: Vec<_> = controls.iter().filter_map(|c| c.name.clone()).collect();
    let field = controls
        .iter()
        .find(|control| {
            control.name.as_deref() == Some("Display name")
                && control.role == ui::accesskit::Role::TextInput
        })
        .unwrap_or_else(|| panic!("the exported tree names the display-name field: {names:#?}"));
    assert!(
        control_rect(app.harness(), &field.id).is_some_and(|rect| rect.width() > 0.0),
        "the named field is drawn where a press reaches it"
    );

    // A screen reader, or an agent acting for one, reaches the field by that
    // name: focus and the text land in the form's own draft.
    let id = app
        .harness()
        .a11y_node_named("Display name")
        .expect("the field is exported by its name");
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::Focus)
        .expect("the field takes focus");
    app.harness()
        .perform_a11y_action(id, &ui::a11y::Action::SetValue("Ada".into()))
        .expect("the field takes the text");
    assert_eq!(
        change(&mut app, |pane| pane.form_name.get()),
        "Ada",
        "typing by accessible name reaches the draft"
    );
}

/// Where the exported node `id` stands, if it is drawn.
fn control_rect(harness: &ui::headless::FrameHarness, id: &str) -> Option<ui::vello::kurbo::Rect> {
    let id = ui::accesskit::NodeId(id.parse().ok()?);
    let node = harness.a11y_node(id)?;
    node.bounds()
        .map(|bounds| ui::vello::kurbo::Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
}
