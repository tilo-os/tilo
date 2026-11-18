//! Calendar setup against an existing canonical account. No credentials are copied.
use crate::{CalDavConfig, RemoteCalendar};

#[cfg(all(test, feature = "live"))]
mod tests;

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

#[cfg(feature = "live")]
pub fn discover_calendars(
    service: &tilo_dav::oauth::Service,
    account: &accounts::Account,
    config: &CalDavConfig,
    google: bool,
) -> Result<Vec<RemoteCalendar>, String> {
    service
        .discover(&account.id, &config.server_url, &config.username, google)
        .map(|collections| {
            collections
                .into_iter()
                .enumerate()
                .map(|(index, collection)| RemoteCalendar {
                    id: collection.href,
                    name: collection.name,
                    color: (index % 8) as u8,
                    visible: true,
                    read_only: collection.read_only,
                })
                .collect()
        })
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
