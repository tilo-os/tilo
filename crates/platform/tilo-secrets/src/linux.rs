//! The Linux backend: the freedesktop Secret Service
//! (`org.freedesktop.secrets`: GNOME Keyring, `KWallet`, `KeePassXC`) over the
//! session bus, with a plain session.
//!
//! A plain session carries secrets unencrypted over the bus, which is private
//! to the user's session — the same trade `secret-tool` makes. Items carry
//! `application = <app id>` and `purpose = <key>`. Locked collections and
//! items are unlocked through the service's own prompt; dismissing it is
//! [`SecretError::Denied`].
//!
//! Every call runs on one bus thread of this module's own. zbus' blocking API
//! drives each call on a runtime of its own, which panics on a thread that
//! already drives a tokio runtime, and callers include services answering on
//! their runtime's thread. They wait on the bus thread instead, for as long
//! as the person takes to answer a prompt.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, mpsc};

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
        *guard = Some(open_session()?);
    }
    Ok(guard)
}

fn open_session() -> Result<Session, SecretError> {
    let connection = Connection::session().map_err(unavailable)?;
    // A process the store cannot examine proves who it is first; one it can
    // examine does nothing here. A failed proof is not an error of its own:
    // the store then refuses the calls, as it would have without it.
    let _proved = crate::device_proof::prove_blocking(&connection);
    let (_, path): (OwnedValue, OwnedObjectPath) = service(&connection)?
        .call("OpenSession", &("plain", Value::from("")))
        .map_err(unavailable)?;
    Ok(Session { connection, path })
}

/// Why one attempt at a call failed: the service no longer knows this
/// process's session (it restarted since the session was opened), it
/// refused a process that identifies itself by proof (a restarted service
/// has forgotten the proof too), or anything else.
enum Attempt {
    StaleSession,
    Refused(SecretError),
    Failed(SecretError),
}

impl From<SecretError> for Attempt {
    fn from(error: SecretError) -> Self {
        Self::Failed(error)
    }
}

/// [`failed`] for a call that names this process's session: a service that
/// restarted since the session was opened answers `NoSession`.
fn session_call_failed(error: zbus::Error) -> Attempt {
    if let zbus::Error::MethodError(name, _, _) = &error {
        if name.as_str() == "org.freedesktop.Secret.Error.NoSession" {
            return Attempt::StaleSession;
        }
        if name.as_str() == "org.freedesktop.Secret.Error.AccessDenied"
            && crate::device_proof::in_use()
        {
            return Attempt::Refused(failed(error));
        }
    }
    Attempt::Failed(failed(error))
}

/// Run `call` with the process's session. A session the service has
/// forgotten (it restarted, through D-Bus activation or its unit's
/// `Restart=`) is reopened and the call made once more, so a long-lived
/// client's first call after a restart does not fail. A process that
/// proves who it is gets the same second attempt after a refusal, on a
/// connection that proved again.
fn with_session<T>(call: impl Fn(&Session) -> Result<T, Attempt>) -> Result<T, SecretError> {
    let mut guard = session()?;
    let result = call_reopening(&mut guard, call);
    reset_on_disconnect(&mut guard, &result);
    drop(guard);
    result
}

/// [`with_session`]'s call on `current`, reopening it once if the service
/// forgot it.
fn call_reopening<T>(
    current: &mut Option<Session>,
    call: impl Fn(&Session) -> Result<T, Attempt>,
) -> Result<T, SecretError> {
    let attempt = |current: &Option<Session>| match current {
        Some(session) => call(session),
        None => Err(Attempt::Failed(SecretError::Denied)),
    };
    let mut result = attempt(current);
    if matches!(result, Err(Attempt::StaleSession | Attempt::Refused(_))) {
        *current = None;
        result = match open_session() {
            Ok(session) => {
                *current = Some(session);
                attempt(current)
            }
            Err(error) => Err(Attempt::Failed(error)),
        };
    }
    result.map_err(|attempt| match attempt {
        Attempt::StaleSession => {
            SecretError::Failed("the Secret Service forgot a session it had just opened".into())
        }
        Attempt::Refused(error) | Attempt::Failed(error) => error,
    })
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
) -> Result<Vec<OwnedObjectPath>, Attempt> {
    let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service(connection)?
        .call("SearchItems", &(attributes,))
        .map_err(session_call_failed)?;
    if !locked.is_empty() {
        unlock(connection, locked.clone())?;
        unlocked.extend(locked);
    }
    Ok(unlocked)
}

type Job = Box<dyn FnOnce() + Send>;

/// The bus thread's queue and identity. `None` when the thread could not
/// start.
fn bus() -> Option<&'static (Mutex<mpsc::Sender<Job>>, std::thread::ThreadId)> {
    static BUS: OnceLock<Option<(Mutex<mpsc::Sender<Job>>, std::thread::ThreadId)>> =
        OnceLock::new();
    BUS.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = tilo_rt::spawn_thread("tilo-secrets-bus", move || {
            while let Ok(job) = rx.recv() {
                job();
            }
        })
        .ok()?;
        Some((Mutex::new(tx), thread.thread().id()))
    })
    .as_ref()
}

/// Run `call` on the bus thread and wait for its answer.
fn on_bus<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    let (queue, thread) = bus().ok_or_else(|| {
        SecretError::Unavailable("the secret store's bus thread did not start".into())
    })?;
    if std::thread::current().id() == *thread {
        return call();
    }
    let (reply, answer) = mpsc::channel();
    let job: Job = Box::new(move || {
        let _ = reply.send(call());
    });
    queue
        .lock()
        .map_err(|_| SecretError::Failed("secret store queue poisoned".into()))?
        .send(job)
        .map_err(|_| SecretError::Unavailable("the secret store's bus thread ended".into()))?;
    answer
        .recv()
        .unwrap_or_else(|_| Err(SecretError::Failed("the secret store call was lost".into())))
}

impl SecretBackend for SecretService {
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }

    fn probe(&self) -> Result<(), SecretError> {
        on_bus(|| session().map(drop))
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        let (app, key) = (app.to_owned(), key.to_owned());
        on_bus(move || lookup(&attributes(&app, &key)))
    }

    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
        let (app, key, label) = (app.to_owned(), key.to_owned(), label.to_owned());
        let value = zeroize::Zeroizing::new(value.to_vec());
        on_bus(move || create(&app, &key, &value, &label))
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        let (app, key) = (app.to_owned(), key.to_owned());
        on_bus(move || remove(&attributes(&app, &key)))
    }
}

/// Store `value` under `app` and `key` in the default collection.
fn create(app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
    with_session(|Session { connection, path }| {
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
            .map_err(session_call_failed)?;
        if complete_prompt(connection, &prompt)? {
            Ok(())
        } else {
            Err(SecretError::Denied.into())
        }
    })
}

/// The first item carrying `attributes`, in search order, as `secret-tool
/// lookup` reads it.
fn lookup(attributes: &HashMap<&str, &str>) -> Result<Option<Secret>, SecretError> {
    with_session(|Session { connection, path }| {
        let found = items(connection, attributes)?;
        if found.is_empty() {
            return Ok(None);
        }
        let secrets: HashMap<OwnedObjectPath, WireSecret> = service(connection)?
            .call("GetSecrets", &(found.clone(), path))
            .map_err(session_call_failed)?;
        Ok(found
            .iter()
            .find_map(|item| secrets.get(item))
            .map(|(_, _, value, _)| Secret::new(value.clone())))
    })
}

/// Delete every item carrying `attributes`.
fn remove(attributes: &HashMap<&str, &str>) -> Result<(), SecretError> {
    with_session(|Session { connection, .. }| {
        for item in items(connection, attributes)? {
            let prompt: OwnedObjectPath =
                proxy(connection, item.as_str(), "org.freedesktop.Secret.Item")?
                    .call("Delete", &())
                    .map_err(failed)?;
            if !complete_prompt(connection, &prompt)? {
                return Err(SecretError::Denied.into());
            }
        }
        Ok(())
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A service answering on its runtime's thread (the collaboration
    /// broker presenting an app's presence) reached the secret store and
    /// aborted the session: zbus' blocking call panicked inside the runtime.
    #[test]
    fn a_runtime_thread_gets_an_answer_rather_than_a_panic() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", scratch.path().join("no-bus").display()),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (probe, read) = runtime.block_on(async {
            (
                SecretService.probe(),
                SecretService.get("so.tilo.test", "missing"),
            )
        });
        assert!(
            matches!(probe, Err(SecretError::Unavailable(_))),
            "{probe:?}"
        );
        assert!(read.is_err(), "{read:?}");
        drop(env);
    }
}
