//! How a process that holds the device identity is recognised by
//! `tilo-secret-service` although the store cannot read its executable.
//!
//! The store names each caller by its executable, read from
//! `/proc/<pid>/exe`. A process that loads the device key makes itself
//! non-dumpable (`tilo_user_identity::persist`), and the kernel then refuses
//! that link to every other process of the user, the store included: the
//! compositor, which loads the device key at startup, would be refused on
//! every call. So a process that read the device key arms this module with
//! it ([`arm`]); while the process is non-dumpable, each new connection to
//! the store first answers a challenge with a proof that it holds the key
//! (`tilo_seal::secret_service::CallerKey`) and names its own executable,
//! which the store then uses in place of the link it cannot read. A
//! process the kernel still lets the store examine proves nothing and is
//! identified as before.
//!
//! The store's side, and why the proof weakens nothing, is in
//! `tilo-secret-service`'s `app` module.

use std::os::unix::ffi::OsStringExt;
use std::sync::Mutex;

use tilo_seal::secret_service::CallerKey;

/// The store's well-known name, object and management interface.
const SERVICE: &str = "org.freedesktop.secrets";
const MANAGEMENT_PATH: &str = "/so/tilo/Secrets1";
const MANAGEMENT: &str = "so.tilo.Secrets1";

static ARMED: Mutex<Option<CallerKey>> = Mutex::new(None);

/// Arm this process with the device secret it just read. Only the key
/// derived for proofs is kept.
pub fn arm(device_secret: &[u8]) {
    let Ok(key) = CallerKey::derive(device_secret) else {
        return;
    };
    if let Ok(mut armed) = ARMED.lock() {
        *armed = Some(key);
    }
}

/// Whether the kernel lets other processes of the user examine this one.
fn dumpable() -> bool {
    // SAFETY: PR_GET_DUMPABLE takes no pointers and only reads a flag.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 1 }
}

/// The key to prove with, when this process needs and can make a proof:
/// it is non-dumpable and holds the device key.
fn key() -> Option<CallerKey> {
    if dumpable() {
        return None;
    }
    ARMED.lock().ok()?.clone()
}

/// Whether this process identifies itself to the store by proof. A refusal
/// is then worth one more attempt on a fresh connection, because a store
/// that restarted has forgotten the proof.
pub(crate) fn in_use() -> bool {
    key().is_some()
}

/// The executable this process names, and the proof for `challenge` on the
/// connection whose unique bus name is `caller`.
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

/// `prove_blocking` for an async connection, such as the one the desktop
/// portal serves on.
///
/// # Errors
///
/// The store's refusal, or a bus failure.
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
