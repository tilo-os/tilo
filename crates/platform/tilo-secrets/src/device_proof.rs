//! How a process that holds the device identity is recognised by
//! `tilo-secret-service`, and by the person's session, although neither can look at it.
//!
//! The store names each caller by its executable, read from `/proc/<pid>/exe`, and a
//! process that loads the device key makes itself non-dumpable, so the kernel refuses
//! that link to every other process of the user, the store included. Such a process
//! proves the identity instead. The session, which reads the device key too, asks the
//! same of a process of the login whose namespaces the kernel hides from it
//! ([`session_key`]).

use std::os::unix::ffi::OsStringExt;
use std::sync::Mutex;

use tilo_seal::secret_service::CallerKey;

const SERVICE: &str = "org.freedesktop.secrets";
const MANAGEMENT_PATH: &str = "/so/tilo/Secrets1";
const MANAGEMENT: &str = "so.tilo.Secrets1";

/// The keys derived for proofs: to the store, and to the session.
#[derive(Clone)]
struct Armed {
    store: CallerKey,
    session: CallerKey,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

/// Arm this process with the device secret it just read; only the keys derived for
/// proofs are kept.
pub fn arm(device_secret: &[u8]) {
    let (Ok(store), Ok(session)) = (
        CallerKey::derive(device_secret),
        CallerKey::for_session(device_secret),
    ) else {
        return;
    };
    if let Ok(mut armed) = ARMED.lock() {
        *armed = Some(Armed { store, session });
    }
}

fn dumpable() -> bool {
    // SAFETY: PR_GET_DUMPABLE takes no pointers and only reads a flag.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 1 }
}

fn armed() -> Option<Armed> {
    if dumpable() {
        return None;
    }
    ARMED.lock().ok()?.clone()
}

fn key() -> Option<CallerKey> {
    armed().map(|armed| armed.store)
}

/// The key this process proves to the person's session with that it holds the device identity,
/// and the session checks such a proof with: `None` unless it read the device key, which made it
/// non-dumpable (`tilo_seal::secret_service::CallerKey::for_session`).
#[must_use]
pub fn session_key() -> Option<CallerKey> {
    armed().map(|armed| armed.session)
}

/// Whether this process identifies itself to the store by proof. A refusal is then
/// worth one more attempt on a fresh connection, since a store that restarted has
/// forgotten the proof.
pub(crate) fn in_use() -> bool {
    key().is_some()
}

fn answer(key: &CallerKey, challenge: &[u8], caller: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    // A process may always read its own link, dumpable or not.
    let program = std::fs::read_link("/proc/self/exe")
        .ok()?
        .into_os_string()
        .into_vec();
    let proof = key.prove(challenge, caller, &program).ok()?;
    Some((program, proof.to_vec()))
}

/// Prove the device identity on `connection`, if this process must.
/// `Ok(false)` when it need not or cannot (dumpable, or never read the
/// device key); a store that does not speak `so.tilo.Secrets1` (another
/// Secret Service) answers with an error, and the caller carries on
/// unproven.
pub(crate) fn prove_blocking(connection: &zbus::blocking::Connection) -> zbus::Result<bool> {
    let Some(key) = key() else {
        return Ok(false);
    };
    let caller = connection
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("a bus connection has no unique name".into()))?
        .to_string();
    let store = zbus::blocking::Proxy::new(connection, SERVICE, MANAGEMENT_PATH, MANAGEMENT)?;
    let challenge: Vec<u8> = store.call("CallerChallenge", &())?;
    let Some((program, proof)) = answer(&key, &challenge, &caller) else {
        return Ok(false);
    };
    let _identity: String = store.call("ProveCaller", &(program, proof))?;
    Ok(true)
}

pub async fn prove(connection: &zbus::Connection) -> zbus::Result<bool> {
    let Some(key) = key() else {
        return Ok(false);
    };
    let caller = connection
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("a bus connection has no unique name".into()))?
        .to_string();
    let store = zbus::Proxy::new(connection, SERVICE, MANAGEMENT_PATH, MANAGEMENT).await?;
    let challenge: Vec<u8> = store.call("CallerChallenge", &()).await?;
    let Some((program, proof)) = answer(&key, &challenge, &caller) else {
        return Ok(false);
    };
    let _identity: String = store.call("ProveCaller", &(program, proof)).await?;
    Ok(true)
}
