use super::*;
use crate::accounts::sample_directory;
use crate::effects::page_of;
use crate::resource::SettingsShell;
use crate::testing::actions_of;
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
    let mut store = accounts::AccountStore::new();
    store.upsert(account);
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

/// A private config root: the stores, vault and mutation journal of one
/// test never meet another's, and nothing reaches the user's own.
fn scratch_root() -> (tempfile::TempDir, AccountRoot) {
    let dir = tempfile::tempdir().expect("scratch config root");
    let root = AccountRoot::at(dir.path().join("tilo"));
    (dir, root)
}

#[test]
fn selected_browser_sessions_use_device_inventory_not_public_identity_records() {
    let (_scratch, root) = scratch_root();
    let mut store = accounts::AccountStore::new();
    store.upsert(accounts::Account::new("selected", browser_sessions::TYPE));
    store.upsert(accounts::Account::new("other", "email"));
    store.save_in(&root.path().unwrap()).unwrap();
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

/// Drive the host's polling until the pending account probe reports,
/// however long the probe thread takes to be scheduled.
fn settle_probes(pane: &mut AccountsPane) {
    let deadline = ui::time::now() + ProbeJob::<String>::DEADLINE;
    loop {
        let _ = pane.poll();
        if pane.form_probe.is_none()
            && pane.reauth_probe.is_none()
            && !pane.form_saving
            && !pane.reauth_saving
            && pane.lane.is_idle()
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

/// A Settings window whose External Accounts area holds `pane`, open on
/// its page: the page the window builds over it, its controls bound to
/// the actions the window publishes.
pub(super) fn window_showing(pane: AccountsPane) -> TestApp<SettingsShell> {
    crate::area::replace_in_next_window(move |context| AccountsArea::holding(context, pane));
    let mut app = TestApp::<SettingsShell>::new().expect("Settings opens headlessly");
    crate::effects::show_pane(&mut app, crate::categories::Category::ExternalAccounts);
    app.settle();
    app
}

/// What `read` finds on the page of a Settings window showing `pane`.
pub(super) fn on_page<T>(pane: AccountsPane, read: impl FnOnce(&dyn Widget) -> T) -> T {
    let mut app = window_showing(pane);
    read(page_of(app.harness().root_widget()))
}

/// Change the pane `app`'s External Accounts area holds with `change`;
/// the page is built again over it.
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

/// Every action the page of a Settings window showing `pane` offers,
/// by what it says.
fn page_actions(pane: AccountsPane) -> Vec<String> {
    on_page(pane, |page| {
        let mut actions = Vec::new();
        actions_of(page, &mut actions);
        actions
    })
}

/// Every line the page of `app` announces.
pub(super) fn announced(app: &mut TestApp<SettingsShell>) -> Vec<String> {
    provider_settings_ui::widgets::announced_text(page_of(app.harness().root_widget()))
}

/// Sample directories are read-only, real ones get labelled destructive remove buttons, and empty stores stay empty.
#[test]
fn import_only_rows_keep_recorded_errors_offline_and_signin_requirements() {
    /// What the rows say of each account's state: the marks after
    /// their names.
    fn texts(widget: &dyn Widget) -> Vec<String> {
        let mut result: Vec<String> = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::ListRow>())
            .map(|row| row.tag_labels().into_iter().map(str::to_owned).collect())
            .unwrap_or_default();
        for child in widget.children() {
            result.extend(texts(child));
        }
        result
    }
    let (_scratch, root) = scratch_root();
    let id = accounts::AccountId::from("import-google");
    let mut account =
        accounts::Account::new(id.clone(), account_custody::providers::GOOGLE_DRIVE_TYPE);
    account.config = serde_json::json!({"files_mode": "import", "use_files": true});
    let mut store = accounts::AccountStore::new();
    store.upsert(account);
    root.save_store(&store).unwrap();
    let reporter = root.sync_reporter(id);
    for expected in [
        "accounts-status-error",
        "accounts-status-offline",
        "accounts-status-reauth",
    ] {
        match expected {
            "accounts-status-error" => reporter.failed(),
            "accounts-status-offline" => reporter.offline(),
            _ => reporter.needs_authentication(),
        }
        .unwrap();
        let labels = on_page(AccountsPane::in_root(root.clone()), texts);
        assert!(labels.contains(&t(expected)), "{labels:?}");
        assert!(
            !labels.contains(&t("accounts-status-import-only")),
            "a known failure is not an untested import source"
        );
    }
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
        // The sample is illustration: one live Add button per provider kind,
        // metadata import, and no Remove buttons. A scratch root keeps whatever the runner's
        // own account stores hold (a browser identity, say) out of the count.
        let (_scratch, root) = scratch_root();
        let pane = AccountsPane::from_dir_in(root, sample_directory(), true);
        let labels = page_actions(pane);
        assert_eq!(
            labels.len(),
            AccountKind::ALL.len() + 1,
            "every provider kind can be added and metadata imported over the sample: {labels:?}"
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
                .filter(|label| **label == t("accounts-import-choose"))
                .count(),
            1,
            "one metadata import action"
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
        // One Add per kind, metadata import, four maintenance actions
        // on each of four accounts, and the Nextcloud account's
        // calendars.
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
        // And no account of any kind leaked in from the sample.
        for kind in AccountKind::ALL {
            assert!(
                pane.directory().views_of(kind).is_empty(),
                "{kind:?} must be empty"
            );
        }
    }
}

/// Removing an account asks first; a connection check reports later.
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

/// The inline add form: local drafts, labelled fields, empty-email no-op and rejected adds.
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
    // Isolate custody selection from an actual session Secret Service, and
    // isolate ConfigGuard's environment from other tests in this process.
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
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (_dir, account_root) = scratch_root();
    let root = account_root.path().unwrap();
    let old_id = accounts::AccountId::from("existing@example.test");
    let mut store = accounts::AccountStore::new();
    store.upsert(accounts::Account::new(old_id.clone(), "email"));
    account_root.save_store(&store).unwrap();
    let store_path = accounts::store_path_in(&root);
    let metadata_before = std::fs::read(&store_path).unwrap();
    let mut vault = accounts::SecretVault::open_in(&root);
    vault.put(&old_id, &accounts::Secret::password("old-password"));
    vault.try_save().unwrap();
    assert_eq!(
        accounts::SecretVault::open_in(&root).custody(),
        accounts::SecretCustody::LocalFile
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
    assert_eq!(std::fs::read(&store_path).unwrap(), metadata_before);
    let saved = account_root.store();
    assert_eq!(saved.all().len(), 1);
    assert!(saved.get(&old_id).is_some());
    assert!(
        saved
            .get(&accounts::AccountId::from("new@example.test"))
            .is_none()
    );
}

/// Edits keep unknown config and toggles; re-authentication probes before persisting.
#[test]
fn account_edits_preserve_config_and_probe_before_persisting() {
    {
        let (_dir, root) = scratch_root();
        let id = accounts::AccountId::from("nextcloud-stable-id");
        let mut original = accounts::Account::new(id.clone(), crate::accounts::NEXTCLOUD_TYPE);
        original.display_name = "Old label".into();
        original.enabled = false;
        original.tint = 7;
        original.owner = Some("local-owner".into());
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
        let mut store = accounts::AccountStore::new();
        store.upsert(original);
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
            "https://new.example/",
            "",
        )
        .expect("edit account");

        let saved = root.store().get(&id).cloned().expect("saved account");
        assert!(!saved.enabled);
        assert_eq!(saved.tint, 7);
        assert_eq!(saved.owner.as_deref(), Some("local-owner"));
        assert_eq!(saved.config["use_files"], false);
        assert_eq!(saved.config["use_calendar"], true);
        assert_eq!(saved.config["use_mail"], false);
        assert_eq!(saved.config["future_provider_field"]["kept"], true);
        assert_eq!(saved.config["server_url"], "https://new.example");
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
        let mut store = accounts::AccountStore::new();
        store.upsert(account);
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

/// Sync now and pause or resume ride on exactly the accounts the cloud
/// service lists as mounted, and the window offers the same controls.
#[test]
fn account_actions_include_sync_now_for_mounted_accounts() {
    use runtime_fs::cloud::{CloudCapabilities, CloudMount, MountHealth};
    let mount = |account: &str, paused: bool| CloudMount {
        account: account.into(),
        mount_point: std::path::PathBuf::from("/home/ada/Remote").join(account),
        health: Some(MountHealth::Healthy),
        paused,
        capabilities: CloudCapabilities::default(),
    };
    for paused in [false, true] {
        let (_scratch, root) = scratch_root();
        // Sample accounts take fresh ids each time: read them back.
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

/// Wait for a background read to end, as the warm-up's wake would.
fn until_finished(finished: impl Fn() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !finished() {
        assert!(std::time::Instant::now() < deadline, "the read ends");
        std::thread::yield_now();
    }
}

/// The warm-up opens the vault off the UI thread once; the model holds
/// what it opened, so a saved credential is there.
#[test]
fn the_area_builds_its_model_from_the_read_it_started() {
    let (dir, root) = scratch_root();
    let id = accounts::AccountId::from("warm@example.test");
    let mut store = accounts::AccountStore::new();
    store.upsert(accounts::Account::new(id.clone(), "email"));
    root.save_store(&store).expect("the store is saved");
    let mut vault = root.vault();
    vault.put(&id, &accounts::Secret::password("test-only-password"));
    vault.try_save().expect("the vault is saved");
    let cx = crate::area::AreaContext::in_profile(dir.path().to_path_buf());
    let area = AccountsArea::new(&cx).with_account_root(root.clone());
    assert!(area.start_read(), "the first read starts");
    assert!(!area.start_read(), "one read at a time");
    assert!(!area.is_open(), "a running read publishes nothing yet");
    until_finished(|| area.read_finished());
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

/// A mutation whose intent is on disk is recovered before the vault is
/// opened, so a vault read before it is not used.
#[test]
fn a_pending_mutation_is_seen() {
    let (_dir, root) = scratch_root();
    assert!(!root.mutation_pending());
    let config = root.path().expect("an explicit root");
    let accounts_dir = accounts::accounts_dir_in(&config);
    std::fs::create_dir_all(&accounts_dir).expect("the accounts directory");
    std::fs::write(accounts_dir.join("mutation-journal.json"), b"{}").expect("an intent");
    assert!(root.mutation_pending());
}

/// A Secret Service that does not answer holds no click: removing an
/// account and checking its connection answer at once on the window's
/// thread, the account stays shown until the vault took its removal,
/// and the removal lands once the vault answers.
#[test]
fn a_stalled_vault_never_holds_the_window() {
    use std::sync::Arc;
    use std::time::Instant;

    let (_dir, root) = scratch_root();
    let id = accounts::AccountId::from("stalled@example.test");
    let mut store = accounts::AccountStore::new();
    store.upsert(accounts::Account::new(id.clone(), "email"));
    root.save_store(&store).expect("the store is saved");
    let mut vault = root.vault();
    vault.put(&id, &accounts::Secret::password("test-only-password"));
    vault.try_save().expect("the vault is saved");
    // What the window's warm-up opened off the UI thread.
    let read = AccountsRead::of(&root);
    let config = root.path().expect("an explicit root");
    let gate = crate::stall::Stall::new();
    let window = {
        let id = id.clone();
        std::thread::spawn(move || {
            ui::rt::mark_ui_thread();
            let stalled = Arc::clone(&gate);
            let root = AccountRoot::at(config).with_vault_gate(move || {
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
            // The vault answers nothing until the gate opens below:
            // clicks answered while it is held did not wait for it.
            gate.assert_held("the clicks");
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

            gate.release();
            let deadline = tilo_test_env::hang_deadline();
            while removed.borrow().is_none() || !pane.lane.is_idle() {
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
    assert!(
        root.vault().get(&id).is_none(),
        "the credential went with the account"
    );
    assert!(!root.mutation_pending(), "the removal's journal is cleared");
}

/// Whether custody holds a Mail account's grant is read on a worker:
/// a vault that does not answer holds no frame of the pane, and the
/// grant shows once it does.
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
    let mut store = accounts::AccountStore::new();
    store.upsert(account);
    root.save_store(&store).expect("the store is saved");
    let dir = AccountDirectory::load_in(&root);
    assert_eq!(dir.mail.accounts.len(), 1, "the pane lists a Mail account");
    root.custodian()
        .seal_oauth(
            &id,
            &account_custody::TokenSet {
                access_token: "test-only-access".into(),
                scope: None,
                refresh_token: "test-only-refresh".into(),
                expires_in: 3600,
                obtained_at: 0,
            },
        )
        .expect("the grant is sealed");
    let read = AccountsRead::of(&root);
    let config = root.path().expect("an explicit root");
    let gate = crate::stall::Stall::new();
    // A window that waited on the vault would sit until the stall gives up,
    // which `assert_held` reports.
    let stalled = std::sync::Arc::clone(&gate);
    let root = AccountRoot::at(config).with_vault_gate(move || {
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
    gate.assert_held("composing the accounts");
    assert!(!shown, "the grant is not known before the vault answers");
    assert!(
        app.with_widget::<crate::window::Inspected, _>(|settings| {
            area(settings).unwrap().model().borrow().busy()
        })
        .unwrap(),
        "the pane keeps polling while the vault is out"
    );

    gate.release();
    let deadline = tilo_test_env::hang_deadline();
    while !shows_granted(&mut app) {
        assert!(Instant::now() < deadline, "the grant is shown");
        app.settle();
        std::thread::yield_now();
    }
}

/// A room bridged to a Discord server, its host answering at once.
struct Bridged(std::cell::RefCell<ui::linked_accounts::LinkState>);

fn ada() -> ui::linked_accounts::AccountName {
    ui::linked_accounts::AccountName {
        user_name: "ada.l".into(),
        display_name: Some("Ada L".into()),
    }
}

impl ui::linked_accounts::LinkedAccountsHost for Bridged {
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

/// Every name and value the page of `app` gives assistive technology,
/// built again over what its area holds now.
fn said(app: &mut TestApp<SettingsShell>) -> Vec<String> {
    fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = widget.accessibility() {
            out.extend(node.name);
            out.extend(node.value);
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
fn linked_accounts_show_where_the_room_is_connected_to_a_service() {
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
