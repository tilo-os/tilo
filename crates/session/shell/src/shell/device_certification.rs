//! Adding a device to the person from a device that holds none of their
//! identity's key, or only a share of it.
//!
//! Pairing finds the new device and the person checks its code there, as
//! always. The certificate it needs is then signed by the person's devices
//! that hold the key ([`tilo_identity_host::certification_ceremony`]): this
//! device proposes it to each of the person's other devices over their
//! private fabric ([`certify`]); a device holding key material shows the
//! person a notification naming the new device, the device that found it and
//! its code, as for any device asking to be them, and signs only when they
//! approve it there ([`answer`]); devices without key material say so at
//! once. A revoked device is refused by every key holder before anyone is
//! asked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

use tilo_identity_host::certification_ceremony::{
    self as ceremony, Answer, Ask, Gathering, IdentityKey, Progress, Reply, Signer, Step,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use crate::shell::device_fabric::{PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;

/// How long the person has to approve on a device that holds the key. The
/// new device waits 270 seconds for its pairing; the exchange after the
/// approval needs part of that.
pub(crate) const APPROVAL_WINDOW: Duration = Duration::from_secs(200);

/// This device's material of the person's identity key.
pub(crate) trait Keys: Send + Sync {
    /// The person's key as this device holds it.
    ///
    /// # Errors
    ///
    /// When the person's identity cannot be opened here.
    fn key(&self, person: [u8; 32]) -> Result<Box<dyn IdentityKey>, String>;
    /// Whether `device` was removed from `person`.
    fn revoked(&self, person: [u8; 32], device: &[u8; 32]) -> bool;
}

/// [`Keys`] from the session's identity store.
struct StoreKeys;

impl Keys for StoreKeys {
    fn key(&self, person: [u8; 32]) -> Result<Box<dyn IdentityKey>, String> {
        let key = crate::shell::identity_admin::fleet_store()?
            .identity_key(&hex::encode(person))
            .map_err(|error| error.to_string())?;
        Ok(Box::new(key))
    }

    fn revoked(&self, person: [u8; 32], device: &[u8; 32]) -> bool {
        crate::shell::identity_admin::fleet_store()
            .ok()
            .and_then(|store| store.load(&hex::encode(person)).ok())
            .is_some_and(|fleet| fleet.is_revoked(device))
    }
}

/// A device another of the person's devices found, waiting for the person
/// here.
#[derive(Clone, Debug)]
pub(crate) struct Asking {
    /// Names it in the notification's answer.
    pub digest: [u8; 32],
    /// What the new device calls itself.
    pub label: String,
    /// The code the new device shows.
    pub code: String,
    /// The person's device that found it.
    pub via: String,
    pub asked_at_ms: u64,
    from: DeviceId,
    ask: Ask,
}

impl Asking {
    /// The new device's key.
    pub(crate) const fn device(&self) -> [u8; 32] {
        self.ask.cert.device_public_key
    }
}

/// One device's part in certifications.
#[derive(Default)]
struct State {
    asking: Vec<Asking>,
    signer: Signer,
    /// The certifications this device runs, by session.
    gatherings: HashMap<[u8; 16], mpsc::Sender<([u8; 32], Answer)>>,
    /// Answers waiting for the loop to send them.
    outbox: Vec<(DeviceId, Answer)>,
    keys: Option<Arc<dyn Keys>>,
}

/// Each device's [`State`], by the device: one session runs one, a test
/// several.
static STATES: LazyLock<Mutex<HashMap<DeviceId, State>>> = LazyLock::new(Mutex::default);
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn with_state<T>(local: &DeviceId, change: impl FnOnce(&mut State) -> T) -> T {
    change(STATES.lock_recover().entry(local.clone()).or_default())
}

fn keys(local: &DeviceId) -> Arc<dyn Keys> {
    with_state(local, |state| state.keys.clone()).unwrap_or_else(|| Arc::new(StoreKeys))
}

/// Stand `keys` in for the identity store of the device `local`.
#[cfg(test)]
pub(crate) fn set_keys(local: &DeviceId, keys: Arc<dyn Keys>) {
    with_state(local, |state| state.keys = Some(keys));
}

/// The session's network, once it runs: pairing certifies through it.
static SESSION_NETWORK: std::sync::RwLock<Option<std::sync::Weak<ShellNetwork>>> =
    std::sync::RwLock::new(None);

/// Let pairing certify through `network`, the session's network.
pub fn attach_network(network: &Arc<ShellNetwork>) {
    use tilo_lock::RwLockRecover as _;
    *SESSION_NETWORK.write_recover() = Some(Arc::downgrade(network));
}

/// The session's network, while it runs.
pub(crate) fn session_network() -> Option<Arc<ShellNetwork>> {
    use tilo_lock::RwLockRecover as _;
    SESSION_NETWORK
        .read_recover()
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
}

fn local(network: &ShellNetwork) -> Result<DeviceId, String> {
    network
        .personal_device_id()
        .ok_or_else(|| "this device has no identity yet".to_string())
}

fn device_key(device: &DeviceId) -> Option<[u8; 32]> {
    hex::decode(device.as_str()).ok()?.try_into().ok()
}

fn device_id(key: [u8; 32]) -> Option<DeviceId> {
    DeviceId::new(hex::encode(key)).ok()
}

fn send(network: &ShellNetwork, to: &DeviceId, action: PersonalFabricAction) -> Result<(), String> {
    network.send_personal_fabric_action(to, action).map(drop)
}

fn ask_action(ask: &Ask) -> Result<PersonalFabricAction, String> {
    Ok(PersonalFabricAction::CertifyAsk {
        ask: postcard::to_allocvec(ask).map_err(|error| error.to_string())?,
    })
}

fn reply(network: &ShellNetwork, to: &DeviceId, answer: &Answer) -> Result<(), String> {
    send(
        network,
        to,
        PersonalFabricAction::CertifyAnswer {
            answer: postcard::to_allocvec(answer).map_err(|error| error.to_string())?,
        },
    )
}

/// Send the answers this device worked out off the loop.
pub(crate) fn pump(network: &ShellNetwork) {
    let Ok(local) = local(network) else {
        return;
    };
    let outbox = with_state(&local, |state| std::mem::take(&mut state.outbox));
    for (to, answer) in outbox {
        if let Err(error) = reply(network, &to, &answer) {
            tracing::warn!(%error, "could not answer a device certification ask");
        }
    }
}

/// Take a certification message of the person's fabric; `false` for any
/// other action.
pub(crate) fn handle_action(
    network: &ShellNetwork,
    envelope: &PersonalFabricActionEnvelope,
) -> bool {
    match &envelope.action {
        PersonalFabricAction::CertifyAsk { ask } => {
            let (Some(person), Ok(local), Ok(ask)) = (
                network.personal_user_public_key(),
                local(network),
                postcard::from_bytes::<Ask>(ask),
            ) else {
                return true;
            };
            let from = envelope.source.clone();
            let via = device_key(&from)
                .and_then(|key| {
                    network
                        .authenticated_devices_of(&person)
                        .into_iter()
                        .find(|cert| cert.device_public_key == key)
                })
                .map_or_else(
                    || from.as_str().chars().take(8).collect(),
                    |cert| cert.device_label,
                );
            let wake = network.loop_waker();
            // The identity store may wait on a secret service: off the loop.
            let spawned = std::thread::Builder::new()
                .name("tilo-device-certification".into())
                .spawn(move || {
                    receive(&local, person, from, via, ask);
                    wake();
                });
            if let Err(error) = spawned {
                tracing::warn!(%error, "could not take a device certification ask");
            }
            true
        }
        PersonalFabricAction::CertifyAnswer { answer } => {
            if let (Some(from), Ok(local), Ok(answer)) = (
                device_key(&envelope.source),
                local(network),
                postcard::from_bytes::<Answer>(answer),
            ) && let Some(gathering) = with_state(&local, |state| {
                state.gatherings.get(&answer.session).cloned()
            }) {
                let _ = gathering.send((from, answer));
            }
            true
        }
        _ => false,
    }
}

/// Take `ask` from the person's device `from` at the device `local`: queue
/// it for the person, or answer it.
fn receive(local: &DeviceId, person: [u8; 32], from: DeviceId, via: String, ask: Ask) {
    let keys = keys(local);
    let session = ask.session;
    let answer = match &ask.step {
        Step::Approve => match keys.key(person) {
            Ok(key) if key.holds_key() => {
                let now = tilo_clock::unix_now_ns();
                match ceremony::check_proposal(
                    &ask,
                    person,
                    &|device| keys.revoked(person, device),
                    now,
                ) {
                    Ok(()) => return queue(local, from, via, ask),
                    Err(reason) => Reply::Refused(reason),
                }
            }
            _ => Reply::HoldsNoKey,
        },
        Step::Sign { .. } => match keys.key(person) {
            Ok(key) => with_state(local, |state| {
                state
                    .signer
                    .sign(key.as_ref(), &ask, tilo_clock::unix_now_ns())
            }),
            Err(error) => Reply::Refused(error),
        },
    };
    with_state(local, |state| {
        state.outbox.push((
            from,
            Answer {
                session,
                reply: answer,
            },
        ));
    });
}

fn queue(local: &DeviceId, from: DeviceId, via: String, ask: Ask) {
    let mut digest = blake3::Hasher::new();
    digest.update(&ask.session);
    digest.update(&ask.digest().unwrap_or_default());
    let asking = Asking {
        digest: *digest.finalize().as_bytes(),
        label: ask.cert.device_label.clone(),
        code: ask.code(),
        via,
        asked_at_ms: tilo_clock::unix_now_ms(),
        from,
        ask,
    };
    let expired = expired_before();
    with_state(local, |state| {
        state
            .asking
            .retain(|other| other.digest != asking.digest && other.asked_at_ms > expired);
        state.asking.push(asking);
    });
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Asks older than this, Unix-epoch milliseconds, can no longer be answered.
fn expired_before() -> u64 {
    tilo_clock::unix_now_ms()
        .saturating_sub(u64::try_from(APPROVAL_WINDOW.as_millis()).unwrap_or(u64::MAX))
}

/// The devices waiting for the person at this device, oldest first.
pub(crate) fn asking(network: &ShellNetwork) -> Vec<Asking> {
    let Ok(local) = local(network) else {
        return Vec::new();
    };
    let expired = expired_before();
    with_state(&local, |state| {
        state.asking.retain(|asking| asking.asked_at_ms > expired);
        state.asking.clone()
    })
}

/// Changes whenever another device of the person asks about a device here.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// The person approved (`approve`) or declined the device `digest` names
/// here. `None` when no device of the person asks under `digest`; else the
/// device's label. Blocks on the identity store.
///
/// # Errors
///
/// When this device's key would not sign, or the answer cannot be sent.
pub(crate) fn answer(
    network: &ShellNetwork,
    digest: [u8; 32],
    approve: bool,
) -> Result<Option<String>, String> {
    let local = local(network)?;
    let Some(asking) = with_state(&local, |state| {
        let index = state
            .asking
            .iter()
            .position(|asking| asking.digest == digest)?;
        Some(state.asking.remove(index))
    }) else {
        return Ok(None);
    };
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let person = asking.ask.cert.user_public_key;
    let answer = if approve {
        let key = keys(&local).key(person)?;
        with_state(&local, |state| {
            state
                .signer
                .approve(key.as_ref(), &asking.ask, true, tilo_clock::unix_now_ns())
        })
    } else {
        Reply::Declined
    };
    let refused = match &answer {
        Reply::Refused(reason) => Some(reason.clone()),
        _ => None,
    };
    reply(
        network,
        &asking.from,
        &Answer {
            session: asking.ask.session,
            reply: answer,
        },
    )?;
    refused.map_or(Ok(Some(asking.label)), Err)
}

/// Removes a certification's answer route when it ends.
struct Registered(DeviceId, [u8; 16]);

impl Drop for Registered {
    fn drop(&mut self) {
        with_state(&self.0, |state| state.gatherings.remove(&self.1));
    }
}

/// Have the person's devices that hold their identity's key certify `cert`
/// (unsigned) for the new device that announced `service`; `threshold` of
/// the key's `total` devices must sign. The person already confirmed the
/// device here. Blocks until it is certified, declined, or
/// [`APPROVAL_WINDOW`] passes.
///
/// # Errors
///
/// What is still needed, plainly, when it was not certified.
pub(crate) fn certify(
    network: &ShellNetwork,
    cert: DeviceCert,
    service: [u8; 32],
    threshold: u16,
    total: u16,
) -> Result<DeviceCert, String> {
    let mut session = [0; 16];
    getrandom::fill(&mut session).map_err(|error| error.to_string())?;
    let mut gathering = Gathering::new(session, cert, service, threshold, total);
    let person = gathering.ask().cert.user_public_key;
    let here = local(network)?;
    let here_key = device_key(&here).ok_or("this device has a malformed key")?;
    let (answers, arrived) = mpsc::channel();
    with_state(&here, |state| {
        state.gatherings.insert(session, answers.clone())
    });
    let _registered = Registered(here.clone(), session);
    // A share held here joins in: the person confirmed the device here.
    let share = keys(&here).key(person).ok().filter(|key| key.holds_key());
    let mut asked = 0_usize;
    for device in network.personal_route_devices() {
        match send(network, &device, ask_action(gathering.ask())?) {
            Ok(()) => asked = asked.saturating_add(1),
            Err(error) => tracing::debug!(%error, "a device of the person could not be asked"),
        }
    }
    if let Some(key) = &share {
        let now = tilo_clock::unix_now_ns();
        let reply = with_state(&here, |state| {
            state
                .signer
                .approve(key.as_ref(), gathering.ask(), true, now)
        });
        let _ = answers.send((here_key, Answer { session, reply }));
    } else if asked == 0 {
        return Err(
            "none of your other devices is online to approve it: turn on the device you set \
             up first, then pair again"
                .into(),
        );
    }
    let deadline = Instant::now()
        .checked_add(APPROVAL_WINDOW)
        .ok_or("the approval window does not fit this clock")?;
    let mut without_key = 0_usize;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok((from, answer)) = arrived.recv_timeout(left) else {
            return Err(format!(
                "It was not approved in time. {}",
                gathering.status()
            ));
        };
        if matches!(answer.reply, Reply::HoldsNoKey) {
            without_key = without_key.saturating_add(1);
        }
        match gathering.answer(from, answer) {
            Progress::Certified(cert) => return Ok(cert),
            Progress::Waiting if share.is_none() && without_key >= asked => {
                return Err(
                    "none of your devices online now can approve it: turn on the device you \
                     set up first, then pair again"
                        .into(),
                );
            }
            Progress::Waiting => {}
            Progress::Failed(reason) => return Err(reason),
            Progress::Send(rounds) => {
                for (to, ask) in rounds {
                    if let Some(key) = share.as_ref().filter(|_| to == here_key) {
                        let now = tilo_clock::unix_now_ns();
                        let reply =
                            with_state(&here, |state| state.signer.sign(key.as_ref(), &ask, now));
                        let _ = answers.send((to, Answer { session, reply }));
                        continue;
                    }
                    let to = device_id(to).ok_or("a device answered under a malformed key")?;
                    send(network, &to, ask_action(&ask)?)?;
                }
            }
        }
    }
}
