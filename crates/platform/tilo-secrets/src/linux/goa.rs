//! Existing owner GOA object and exact Secret Service item; no refresh or mutation.
use super::chromium::{fixed_proxy, owner_bus, service_owner, unlock_item};
use super::{
    Connection, HashMap, OwnedObjectPath, OwnedValue, Proxy, SERVICE_PATH, Secret, SecretError,
    Value, WireSecret, on_bus,
};
use crate::owner_migration::{GoaCredentials, GoaProvider, GoaRegistration};
const GOA: &str = "org.gnome.OnlineAccounts";
fn goa_owner(bus: &Proxy<'_>, uid: u32) -> Result<Option<String>, SecretError> {
    let present: bool = bus
        .call("NameHasOwner", &(GOA,))
        .map_err(|_| SecretError::Denied)?;
    if !present {
        return Ok(None);
    }
    let owner: String = bus
        .call("GetNameOwner", &(GOA,))
        .map_err(|_| SecretError::Denied)?;
    let actual: u32 = bus
        .call("GetConnectionUnixUser", &(&owner,))
        .map_err(|_| SecretError::Denied)?;
    if actual != uid || !owner.starts_with(':') {
        return Err(SecretError::Denied);
    }
    Ok(Some(owner))
}
fn registration(
    connection: &Connection,
    owner: &str,
    provider: GoaProvider,
    id: &str,
    identity: &str,
) -> Result<GoaRegistration, SecretError> {
    let path = format!("/org/gnome/OnlineAccounts/Accounts/{id}");
    let account = fixed_proxy(connection, owner, &path, "org.gnome.OnlineAccounts.Account")?;
    let actual_provider: String = account
        .get_property("ProviderType")
        .map_err(|_| SecretError::Denied)?;
    let actual_id: String = account
        .get_property("Id")
        .map_err(|_| SecretError::Denied)?;
    let actual_identity: String = account
        .get_property("Identity")
        .map_err(|_| SecretError::Denied)?;
    let files_disabled: bool = account
        .get_property("FilesDisabled")
        .map_err(|_| SecretError::Denied)?;
    if actual_provider != provider.as_str()
        || actual_id != id
        || actual_identity != identity
        || files_disabled
    {
        return Err(SecretError::Denied);
    }
    // FilesDisabled alone is present even on providers without a Files service.
    // Require the actual enabled interface; this does not refresh credentials.
    let files = fixed_proxy(connection, owner, &path, "org.gnome.OnlineAccounts.Files")?;
    let files_uri: String = files.get_property("Uri").map_err(|_| SecretError::Denied)?;
    if files_uri.is_empty() || files_uri.len() > 2048 || files_uri.chars().any(char::is_control) {
        return Err(SecretError::Denied);
    }
    let oauth = fixed_proxy(
        connection,
        owner,
        &path,
        "org.gnome.OnlineAccounts.OAuth2Based",
    )?;
    let client_id: String = oauth
        .get_property("ClientId")
        .map_err(|_| SecretError::Denied)?;
    let client_secret = zeroize::Zeroizing::new(
        oauth
            .get_property::<String>("ClientSecret")
            .map_err(|_| SecretError::Denied)?,
    );
    if client_id.is_empty()
        || client_id.len() > 2048
        || client_id.chars().any(char::is_control)
        || client_secret.len() > 4096
        || client_secret.chars().any(char::is_control)
    {
        return Err(SecretError::Denied);
    }
    Ok(GoaRegistration {
        client_id,
        client_secret,
    })
}
fn selected_item(
    connection: &Connection,
    owner: &str,
    service: &Proxy<'_>,
    provider: GoaProvider,
    id: &str,
) -> Result<Proxy<'static>, SecretError> {
    let key = format!("{}:gen{}:{id}", provider.as_str(), provider.generation());
    let attributes = HashMap::from([("goa-identity", key.as_str())]);
    let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
        .call("SearchItems", &(attributes,))
        .map_err(|_| SecretError::Denied)?;
    if unlocked.len().saturating_add(locked.len()) != 1 {
        return Err(SecretError::Denied);
    }
    let item = unlocked
        .first()
        .or_else(|| locked.first())
        .ok_or(SecretError::Denied)?;
    if !locked.is_empty() {
        unlock_item(connection, owner, service, item)?;
    }
    let selected = Proxy::new(
        connection,
        owner.to_owned(),
        item.clone(),
        "org.freedesktop.Secret.Item",
    )
    .map_err(|_| SecretError::Denied)?;
    let actual: HashMap<String, String> = selected
        .get_property("Attributes")
        .map_err(|_| SecretError::Denied)?;
    if actual.get("goa-identity") != Some(&key) {
        return Err(SecretError::Denied);
    }
    Ok(selected)
}
pub(crate) fn read_goa(
    uid: u32,
    provider: GoaProvider,
    id: String,
    identity: String,
) -> Result<GoaCredentials, SecretError> {
    on_bus(move || {
        if uid == 0 || unsafe { libc::getuid() } != uid || unsafe { libc::geteuid() } != uid {
            return Err(SecretError::Denied);
        }
        let connection = owner_bus(uid)?;
        let bus = fixed_proxy(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        let source_owner = goa_owner(&bus, uid)?;
        let original = source_owner
            .as_ref()
            .map(|owner| registration(&connection, owner, provider, &id, &identity))
            .transpose()?;
        let owner = service_owner(&bus, uid)?;
        let service = fixed_proxy(
            &connection,
            &owner,
            SERVICE_PATH,
            "org.freedesktop.Secret.Service",
        )?;
        let selected = selected_item(&connection, &owner, &service, provider, &id)?;
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|_| SecretError::Denied)?;
        let result = (|| {
            let (returned, parameters, bytes, content_type): WireSecret = selected
                .call("GetSecret", &(&session,))
                .map_err(|_| SecretError::Denied)?;
            let value = zeroize::Zeroizing::new(bytes);
            if returned != session
                || !parameters.is_empty()
                || value.is_empty()
                || value.len() > 64 * 1024
                || std::str::from_utf8(&value).is_err()
                || !matches!(
                    content_type.as_str(),
                    "text/plain" | "text/plain; charset=utf8" | "text/plain; charset=utf-8"
                )
                || service_owner(&bus, uid)? != owner
                || goa_owner(&bus, uid)? != source_owner
            {
                return Err(SecretError::Denied);
            }
            let attributes: HashMap<String, String> = selected
                .get_property("Attributes")
                .map_err(|_| SecretError::Denied)?;
            let expected = format!("{}:gen{}:{id}", provider.as_str(), provider.generation());
            if attributes.get("goa-identity") != Some(&expected) {
                return Err(SecretError::Denied);
            }
            let current = source_owner
                .as_ref()
                .map(|owner| registration(&connection, owner, provider, &id, &identity))
                .transpose()?;
            if original
                .as_ref()
                .map(|r| (&r.client_id, r.client_secret.as_str()))
                != current
                    .as_ref()
                    .map(|r| (&r.client_id, r.client_secret.as_str()))
            {
                return Err(SecretError::Denied);
            }
            Ok(GoaCredentials {
                registration: original,
                secret: Secret::new(value.to_vec()),
            })
        })();
        if let Ok(session) = fixed_proxy(
            &connection,
            &owner,
            session.as_str(),
            "org.freedesktop.Secret.Session",
        ) {
            let _ = session.call_method("Close", &());
        }
        result
    })
}

#[cfg(all(test, feature = "native-goa-qualification"))]
mod native_process;
#[cfg(all(test, feature = "native-goa-qualification"))]
mod tests;
