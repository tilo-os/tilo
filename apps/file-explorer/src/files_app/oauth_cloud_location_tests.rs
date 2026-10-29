use super::*;
use crate::remote_accounts::{self, RemoteAccountHealth};

const PROVIDERS: [(&str, &str); 4] = [
    ("a-google", remote_accounts::GOOGLE_DRIVE_TYPE),
    ("b-onedrive", remote_accounts::ONEDRIVE_TYPE),
    ("c-dropbox", remote_accounts::DROPBOX_TYPE),
    ("d-icloud", remote_accounts::ICLOUD_DRIVE_TYPE),
];

fn fixture_store(root: &Path) -> accounts::AccountStore {
    let mut store = accounts::AccountStore::new();
    for (id, provider) in PROVIDERS {
        let mut account = accounts::Account::new(id, provider);
        account.display_name = "Cloud".into();
        // Projection must select only public provider facts, not forward
        // arbitrary stored configuration or imply authenticated access.
        account.config = serde_json::json!({"unsupported_private_field":"secret-sentinel"});
        store.upsert(account);
    }
    store.save_in(root).unwrap();
    accounts::AccountSyncReporter::in_root(accounts::AccountId::from("a-google"), root)
        .offline()
        .unwrap();
    store
}

#[test]
fn canonical_oauth_accounts_publish_sidebar_resources_and_open_exact_cloud_mounts() {
    with_owner_callback(canonical_oauth_accounts);
}

fn canonical_oauth_accounts() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let _store = fixture_store(config.path());
    let mut widget = FileExplorerWidget::with_persistence_dirs(
        None,
        Some(home.path().to_owned()),
        favorites::UserDirs::default(),
    );
    widget.state().accounts_root = Some(config.path().to_owned());
    widget.state().refresh_external_search_catalogs();
    let mut document = automerge::AutoCommit::new();
    let accounts = widget.state().remote_account_snapshot();
    assert_eq!(accounts.len(), PROVIDERS.len());
    let first = accounts
        .iter()
        .find(|account| account.id.as_str() == "a-google")
        .unwrap();
    assert_eq!(first.health, RemoteAccountHealth::Offline);
    assert!(
        accounts
            .iter()
            .filter(|account| account.id.as_str() != "a-google")
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
    assert!(
        !accounts::vault_dir_in(config.path()).exists(),
        "projection and open must not create credential custody"
    );
}

#[test]
fn disabled_and_removed_oauth_accounts_update_existing_places_and_semantic_actions() {
    with_owner_callback(disabled_and_removed_oauth_accounts);
}

fn disabled_and_removed_oauth_accounts() {
    let config = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut store = fixture_store(config.path());
    let mut disabled = store
        .get(&accounts::AccountId::from("c-dropbox"))
        .unwrap()
        .clone();
    disabled.enabled = false;
    store.upsert(disabled);
    store.save_in(config.path()).unwrap();
    let mut widget = FileExplorerWidget::with_persistence_dirs(
        None,
        Some(home.path().to_owned()),
        favorites::UserDirs::default(),
    );
    widget.state().accounts_root = Some(config.path().to_owned());
    widget.state().refresh_external_search_catalogs();
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
    store.remove(&accounts::AccountId::from("c-dropbox"));
    store.save_in(config.path()).unwrap();
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
