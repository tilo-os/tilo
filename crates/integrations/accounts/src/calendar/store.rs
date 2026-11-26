//! The calendar's own shape of the workspace's accounts: what a device with a
//! window reads them as. A calendar's password is the workspace's vault's, not
//! this device's: the app asks the session for it when it signs in.

use crate::Account as StoredAccount;
use crate::person::AccountList;
use serde::{Deserialize, Serialize};

use super::{CalDavConfig, CalendarAccount, CalendarAccounts, RemoteCalendar, TYPE_ID};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: CalDavConfig,
    #[serde(default)]
    calendars: Vec<RemoteCalendar>,
    #[serde(default)]
    collapsed: bool,
    /// Whether the account is connected for reading only, beside its server
    /// and its user name. An account that is written to says nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
}

impl CalendarAccount {
    fn from_stored(stored: &StoredAccount) -> Option<Self> {
        let payload: StoredConfig = stored
            .config
            .get("calendar")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .or_else(|| stored.config_as::<StoredConfig>())?;
        let mut config = payload.config;
        if config.display_name.is_empty() {
            config.display_name.clone_from(&stored.display_name);
        }
        // The entry of the workspace's vault this account's password is in, as Settings names it.
        // The app asks the session for it when it signs in, and this device keeps none.
        config.password_item = crate::email::password_item(&stored.config).map(str::to_owned);
        if stored
            .config
            .get("calendar_provider")
            .and_then(serde_json::Value::as_str)
            == Some("google")
        {
            config.oauth_account_id = Some(stored.id.to_string());
        }
        let mut account = Self {
            id: stored.id.to_string(),
            config,
            calendars: payload.calendars,
            collapsed: payload.collapsed,
            read_only: payload.read_only,
        };
        account.calendars = account.kept_calendars();
        Some(account)
    }
}

impl CalendarAccounts {
    /// The workspace's accounts as the calendar's shape, reading the record's calendar accounts and
    /// the Nextcloud accounts connected for calendars.
    pub fn from_shared(shared: &AccountList) -> Self {
        let accounts = shared
            .of_type(TYPE_ID)
            .into_iter()
            .chain(shared.all().into_iter().filter(|stored| {
                stored
                    .config
                    .get("use_calendar")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    && stored.type_id.as_str() != TYPE_ID
            }))
            .filter(|stored| stored.enabled)
            .filter_map(|stored| CalendarAccount::from_stored(&stored))
            .collect();
        Self { accounts }
    }
}
