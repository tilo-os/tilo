//! Read-only, exact branded OSCrypt lookup on the original user's authenticated bus.
use super::*;
use std::{
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Duration,
};

pub(super) fn owner_bus(uid: u32) -> Result<Connection, SecretError> {
    let socket =
        UnixStream::connect(format!("/run/user/{uid}/bus")).map_err(|_| SecretError::Denied)?;
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .map_err(|_| SecretError::Denied)?;
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(peer).cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != uid
    {
        return Err(SecretError::Denied);
    }
    socket
        .set_nonblocking(true)
        .map_err(|_| SecretError::Denied)?;
    // Register the very same authenticated socket on the persistent platform
    // runtime. Leave its context before zbus' blocking builder drives handshake.
    let socket = {
        let _context = tilo_rt::app().handle().enter();
        tokio::net::UnixStream::from_std(socket).map_err(|_| SecretError::Denied)?
    };
    zbus::blocking::connection::Builder::unix_stream(socket)
        .method_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| SecretError::Denied)
}
pub(super) fn fixed_proxy<'a>(
    connection: &Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'static str,
) -> Result<Proxy<'a>, SecretError> {
    Proxy::new(connection, destination, path, interface).map_err(|_| SecretError::Denied)
}
pub(super) fn service_owner(bus: &Proxy<'_>, uid: u32) -> Result<String, SecretError> {
    let owner: String = bus
        .call("GetNameOwner", &(SERVICE,))
        .map_err(|_| SecretError::Denied)?;
    let actual: u32 = bus
        .call("GetConnectionUnixUser", &(&owner,))
        .map_err(|_| SecretError::Denied)?;
    if actual != uid || !owner.starts_with(':') {
        return Err(SecretError::Denied);
    }
    Ok(owner)
}
fn existing_item(
    service: &Proxy<'_>,
    application: &'static str,
) -> Result<Option<(OwnedObjectPath, bool)>, SecretError> {
    let attributes = HashMap::from([("application", application)]);
    let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
        .call("SearchItems", &(attributes,))
        .map_err(|_| SecretError::Denied)?;
    if unlocked.len().saturating_add(locked.len()) > 1 {
        return Err(SecretError::Denied);
    }
    Ok(unlocked
        .into_iter()
        .map(|item| (item, false))
        .chain(locked.into_iter().map(|item| (item, true)))
        .next())
}
pub(super) fn unlock_item(
    connection: &Connection,
    destination: &str,
    service: &Proxy<'_>,
    item: &OwnedObjectPath,
) -> Result<(), SecretError> {
    let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
        .call("Unlock", &(vec![item],))
        .map_err(|_| SecretError::Denied)?;
    if prompt.as_str() == "/" {
        return Ok(());
    }
    let prompt = fixed_proxy(
        connection,
        destination,
        prompt.as_str(),
        "org.freedesktop.Secret.Prompt",
    )?;
    let mut completed = prompt
        .receive_signal("Completed")
        .map_err(|_| SecretError::Denied)?;
    prompt
        .call_method("Prompt", &("",))
        .map_err(|_| SecretError::Denied)?;
    let message = completed.next().ok_or(SecretError::Denied)?;
    let (dismissed, _): (bool, OwnedValue) = message
        .body()
        .deserialize()
        .map_err(|_| SecretError::Denied)?;
    if dismissed {
        return Err(SecretError::Denied);
    }
    Ok(())
}
pub(crate) fn read_chromium(
    uid: u32,
    application: &'static str,
) -> Result<Option<Secret>, SecretError> {
    on_bus(move || {
        if uid == 0
            || unsafe { libc::getuid() } != uid
            || unsafe { libc::geteuid() } != uid
            || !matches!(application, "chrome" | "chromium" | "Slack")
        {
            return Err(SecretError::Denied);
        }
        let connection = owner_bus(uid)?;
        let bus = fixed_proxy(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        let own = connection.unique_name().ok_or(SecretError::Denied)?;
        let own_uid: u32 = bus
            .call("GetConnectionUnixUser", &(own,))
            .map_err(|_| SecretError::Denied)?;
        if own_uid != uid {
            return Err(SecretError::Denied);
        }
        let owner = service_owner(&bus, uid)?;
        let service = fixed_proxy(
            &connection,
            &owner,
            SERVICE_PATH,
            "org.freedesktop.Secret.Service",
        )?;
        let Some((item, locked)) = existing_item(&service, application)? else {
            return Ok(None);
        };
        if locked {
            unlock_item(&connection, &owner, &service, &item)?;
            if existing_item(&service, application)? != Some((item.clone(), false)) {
                return Err(SecretError::Denied);
            }
        }
        let item_proxy = fixed_proxy(
            &connection,
            &owner,
            item.as_str(),
            "org.freedesktop.Secret.Item",
        )?;
        let attributes: HashMap<String, String> = item_proxy
            .get_property("Attributes")
            .map_err(|_| SecretError::Denied)?;
        if attributes.get("application").map(String::as_str) != Some(application) {
            return Err(SecretError::Denied);
        }
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|_| SecretError::Denied)?;
        let result = (|| {
            let (returned_session, parameters, bytes, content_type): WireSecret = item_proxy
                .call("GetSecret", &(&session,))
                .map_err(|_| SecretError::Denied)?;
            let value = zeroize::Zeroizing::new(bytes);
            if returned_session != session
                || !parameters.is_empty()
                || value.is_empty()
                || value.len() > 4096
                || value.contains(&0)
                || std::str::from_utf8(&value).is_err()
                || !matches!(
                    content_type.as_str(),
                    "text/plain" | "text/plain; charset=utf8" | "text/plain; charset=utf-8"
                )
                || service_owner(&bus, uid)? != owner
            {
                return Err(SecretError::Denied);
            }
            Ok(Some(Secret::new(value.to_vec())))
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

#[cfg(all(test, feature = "native-chromium-qualification"))]
mod native_process;
