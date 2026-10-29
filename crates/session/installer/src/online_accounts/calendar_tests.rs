use super::*;
use std::os::unix::fs::PermissionsExt;

#[derive(Default)]
struct NoBrowser(std::sync::atomic::AtomicUsize);
impl Drop for NoBrowser {
    fn drop(&mut self) {
        assert_eq!(
            self.0.load(Ordering::SeqCst),
            0,
            "selection must not launch authorization"
        );
    }
}
impl BrowserOpener for NoBrowser {
    fn open(&self, _: &str) -> Result<(), browser::BrowserError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(browser::BrowserError::Unavailable)
    }
    fn close(&self) -> Result<(), browser::BrowserError> {
        Ok(())
    }
}

pub(crate) fn reviewing() -> (tempfile::TempDir, AccountsController) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut controller = AccountsController::fixture(
        directory.path(),
        Arc::new(NoBrowser::default()),
        |custody| custody,
    )
    .unwrap();
    let config = calendar::CalDavConfig {
        server_url: "https://cloud.example/remote.php/dav/".into(),
        username: "person".into(),
        ..Default::default()
    };
    let mut account = new_account("nextcloud");
    account.config = serde_json::json!({"server_url": "https://cloud.example", "username": "person", "use_files": true});
    controller.pending = Some(Verified {
        entries: vec![(
            account,
            PendingCredential::Existing(Secret::password("app-password")),
        )],
        calendar: Some(CalendarReview {
            entry: 0,
            config,
            google: false,
            discovered: vec![calendar::RemoteCalendar {
                id: "https://cloud.example/remote.php/dav/calendars/person/work/".into(),
                name: "Work".into(),
                color: 0,
                visible: true,
                read_only: false,
            }],
        }),
    });
    controller.status = AccountsStatus::ReviewCalendars;
    (directory, controller)
}

#[test]
fn review_does_not_save_before_explicit_selection() {
    let (_directory, mut controller) = reviewing();
    assert!(controller.poll() == AccountsStatus::ReviewCalendars);
    assert_eq!(controller.calendars().len(), 1);
    assert!(controller.selected().is_empty());
    assert!(controller.connected().is_empty());
    assert!(AccountStore::load_in(&controller.root).is_empty());
    let id = controller.calendars()[0].0.clone();
    controller
        .command(AccountsCommand::SelectCalendars(vec![id.clone()]))
        .unwrap();
    assert!(controller.status() == AccountsStatus::Added);
    assert_eq!(controller.selected().len(), 1);
    let store = AccountStore::load_in(&controller.root);
    let account = store.get(&controller.selected()[0]).unwrap();
    assert_eq!(account.type_id.as_str(), "nextcloud");
    assert_eq!(account.config["use_files"], true);
    assert_eq!(account.config["calendar"]["calendars"][0]["id"], id);
    assert!(controller.calendars().is_empty());
    let connected = controller.connected();
    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0].provider, Provider::Nextcloud);
    assert!(connected[0].files && connected[0].calendar && !connected[0].mail);
    let vault = SecretVault::open_in(&controller.root);
    assert_eq!(vault.get(&account.id).unwrap().password, "app-password");
}

#[test]
fn invalid_selection_keeps_review_without_writing() {
    let (_directory, mut controller) = reviewing();
    let id = controller.calendars()[0].0.clone();
    for selection in [
        vec![],
        vec!["https://elsewhere.example/calendar/".into()],
        vec![id.clone(), id],
    ] {
        assert_eq!(
            controller.command(AccountsCommand::SelectCalendars(selection)),
            Err(AccountsError::InvalidRequest)
        );
        assert!(controller.status() == AccountsStatus::ReviewCalendars);
        assert_eq!(controller.calendars().len(), 1);
        assert!(controller.selected().is_empty());
    }
}

#[test]
fn selected_collections_and_credentials_survive_bound_adoption() {
    let (_directory, mut controller) = reviewing();
    let id = controller.calendars()[0].0.clone();
    controller
        .command(AccountsCommand::SelectCalendars(vec![id.clone()]))
        .unwrap();
    let selected = controller.selected().to_vec();
    let binding = PortableBinding::new("install", "disk", "alice", 1000).unwrap();
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = controller
        .stage_selected_with_key(&selected, &key, &binding)
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    assert_eq!(
        portable::import_sealed_accounts(destination.path(), &sealed, &key, binding.as_bytes())
            .unwrap(),
        selected
    );
    assert!(connected_accounts(destination.path(), &selected) == controller.connected());
    let store = AccountStore::load_in(destination.path());
    let account = store.get(&selected[0]).unwrap();
    assert_eq!(account.config["calendar"]["calendars"][0]["id"], id);
    assert_eq!(account.config["use_files"], true);
    let vault = SecretVault::open_in(destination.path());
    assert_eq!(
        account_custody::password_credential(&vault.get(&selected[0]).unwrap()).as_str(),
        "app-password"
    );
}

#[test]
fn cancellation_discards_discovery_and_credentials() {
    let (_directory, mut controller) = reviewing();
    controller.command(AccountsCommand::Cancel).unwrap();
    assert!(controller.status() == AccountsStatus::Ready);
    assert!(controller.pending.is_none());
    assert!(controller.calendars().is_empty());
    assert!(controller.selected().is_empty());
    assert_eq!(
        controller.command(AccountsCommand::SelectCalendars(vec![])),
        Err(AccountsError::InvalidRequest)
    );
}
