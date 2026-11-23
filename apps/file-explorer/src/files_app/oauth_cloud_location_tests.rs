use super::*;
use crate::remote_accounts::{self, RemoteAccountHealth};

const PROVIDERS: [(&str, &str); 4] = [
    ("a-google", remote_accounts::GOOGLE_DRIVE_TYPE),
    ("b-onedrive", remote_accounts::ONEDRIVE_TYPE),
    ("c-dropbox", remote_accounts::DROPBOX_TYPE),
    ("d-icloud", remote_accounts::ICLOUD_DRIVE_TYPE),
];

/// The app that keeps the person's accounts record, as Settings does.
struct AccountsOwner;

impl ui::framework::ResourceApp for AccountsOwner {
    type Resource = crate::resource::FileExplorer;
    const ID: &'static str = accounts::person::ACCOUNTS_OWNER;
    const TITLE: &'static str = "Settings";

    fn view(_cx: &mut ui::framework::ViewCx<crate::resource::FileExplorer>) -> ui::widget::Element {
        Box::new(ui::widget::Column::new())
    }
}

type Accounts = ui::framework::WorkspaceResource<accounts::person::AccountList>;

/// Save `account` into the workspace's accounts as Settings does.
fn save(record: &Accounts, account: &accounts::Account) {
    record
        .invoke(&accounts::person::AccountList::SAVE_ACCOUNT.with((
            account.id.to_string(),
            account.type_id.to_string(),
            account.display_name.clone(),
            account.config.clone(),
            account.enabled,
            account.tint,
        )))
        .unwrap();
}

fn person() -> String {
    "fe".repeat(32)
}

/// The person's accounts record, kept as Settings keeps it, with the four
/// providers saved in it.
fn fixture_record() -> (ui::framework::testing::TestApp<AccountsOwner>, Accounts) {
    let mut settings =
        ui::framework::testing::TestApp::<AccountsOwner>::for_person(&person()).unwrap();
    let kept = settings
        .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
        .unwrap();
    for (id, provider) in PROVIDERS {
        let mut account = accounts::Account::new(id, provider);
        account.display_name = "Cloud".into();
        account.config = serde_json::json!({"unsupported_private_field":"secret-sentinel"});
        save(&kept, &account);
    }
    (settings, kept)
}

tilo_test_env::scenario! {
    fn oauth_accounts_are_sidebar_places_that_follow_their_account;

    phase canonical_oauth_accounts_publish_sidebar_resources_and_open_exact_cloud_mounts;
    {
        let (settings, record) = fixture_record();
        with_owner_callback(move || canonical_oauth_accounts(settings, record));
    }

    phase disabled_and_removed_oauth_accounts_update_existing_places_and_semantic_actions;
    {
        let (settings, record) = fixture_record();
        with_owner_callback(move || disabled_and_removed_oauth_accounts(settings, &record));
    }
}

fn canonical_oauth_accounts(
    _settings: ui::framework::testing::TestApp<AccountsOwner>,
    record: Accounts,
) {
    let home = tempfile::tempdir().unwrap();
    let mut widget = FileExplorerWidget::with_persistence_dirs(
        None,
        Some(home.path().to_owned()),
        favorites::UserDirs::default(),
    );
    widget.state().follow_person_accounts(record);
    let mut document = automerge::AutoCommit::new();
    let accounts = widget.state().remote_account_snapshot();
    assert_eq!(accounts.len(), PROVIDERS.len());
    assert!(
        accounts
            .iter()
            .all(|account| account.health == RemoteAccountHealth::NeverChecked)
    );
    for (index, (id, provider)) in PROVIDERS.iter().enumerate() {
        let account = accounts
            .iter()
            .find(|account| account.id.as_str() == *id)
            .unwrap();
        assert_eq!(remote_accounts::type_id(account.kind).as_str(), *provider);
        let name = if index == 0 {
            "Cloud".into()
        } else {
            format!("Cloud ({})", index.checked_add(1).unwrap())
        };
        let mount = home.path().join("Remote").join(name);
        assert_eq!(account.mount.as_ref(), Some(&mount));
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(mount.join("Selected-provider.txt"), id.as_bytes()).unwrap();
        let places = widget.state().current_places().sections();
        assert!(
            places.iter().flat_map(|section| &section.items).any(|row| {
                row.id == format!("loc:{}", mount.display()) && row.label == "Cloud"
            })
        );
        let resource_id = remote_accounts::resource_id(&account.id);
        let resources = crate::files_app::published::tree(&widget, &document);
        let resource = resources
            .iter()
            .flat_map(|root| &root.children)
            .find(|resource| resource.id.as_str() == resource_id)
            .unwrap();
        assert_eq!(resource.kind, "files.remote-account");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION)
            .unwrap();
        assert_eq!(open.contract.availability, Availability::Available);
        assert!(
            !serde_json::to_string(&resource.presentation)
                .unwrap()
                .contains("secret-sentinel")
        );
        assert!(matches!(
            crate::files_app::published::invoke(
                &mut widget,
                &mut document,
                &resource_id,
                crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
                &serde_json::json!({})
            )
            .unwrap(),
            Some(ui::framework::Effected::Done(_))
        ));
        assert_eq!(widget.state().current_path, mount);
        assert_eq!(
            std::fs::read(widget.state().current_path.join("Selected-provider.txt")).unwrap(),
            id.as_bytes()
        );
    }
}

fn disabled_and_removed_oauth_accounts(
    _settings: ui::framework::testing::TestApp<AccountsOwner>,
    record: &Accounts,
) {
    let home = tempfile::tempdir().unwrap();
    let mut disabled = record.get().account("c-dropbox").unwrap();
    disabled.enabled = false;
    save(record, &disabled);
    let mut widget = FileExplorerWidget::with_persistence_dirs(
        None,
        Some(home.path().to_owned()),
        favorites::UserDirs::default(),
    );
    widget.state().follow_person_accounts(record.clone());
    let mut document = automerge::AutoCommit::new();
    let places = widget.state().current_places().sections();
    assert!(places.iter().flat_map(|section| &section.items).any(|row| {
        row.id == "account:c-dropbox" && row.secondary.as_deref() == Some("Disabled")
    }));
    let id = remote_accounts::resource_id(&accounts::AccountId::from("c-dropbox"));
    let resources = crate::files_app::published::tree(&widget, &document);
    let resource = resources
        .iter()
        .flat_map(|root| &root.children)
        .find(|resource| resource.id.as_str() == id)
        .unwrap();
    assert!(resource.actions.iter().any(|action| {
        action.id == crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION
            && matches!(&action.contract.availability, Availability::Unavailable { code, .. } if code == "account_disabled")
    }));
    record
        .invoke(
            &accounts::person::AccountList::REMOVE_ACCOUNT
                .with(ui::tilo_resource::Id::from_raw("c-dropbox")),
        )
        .unwrap();
    widget.state().refresh_external_search_catalogs();
    assert!(
        !widget
            .state()
            .current_places()
            .sections()
            .iter()
            .flat_map(|section| &section.items)
            .any(|row| row.id == "account:c-dropbox")
    );
    let resources = crate::files_app::published::tree(&widget, &document);
    assert!(
        !resources
            .iter()
            .flat_map(|root| &root.children)
            .any(|resource| resource.id.as_str() == id)
    );
    assert!(
        crate::files_app::published::invoke(
            &mut widget,
            &mut document,
            &id,
            crate::search_provider::REMOTE_ACCOUNT_OPEN_ACTION,
            &serde_json::json!({})
        )
        .is_err()
    );
}

fn account_rows(
    files: &mut ui::framework::testing::TestApp<crate::resource::FileExplorerShell>,
) -> Vec<(String, String)> {
    files
        .with_widget(|widget: &mut FileExplorerWidget| {
            widget
                .state()
                .current_places()
                .sections()
                .iter()
                .flat_map(|section| &section.items)
                .filter(|row| row.id.starts_with("account:"))
                .map(|row| (row.id.clone(), row.label.clone()))
                .collect()
        })
        .unwrap()
}

#[test]
fn an_account_saved_to_the_persons_accounts_shows_in_files_places_live() {
    let mut settings =
        ui::framework::testing::TestApp::<AccountsOwner>::for_person(&person()).unwrap();
    let mut files =
        ui::framework::testing::TestApp::<crate::resource::FileExplorerShell>::for_person(&person())
            .unwrap();
    let kept = settings
        .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
        .unwrap();
    let read = files
        .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
        .expect("Files reads the workspace's accounts");
    assert_eq!(
        read.record().route(),
        kept.record().route(),
        "Files reads the accounts record Settings keeps"
    );
    files.settle();
    assert!(account_rows(&mut files).is_empty());

    let mut account = accounts::Account::new("work-cloud", remote_accounts::NEXTCLOUD_TYPE);
    account.display_name = "Work cloud".into();
    account.config = serde_json::json!({"server_url": "https://cloud.example", "username": "ada"});
    save(&kept, &account);
    read.record()
        .merge_peer_document(kept.record().document().unwrap())
        .unwrap();
    files.settle();
    assert_eq!(
        account_rows(&mut files),
        [("account:work-cloud".to_owned(), "Work cloud".to_owned())],
        "the window that was already open shows the account"
    );

    kept.invoke(
        &accounts::person::AccountList::REMOVE_ACCOUNT
            .with(ui::tilo_resource::Id::from_raw("work-cloud")),
    )
    .unwrap();
    read.record()
        .merge_peer_document(kept.record().document().unwrap())
        .unwrap();
    files.settle();
    assert!(
        account_rows(&mut files).is_empty(),
        "a removed account leaves Files' places"
    );
}
