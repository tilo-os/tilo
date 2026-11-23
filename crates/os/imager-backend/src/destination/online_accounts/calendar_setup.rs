//! Calendar setup against an existing canonical account. No credentials are copied.
use calendar_model::account::{CalDavConfig, RemoteCalendar};

pub const GOOGLE_CALDAV_URL: &str = "https://apidata.googleusercontent.com/caldav/v2/";

pub fn google_config(email: &str) -> CalDavConfig {
    let calendar_id = url::form_urlencoded::byte_serialize(email.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
        .replace("%40", "@");
    CalDavConfig {
        server_url: format!("{GOOGLE_CALDAV_URL}{calendar_id}/user"),
        username: email.into(),
        ..Default::default()
    }
}

pub(crate) fn same_origin_url(base: &url::Url, href: &str) -> Result<url::Url, String> {
    let target = base.join(href).map_err(|_| "invalid calendar URL")?;
    if target.origin() != base.origin()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.query().is_some()
        || target.fragment().is_some()
        || !matches!(target.scheme(), "http" | "https")
    {
        return Err("calendar URL must stay on the configured origin".into());
    }
    Ok(target)
}

/// Enable only confirmed collections on the original account. The caller saves
/// the canonical store; Files settings, account ID/type and vault remain intact.
/// `discovered` must be the result of discovery, not user-entered collection URLs.
pub fn select_calendars(
    account: &mut accounts::Account,
    config: &CalDavConfig,
    discovered: &[RemoteCalendar],
    selected: &[String],
    google: bool,
) -> Result<(), String> {
    if selected.is_empty() {
        return Err("select at least one calendar".into());
    }
    let server = url::Url::parse(&config.server_url).map_err(|_| "invalid calendar server URL")?;
    let mut calendars = Vec::new();
    for id in selected {
        if calendars
            .iter()
            .any(|calendar: &RemoteCalendar| &calendar.id == id)
        {
            return Err("duplicate calendar selection".into());
        }
        let calendar = discovered
            .iter()
            .find(|calendar| &calendar.id == id)
            .ok_or("calendar was not discovered")?;
        same_origin_url(&server, id)?;
        calendars.push(calendar.clone());
    }
    // An account connected for reading only stays so when its calendars are
    // chosen again, and none of them is taken as one that can be written,
    // whatever its server answered for it.
    let read_only = account
        .config
        .get("calendar")
        .and_then(|kept| kept.get("read_only"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_default();
    for calendar in &mut calendars {
        calendar.read_only |= read_only;
    }
    let mut payload = serde_json::to_value(config).map_err(|error| error.to_string())?;
    let settings = payload
        .as_object_mut()
        .ok_or("calendar settings are not an object")?;
    settings.insert(
        "calendars".into(),
        serde_json::to_value(calendars).map_err(|error| error.to_string())?,
    );
    if read_only {
        settings.insert("read_only".into(), true.into());
    }
    let object = account
        .config
        .as_object_mut()
        .ok_or("account settings are not an object")?;
    object.insert("calendar".into(), payload);
    object.insert("use_calendar".into(), true.into());
    if google {
        object.insert("calendar_provider".into(), "google".into());
    } else {
        object.remove("calendar_provider");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
use super::*;
use accounts::person::AccountList;
use accounts::{Account, AccountId, SecretVault};
use test_http::{Reply, Responder};

fn collection(server: &str) -> RemoteCalendar {
    RemoteCalendar {
        id: format!("{server}/cal/work/"),
        name: "Work".into(),
        color: 1,
        visible: true,
        read_only: false,
    }
}

tilo_test_env::scenario! {
    /// Setting up a Nextcloud account: discovery, and the selection that is kept or refused.
    fn nextcloud_calendars_are_discovered_and_selected;

    phase selected_nextcloud_calendars_keep_files_and_no_password;
    {
        let root = tempfile::tempdir().unwrap();
        let mut account = Account::new("canonical-nextcloud", "nextcloud");
        account.config = serde_json::json!({"server_url":"https://cloud.example/remote.php/dav/files/internal/", "username":"internal", "use_files":true, "files":{"root":"documents"}});
        let original = account.config.clone();
        let config = CalDavConfig {
            server_url: "https://cloud.example/remote.php/dav/".into(),
            username: "internal".into(),
            ..Default::default()
        };
        let calendars = vec![collection("https://cloud.example")];
        select_calendars(
            &mut account,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert_eq!(account.config["files"], original["files"]);
        assert_eq!(account.config["server_url"], original["server_url"]);
        let mut record = accounts::person::staged_in(root.path()).unwrap();
        record
            .apply_confirmed(AccountList::saving(&account))
            .unwrap();
        accounts::person::keep_staged_in(root.path(), &mut record).unwrap();
        let saved = record.state().unwrap();
        assert_eq!(saved.all().len(), 1);
        assert_eq!(
            saved.account(account.id.as_str()).unwrap().type_id.as_str(),
            "nextcloud"
        );
        assert_eq!(
            saved.account(account.id.as_str()).unwrap().config["files"],
            original["files"]
        );
        let projection = calendar_model::account::CalendarAccounts::from_shared(&saved);
        assert_eq!(projection.accounts.len(), 1);
        assert_eq!(projection.accounts[0].id, "canonical-nextcloud");
        assert_eq!(
            projection.accounts[0].config.password, "",
            "a calendar's password is the workspace's vault's, never this device's"
        );
        assert!(
            SecretVault::open_in(root.path()).get(&account.id).is_none(),
            "a calendar's password is the workspace's vault's, never this device's"
        );
    }

    phase an_account_connected_for_reading_only_stays_so_when_its_calendars_are_chosen_again;
    {
        let config = CalDavConfig {
            server_url: "https://cloud.example/remote.php/dav/".into(),
            username: "internal".into(),
            ..Default::default()
        };
        // Its server would let the person write this calendar.
        let calendars = vec![collection("https://cloud.example")];
        let mut account = Account::new("canonical-nextcloud", "nextcloud");
        account.config = serde_json::json!({
            "server_url": "https://cloud.example/remote.php/dav/files/internal/",
            "username": "internal",
            "use_files": true,
            "calendar": {
                "server_url": "https://cloud.example/remote.php/dav/",
                "username": "internal",
                "read_only": true,
            },
        });
        select_calendars(
            &mut account,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert_eq!(
            account.config["calendar"]["read_only"], true,
            "the account keeps its word: {}",
            account.config
        );
        assert_eq!(
            account.config["calendar"]["calendars"][0]["read_only"], true,
            "no calendar of it is taken as one that can be written: {}",
            account.config
        );
        let mut shared = AccountList::default();
        shared.keep(&account).unwrap();
        let projection = calendar_model::account::CalendarAccounts::from_shared(&shared);
        assert_eq!(projection.accounts.len(), 1);
        assert!(projection.accounts[0].read_only);
        assert!(projection.accounts[0].calendars[0].read_only);

        // An account that is written to is chosen as its server answers.
        let mut writing = Account::new("another-nextcloud", "nextcloud");
        writing.config = serde_json::json!({
            "server_url": "https://cloud.example/remote.php/dav/files/internal/",
            "username": "internal",
            "use_files": true,
        });
        select_calendars(
            &mut writing,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert!(writing.config["calendar"].get("read_only").is_none());
        assert_eq!(writing.config["calendar"]["calendars"][0]["read_only"], false);
    }

    phase selection_rejects_unconfirmed_duplicate_and_cross_origin_urls_without_mutation;
    {
        let mut account = Account::new("a", "nextcloud");
        account.config = serde_json::json!({"use_files":true});
        let before = account.clone();
        let config = CalDavConfig {
            server_url: "https://cloud.example/".into(),
            ..Default::default()
        };
        let valid = collection("https://cloud.example");
        for selected in [
            vec![],
            vec!["unknown".into()],
            vec![valid.id.clone(), valid.id.clone()],
        ] {
            assert!(
                select_calendars(
                    &mut account,
                    &config,
                    std::slice::from_ref(&valid),
                    &selected,
                    false
                )
                .is_err()
            );
            assert_eq!(account, before);
        }
        let hostile = collection("https://attacker.example");
        assert!(
            select_calendars(
                &mut account,
                &config,
                std::slice::from_ref(&hostile),
                std::slice::from_ref(&hostile.id),
                false
            )
            .is_err()
        );
        assert_eq!(account, before);
    }

}

tilo_test_env::scenario! {
    /// Setting up a Google account: the calendar URL its address makes, and what its selection
    /// keeps.
    fn google_calendars_are_addressed_and_selected_by_their_url;

    phase google_collection_id_cannot_change_the_url_path_or_query;
    {
        let config = google_config("ada+calendar /?#%@example.org");
        assert_eq!(
            config.server_url,
            "https://apidata.googleusercontent.com/caldav/v2/ada%2Bcalendar%20%2F%3F%23%25@example.org/user"
        );
    }

    phase google_projection_uses_original_id_and_never_serializes_tokens;
    {
        let mut account = Account::new("drive-account", "google-drive");
        account.config = serde_json::json!({"use_files":true});
        let config = google_config("ada@example.org");
        assert_eq!(
            config.server_url,
            "https://apidata.googleusercontent.com/caldav/v2/ada@example.org/user"
        );
        let calendar = RemoteCalendar {
            id: config.server_url.clone(),
            ..collection("https://unused.example")
        };
        select_calendars(
            &mut account,
            &config,
            std::slice::from_ref(&calendar),
            std::slice::from_ref(&calendar.id),
            true,
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut record = accounts::person::staged_in(root.path()).unwrap();
        record
            .apply_confirmed(AccountList::saving(&account))
            .unwrap();
        accounts::person::keep_staged_in(root.path(), &mut record).unwrap();
        let credential_store = account_credentials::CredentialStore::for_root(Some(root.path().to_owned()));
        credential_store
            .seal_oauth(
                &AccountId::from("drive-account"),
                &account_credentials::TokenSet {
                    access_token: "approved-google-access".into(),
                    refresh_token: "canonical-refresh".into(),
                    obtained_at: 1,
                    expires_in: 3600,
                    scope: None,
                },
            )
            .unwrap();
        let vault = SecretVault::open_in(root.path());
        let before = vault.get(&AccountId::from("drive-account")).unwrap();
        let shared = record.state().unwrap();
        let projection = calendar_model::account::CalendarAccounts::from_shared(&shared);
        assert_eq!(
            projection.accounts[0].config.oauth_account_id.as_deref(),
            Some("drive-account")
        );
        assert!(projection.accounts[0].config.password.is_empty());
        assert_eq!(
            SecretVault::open_in(root.path()).get(&AccountId::from("drive-account")),
            Some(before)
        );
        let plaintext = String::from_utf8_lossy(
            &std::fs::read(accounts::person::staged_path_in(root.path())).unwrap(),
        )
        .into_owned();
        assert!(!plaintext.contains("approved-google-access"));
        assert!(!plaintext.contains("canonical-refresh"));
    }
}
}
