//! The Linux backend: the freedesktop Secret Service
//! (`org.freedesktop.secrets`: GNOME Keyring, `KWallet`, `KeePassXC`) over the
//! session bus, with a plain session.
//!
//! A plain session carries secrets unencrypted over the bus, which is private
//! to the user's session — the same trade `secret-tool` makes. Items carry
//! `application = <app id>` and `purpose = <key>`. Locked collections and
//! items are unlocked through the service's own prompt; dismissing it is
//! [`SecretError::Denied`].

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{Protection, Secret, SecretBackend, SecretError};

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";

/// `(session, parameters, value, content type)`.
type WireSecret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

pub(crate) struct SecretService;

/// One connection and plain session per process, opened on first use and
/// reopened after a failure.
struct Session {
    connection: Connection,
    path: OwnedObjectPath,
}

fn session() -> Result<std::sync::MutexGuard<'static, Option<Session>>, SecretError> {
    static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    let mut guard = SESSION
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| SecretError::Failed("secret session poisoned".into()))?;
    if guard.is_none() {
        let connection = Connection::session().map_err(unavailable)?;
        let (_, path): (OwnedValue, OwnedObjectPath) = service(&connection)?
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(unavailable)?;
        *guard = Some(Session { connection, path });
    }
    Ok(guard)
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` adapter")]
fn unavailable(error: zbus::Error) -> SecretError {
    SecretError::Unavailable(error.to_string())
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` adapter")]
fn failed(error: zbus::Error) -> SecretError {
    if let zbus::Error::MethodError(name, _, _) = &error
        && name.as_str() == "org.freedesktop.Secret.Error.IsLocked"
    {
        return SecretError::Denied;
    }
    SecretError::Failed(format!("Secret Service: {error}"))
}

fn service(connection: &Connection) -> Result<Proxy<'static>, SecretError> {
    Proxy::new(
        connection,
        SERVICE,
        SERVICE_PATH,
        "org.freedesktop.Secret.Service",
    )
    .map_err(unavailable)
}

fn proxy<'a>(
    connection: &Connection,
    path: &'a str,
    interface: &'static str,
) -> Result<Proxy<'a>, SecretError> {
    Proxy::new(connection, SERVICE, path, interface).map_err(failed)
}

fn attributes<'a>(app: &'a str, key: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("application", app), ("purpose", key)])
}

/// Run the service's unlock or confirmation prompt at `prompt`, if any, and
/// wait for the user. `Ok(false)` when they dismissed it.
fn complete_prompt(connection: &Connection, prompt: &OwnedObjectPath) -> Result<bool, SecretError> {
    if prompt.as_str() == "/" {
        return Ok(true);
    }
    let prompt = proxy(connection, prompt.as_str(), "org.freedesktop.Secret.Prompt")?;
    let mut completed = prompt.receive_signal("Completed").map_err(failed)?;
    prompt.call_method("Prompt", &("",)).map_err(failed)?;
    let signal = completed
        .next()
        .ok_or_else(|| SecretError::Failed("the prompt went away".into()))?;
    let (dismissed, _result): (bool, OwnedValue) = signal.body().deserialize().map_err(failed)?;
    Ok(!dismissed)
}

/// Unlock `objects`, prompting if the service asks to.
fn unlock(connection: &Connection, objects: Vec<OwnedObjectPath>) -> Result<(), SecretError> {
    let (_unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service(connection)?
        .call("Unlock", &(objects,))
        .map_err(failed)?;
    if complete_prompt(connection, &prompt)? {
        Ok(())
    } else {
        Err(SecretError::Denied)
    }
}

/// Every item carrying `attributes`, unlocked.
fn items(
    connection: &Connection,
    attributes: &HashMap<&str, &str>,
) -> Result<Vec<OwnedObjectPath>, SecretError> {
    let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service(connection)?
        .call("SearchItems", &(attributes,))
        .map_err(failed)?;
    if !locked.is_empty() {
        unlock(connection, locked.clone())?;
        unlocked.extend(locked);
    }
    Ok(unlocked)
}

impl SecretBackend for SecretService {
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }

    fn probe(&self) -> Result<(), SecretError> {
        session().map(drop)
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        lookup(&attributes(app, key))
    }

    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
        let mut guard = session()?;
        let result = (|| {
            let Session { connection, path } = guard.as_ref().ok_or(SecretError::Denied)?;
            let collection = proxy(
                connection,
                DEFAULT_COLLECTION,
                "org.freedesktop.Secret.Collection",
            )?;
            if collection.get_property::<bool>("Locked").map_err(failed)? {
                let default = OwnedObjectPath::try_from(DEFAULT_COLLECTION)
                    .map_err(|error| SecretError::Failed(error.to_string()))?;
                unlock(connection, vec![default])?;
            }
            let properties: HashMap<&str, Value<'_>> = HashMap::from([
                ("org.freedesktop.Secret.Item.Label", Value::from(label)),
                (
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(attributes(app, key)),
                ),
            ]);
            let secret = (
                path,
                Vec::<u8>::new(),
                value.to_vec(),
                "application/octet-stream",
            );
            let (_item, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
                .call("CreateItem", &(properties, secret, true))
                .map_err(failed)?;
            if complete_prompt(connection, &prompt)? {
                Ok(())
            } else {
                Err(SecretError::Denied)
            }
        })();
        reset_on_disconnect(&mut guard, &result);
        result
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        remove(&attributes(app, key))
    }

    fn legacy_get(&self, attributes: &[(&str, &str)]) -> Result<Option<Secret>, SecretError> {
        if attributes.is_empty() {
            return Ok(None);
        }
        lookup(&attributes.iter().copied().collect())
    }

    fn legacy_delete(&self, attributes: &[(&str, &str)]) -> Result<(), SecretError> {
        if attributes.is_empty() {
            return Ok(());
        }
        remove(&attributes.iter().copied().collect())
    }
}

/// The first item carrying `attributes`, in search order, as `secret-tool
/// lookup` reads it.
fn lookup(attributes: &HashMap<&str, &str>) -> Result<Option<Secret>, SecretError> {
    let mut guard = session()?;
    let result = (|| {
        let Session { connection, path } = guard.as_ref().ok_or(SecretError::Denied)?;
        let found = items(connection, attributes)?;
        if found.is_empty() {
            return Ok(None);
        }
        let secrets: HashMap<OwnedObjectPath, WireSecret> = service(connection)?
            .call("GetSecrets", &(found.clone(), path))
            .map_err(failed)?;
        Ok(found
            .iter()
            .find_map(|item| secrets.get(item))
            .map(|(_, _, value, _)| Secret::new(value.clone())))
    })();
    reset_on_disconnect(&mut guard, &result);
    result
}

/// Delete every item carrying `attributes`.
fn remove(attributes: &HashMap<&str, &str>) -> Result<(), SecretError> {
    let mut guard = session()?;
    let result = (|| {
        let Session { connection, .. } = guard.as_ref().ok_or(SecretError::Denied)?;
        for item in items(connection, attributes)? {
            let prompt: OwnedObjectPath =
                proxy(connection, item.as_str(), "org.freedesktop.Secret.Item")?
                    .call("Delete", &())
                    .map_err(failed)?;
            if !complete_prompt(connection, &prompt)? {
                return Err(SecretError::Denied);
            }
        }
        Ok(())
    })();
    reset_on_disconnect(&mut guard, &result);
    result
}

/// Drop the session after a transport failure, so the next call reconnects
/// (the service restarted, or the bus went away).
fn reset_on_disconnect<T>(session: &mut Option<Session>, result: &Result<T, SecretError>) {
    if matches!(
        result,
        Err(SecretError::Unavailable(_) | SecretError::Failed(_))
    ) {
        *session = None;
    }
}
