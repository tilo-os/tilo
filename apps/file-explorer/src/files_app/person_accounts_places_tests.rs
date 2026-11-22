use super::*;
use crate::remote_accounts;

/// The app that keeps the workspace's accounts record, as Settings does.
struct AccountsOwner;

impl ui::framework::ResourceApp for AccountsOwner {
    type Resource = crate::resource::FileExplorer;
    const ID: &'static str = accounts::person::ACCOUNTS_OWNER;
    const TITLE: &'static str = "Settings";

    fn view(_cx: &mut ui::framework::ViewCx<crate::resource::FileExplorer>) -> ui::widget::Element {
        Box::new(ui::widget::Column::new())
    }
}

fn person() -> String {
    "fe".repeat(32)
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
    kept.invoke(&accounts::person::AccountList::saving(&account))
        .unwrap();
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
