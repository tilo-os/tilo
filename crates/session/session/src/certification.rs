//! Adding a device to the person from a device that holds none of their identity's key, or only a
//! share of it.
//! Pairing finds the new device and the person checks its code there, as always. The certificate it
//! needs is then signed by the person's devices that hold the key
//! ([`tilo_identity_host::certification_ceremony`]): this device proposes it to each of the
//! person's other devices over their private fabric ([`certify`]); a device holding key material
//! shows the person a notification naming the new device, the device that found it and its code, as
//! for any device asking to be them, and signs only when they approve it there ([`answer`], from
//! the notification's own buttons); devices without key material say so at once. A revoked device
//! is refused by every key holder before anyone is asked.
//!
//! A new device that opened the pairing link this device shows ([`crate::pairing_link`]) asks at
//! the host the link names instead. While the link works this device waits there for it
//! ([`watch_host`]), on its own connection as the person, not on any workspace's: the device joins
//! the person, whichever workspace the link opened. It is put to the person in the same list and
//! notification, and approving it has the person's key sign its certificate, which the host hands
//! the new device.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::platform::fabric::{DeviceId, PersonalFabricAction, PersonalFabricActionEnvelope};
use crate::platform::fabric::{FabricLink, SharedLink};
use tilo_identity_host::certification_ceremony::{
    self as ceremony, Answer, Ask, Gathering, IdentityKey, Progress, Reply, Signer, Step,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

/// How long the person has to approve on a device that holds the key. The new device waits 270
/// seconds for its pairing; the exchange after the approval needs part of that.
pub(crate) const APPROVAL_WINDOW: Duration = Duration::from_secs(200);

/// The host action id prefix a device asking to be the person is answered under:
/// `<prefix><digest>:approve` or `<prefix><digest>:decline` , which the session's loop hands back
/// from the notification's buttons.
const ANSWER_PREFIX: &str = "device-answer:";

/// This device's material of the person's identity key.
pub(crate) trait Keys: Send + Sync {
    /// The person's key as this device holds it.
    ///
    /// # Errors
    ///
    /// When the person's identity cannot be opened here.
    fn key(&self, person: [u8; 32]) -> Result<Box<dyn IdentityKey + Send + Sync>, String>;
    /// Whether `device` was removed from `person` .
    fn revoked(&self, person: [u8; 32], device: &[u8; 32]) -> bool;
    /// The certificate this device's fleet holds for `device` under `person` , when it holds one:
    /// what a renewal replaces is checked against it, never against the asker's word alone.
    fn held(&self, person: [u8; 32], device: &[u8; 32]) -> Option<tilo_user_identity::DeviceCert>;
}

/// [`Keys`] from the session's identity store.
struct StoreKeys;

impl Keys for StoreKeys {
    fn key(&self, person: [u8; 32]) -> Result<Box<dyn IdentityKey + Send + Sync>, String> {
        let key = crate::identity_admin::fleet_store()?
            .identity_key(&hex::encode(person))
            .map_err(|error| error.to_string())?;
        Ok(Box::new(key))
    }

    fn revoked(&self, person: [u8; 32], device: &[u8; 32]) -> bool {
        crate::identity_admin::fleet_store()
            .ok()
            .and_then(|store| store.load(&hex::encode(person)).ok())
            .is_some_and(|fleet| fleet.is_revoked(device))
    }

    fn held(&self, person: [u8; 32], device: &[u8; 32]) -> Option<tilo_user_identity::DeviceCert> {
        crate::identity_admin::fleet_store()
            .ok()
            .and_then(|store| store.load(&hex::encode(person)).ok())
            .and_then(|fleet| {
                fleet
                    .certificates
                    .into_iter()
                    .find(|cert| &cert.device_public_key == device)
            })
    }
}

/// A device another of the person's devices found, waiting for the person here.
#[derive(Clone, Debug)]
pub(crate) struct Asking {
    /// Names it in the notification's answer.
    pub(crate) digest: [u8; 32],
    /// What the new device calls itself.
    pub(crate) label: String,
    /// The code the new device shows.
    pub(crate) code: String,
    /// The person's device that found it.
    pub(crate) via: String,
    pub(crate) asked_at_ms: u64,
    route: Route,
}

/// Where a device asking to be the person asked, and so where its answer goes.
#[derive(Clone, Debug)]
enum Route {
    /// Another of the person's devices found it and proposed its certificate here.
    Device { from: DeviceId, ask: Box<Ask> },
    /// It opened this device's pairing link, and waits at the host the link names.
    Host {
        origin: String,
        host: tilo_relay_client::Host,
        person: [u8; 32],
        device: [u8; 32],
    },
}

impl Asking {
    /// The new device's key.
    pub(crate) const fn device(&self) -> [u8; 32] {
        match &self.route {
            Route::Device { ask, .. } => ask.cert.device_public_key,
            Route::Host { device, .. } => *device,
        }
    }
}

/// A host this device waits at for the new devices its pairing link sends there.
struct Watch {
    /// Until when, Unix-epoch milliseconds: when the link shown runs out.
    until_ms: u64,
    /// Whether it still waits; a wait that failed is not started again for the same link.
    running: bool,
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
    /// The asks the person was shown, by digest: a new one is told, one that went is withdrawn.
    shown: Vec<[u8; 32]>,
    /// The hosts this device waits at for a device its pairing link sent, by origin.
    watching: HashMap<String, Watch>,
}

/// Each device's [`State`], by the device: one session runs one, a test several.
static STATES: LazyLock<Mutex<HashMap<DeviceId, State>>> = LazyLock::new(Mutex::default);

fn with_state<T>(local: &DeviceId, change: impl FnOnce(&mut State) -> T) -> T {
    change(STATES.lock_recover().entry(local.clone()).or_default())
}

fn keys(local: &DeviceId) -> Arc<dyn Keys> {
    with_state(local, |state| state.keys.clone()).unwrap_or_else(|| Arc::new(StoreKeys))
}

/// Stand `keys` in for the identity store of the device `local` .
#[cfg(test)]
pub(crate) fn set_keys(local: &DeviceId, keys: Arc<dyn Keys>) {
    with_state(local, |state| state.keys = Some(keys));
}

/// What wakes the session's loop when another device's ask landed here.
static WAKE: std::sync::RwLock<Option<tilo_service::Wake>> = std::sync::RwLock::new(None);

/// Let what lands here wake `wake` , the session's loop.
pub(crate) fn attach(wake: tilo_service::Wake) {
    use tilo_lock::RwLockRecover as _;
    *WAKE.write_recover() = Some(wake);
}

fn wake() {
    use tilo_lock::RwLockRecover as _;
    let wake = WAKE.read_recover().clone();
    if let Some(wake) = wake {
        wake();
    }
}

fn local(fabric: &dyn FabricLink) -> Result<DeviceId, String> {
    fabric
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| "this device has no identity yet".to_string())
}

fn device_key(device: &DeviceId) -> Option<[u8; 32]> {
    hex::decode(device.as_str()).ok()?.try_into().ok()
}

fn device_id(key: [u8; 32]) -> Option<DeviceId> {
    DeviceId::new(hex::encode(key)).ok()
}

fn send(
    fabric: &dyn FabricLink,
    to: &DeviceId,
    action: PersonalFabricAction,
) -> Result<(), String> {
    fabric
        .try_send_personal_fabric_action(to, action)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn ask_action(ask: &Ask) -> Result<PersonalFabricAction, String> {
    Ok(PersonalFabricAction::CertifyAsk {
        ask: postcard::to_allocvec(ask).map_err(|error| error.to_string())?,
    })
}

fn reply(fabric: &dyn FabricLink, to: &DeviceId, answer: &Answer) -> Result<(), String> {
    send(
        fabric,
        to,
        PersonalFabricAction::CertifyAnswer {
            answer: postcard::to_allocvec(answer).map_err(|error| error.to_string())?,
        },
    )
}

/// Send the answers this device worked out off the loop.
pub(crate) fn pump(fabric: &dyn FabricLink) {
    let Ok(local) = local(fabric) else {
        return;
    };
    let outbox = with_state(&local, |state| std::mem::take(&mut state.outbox));
    for (to, answer) in outbox {
        if let Err(error) = reply(fabric, &to, &answer) {
            tracing::warn!(%error, "could not answer a device certification ask");
        }
    }
}

/// Take a certification message of the person's fabric; `false` for any other action.
pub(crate) fn handle_action(fabric: &SharedLink, envelope: &PersonalFabricActionEnvelope) -> bool {
    match &envelope.action {
        PersonalFabricAction::CertifyAsk { ask } => {
            let (Ok(local), Ok(ask)) = (local(fabric.as_ref()), postcard::from_bytes::<Ask>(ask))
            else {
                return true;
            };
            let from = envelope.source.clone();
            let person = ask.cert.user_public_key;
            let via = fabric.device_fabric().snapshots().get(&from).map_or_else(
                || from.as_str().chars().take(8).collect(),
                |snapshot| snapshot.presence.label.clone(),
            );
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
                local(fabric.as_ref()),
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

/// Take `ask` from the person's device `from` at the device `local` : queue it for the person, or
/// answer it.
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
                    Ok(()) => {
                        queue(local, from, via, ask);
                        return;
                    }
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
        // A renewal adds no device and grants nothing new, so the person is not asked: the device
        // is already theirs. A key holder checks it is exactly that — the same device key, under
        // the same root, issued later, wider in nothing, and not revoked — and that the certificate
        // it replaces is the one the fleet holds for the device, then signs it.
        Step::Renew { held } => match keys.key(person) {
            Ok(key) if key.holds_key() => {
                let current = keys.held(person, &ask.cert.device_public_key);
                match ceremony::check_renewal(&ask, held, current.as_ref(), person, &|device| {
                    keys.revoked(person, device)
                }) {
                    Ok(()) => with_state(local, |state| {
                        state
                            .signer
                            .renew(key.as_ref(), &ask, tilo_clock::unix_now_ns())
                    }),
                    Err(reason) => Reply::Refused(reason),
                }
            }
            Ok(_) => Reply::HoldsNoKey,
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
        route: Route::Device {
            from,
            ask: Box::new(ask),
        },
    };
    let expired = expired_before();
    with_state(local, |state| {
        state
            .asking
            .retain(|other| other.digest != asking.digest && other.asked_at_ms > expired);
        state.asking.push(asking);
    });
}

/// Asks older than this, Unix-epoch milliseconds, can no longer be answered.
fn expired_before() -> u64 {
    tilo_clock::unix_now_ms()
        .saturating_sub(u64::try_from(APPROVAL_WINDOW.as_millis()).unwrap_or(u64::MAX))
}

/// The devices waiting for the person at this device, oldest first.
pub(crate) fn asking(fabric: &dyn FabricLink) -> Vec<Asking> {
    let Ok(local) = local(fabric) else {
        return Vec::new();
    };
    let expired = expired_before();
    with_state(&local, |state| {
        state.asking.retain(|asking| asking.asked_at_ms > expired);
        state.asking.clone()
    })
}

/// The asks the person was not shown yet, as the loop offers them: each is told once, and one that
/// went is withdrawn.
pub(crate) fn newly_asking(fabric: &dyn FabricLink) -> (Vec<Asking>, Vec<[u8; 32]>) {
    let Ok(local) = local(fabric) else {
        return (Vec::new(), Vec::new());
    };
    let asking = asking(fabric);
    let digests: Vec<[u8; 32]> = asking.iter().map(|asking| asking.digest).collect();
    with_state(&local, |state| {
        let fresh: Vec<Asking> = asking
            .into_iter()
            .filter(|asking| !state.shown.contains(&asking.digest))
            .collect();
        let gone: Vec<[u8; 32]> = state
            .shown
            .iter()
            .copied()
            .filter(|digest| !digests.contains(digest))
            .collect();
        state.shown.clone_from(&digests);
        (fresh, gone)
    })
}

/// The person approved (`approve`) or declined the device `digest` names
/// here. `None` when no device of the person asks under `digest`; else the
/// device's label. Blocks on the identity store.
///
/// # Errors
///
/// When this device's key would not sign, or the answer cannot be sent.
fn answer(
    fabric: &dyn FabricLink,
    digest: [u8; 32],
    approve: bool,
) -> Result<Option<String>, String> {
    let local = local(fabric)?;
    let Some(asking) = with_state(&local, |state| {
        let index = state
            .asking
            .iter()
            .position(|asking| asking.digest == digest)?;
        Some(state.asking.remove(index))
    }) else {
        return Ok(None);
    };
    let (from, ask) = match asking.route {
        Route::Device { from, ask } => (from, ask),
        Route::Host { host, person, .. } => {
            answer_at_host(&local, host, person, digest, approve)?;
            return Ok(Some(asking.label));
        }
    };
    let person = ask.cert.user_public_key;
    let answer = if approve {
        let key = keys(&local).key(person)?;
        with_state(&local, |state| {
            state
                .signer
                .approve(key.as_ref(), &ask, true, tilo_clock::unix_now_ns())
        })
    } else {
        Reply::Declined
    };
    let refused = match &answer {
        Reply::Refused(reason) => Some(reason.clone()),
        _ => None,
    };
    reply(
        fabric,
        &from,
        &Answer {
            session: ask.session,
            reply: answer,
        },
    )?;
    refused.map_or(Ok(Some(asking.label)), Err)
}

/// Answer the device asking under `digest` at `host`: approved, the person's key certifies it there.
/// Blocks on the host.
fn answer_at_host(
    local: &DeviceId,
    host: tilo_relay_client::Host,
    person: [u8; 32],
    digest: [u8; 32],
    approve: bool,
) -> Result<(), String> {
    let signer = ceremony::PersonSigner::new(keys(local).key(person)?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let approver = tilo_relay_client::MemberApprover::new(host);
        let answered = tilo_relay_client::answer_pairing(&approver, &signer, digest, approve).await;
        approver.close().await;
        answered.map(drop).map_err(|error| error.to_string())
    })
}

/// Wait at the host at `origin` for the new devices asking to be `person` there, until `until_ms`:
/// the pairing link this device shows names that host, and a device that opens it asks there. Each
/// is put to the person as any device asking to be them ([`asking`]), and answered there
/// ([`answer`]). Asking again while it waits only moves when it stops; a wait that failed is not
/// started again until a new link is shown.
pub(crate) fn watch_host(fabric: &dyn FabricLink, origin: &str, person: [u8; 32], until_ms: u64) {
    let Ok(local) = local(fabric) else {
        return;
    };
    let start = with_state(&local, |state| match state.watching.get_mut(origin) {
        Some(watch) if watch.running => {
            watch.until_ms = watch.until_ms.max(until_ms);
            false
        }
        Some(watch) if watch.until_ms >= until_ms => false,
        _ => {
            state.watching.insert(
                origin.to_owned(),
                Watch {
                    until_ms,
                    running: true,
                },
            );
            true
        }
    });
    if !start {
        return;
    }
    let origin = origin.to_owned();
    let spawned = std::thread::Builder::new()
        .name("tilo-pairing-link".into())
        .spawn(move || {
            if let Err(error) = wait_at_host(&local, &origin, person) {
                tracing::warn!(%error, %origin, "this device stopped waiting for a new device");
            }
            with_state(&local, |state| {
                if let Some(watch) = state.watching.get_mut(&origin) {
                    watch.running = false;
                }
            });
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not wait for a new device");
    }
}

/// [`watch_host`]'s wait, until the link runs out: each time the devices waiting at the host
/// change, the list here follows them.
fn wait_at_host(local: &DeviceId, origin: &str, person: [u8; 32]) -> Result<(), String> {
    let signer = ceremony::PersonSigner::new(keys(local).key(person)?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = tilo_workspace_directory::client()?;
        let info = tilo_relay_client::https::host_info(&tilo_relay_client::https::Origin::new(
            &client, origin,
        ))
        .await
        .map_err(|error| error.to_string())?;
        let address = info.address.ok_or("the host takes no device connections")?;
        let host = tilo_relay_client::Host::published(&address);
        let approver = tilo_relay_client::MemberApprover::new(host.clone());
        let mut known = Vec::new();
        let waited = loop {
            let until_ms = with_state(local, |state| {
                state.watching.get(origin).map_or(0, |watch| watch.until_ms)
            });
            if tilo_clock::unix_now_ms() >= until_ms {
                break Ok(());
            }
            let pending =
                match tilo_relay_client::next_pairings(&approver, &signer, known.clone()).await {
                    Ok(pending) => pending,
                    Err(error) => break Err(error.to_string()),
                };
            known = pending.iter().map(|entry| entry.request_digest).collect();
            follow_host(local, origin, &host, person, pending);
            wake();
        };
        approver.close().await;
        waited
    })
}

/// Put the devices `pending` at the host at `origin` to the person, in place of those that asked
/// there before: one answered elsewhere, or gone, is no longer asked here.
fn follow_host(
    local: &DeviceId,
    origin: &str,
    host: &tilo_relay_client::Host,
    person: [u8; 32],
    pending: Vec<tilo_relay_protocol::certify::PendingCertification>,
) {
    let expired = expired_before();
    with_state(local, |state| {
        let digests: Vec<[u8; 32]> = pending.iter().map(|entry| entry.request_digest).collect();
        state.asking.retain(|asking| match &asking.route {
            Route::Host { origin: at, .. } => at != origin || digests.contains(&asking.digest),
            Route::Device { .. } => true,
        });
        for entry in pending {
            // Asked when the host queued it: one the person did not answer in time stays gone.
            let asked_at_ms = entry.requested_at_ns / 1_000_000;
            if entry.request.user_public_key != person
                || asked_at_ms <= expired
                || state
                    .asking
                    .iter()
                    .any(|asking| asking.digest == entry.request_digest)
            {
                continue;
            }
            state.asking.push(Asking {
                digest: entry.request_digest,
                label: entry.request.device_label.clone(),
                code: entry.pairing_code,
                via: String::new(),
                asked_at_ms,
                route: Route::Host {
                    origin: origin.to_owned(),
                    host: host.clone(),
                    person,
                    device: entry.request.device_public_key,
                },
            });
        }
    });
}

/// The banner button that answers the device asking under `digest` here: approving lets it act as
/// the person, declining refuses it ([`host_action`]).
pub(crate) fn answer_key(digest: [u8; 32], approve: bool) -> String {
    format!(
        "{ANSWER_PREFIX}{}:{}",
        hex::encode(digest),
        if approve { "approve" } else { "decline" }
    )
}

/// The person chose `choice` , one of the notification's buttons, if it is one of this device's:
/// answers the device it names there, off the loop.
pub(crate) fn host_action(fabric: &SharedLink, choice: &str) -> bool {
    let Some(rest) = choice.strip_prefix(ANSWER_PREFIX) else {
        return false;
    };
    let Some((digest, verdict)) = rest.rsplit_once(':') else {
        return false;
    };
    let (Ok(digest), Some(approve)) = (
        hex::decode(digest)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or("that is no device ask"),
        match verdict {
            "approve" => Some(true),
            "decline" => Some(false),
            _ => None,
        },
    ) else {
        return true;
    };
    let link = Arc::clone(fabric);
    let spawned = std::thread::Builder::new()
        .name("tilo-device-certification-answer".into())
        .spawn(move || {
            if let Err(error) = answer(link.as_ref(), digest, approve) {
                tracing::warn!(%error, "a device asking to be the person was not answered");
            }
            wake();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not answer a device asking to be the person");
    }
    true
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
    fabric: &SharedLink,
    cert: DeviceCert,
    service: [u8; 32],
    threshold: u16,
    total: u16,
) -> Result<DeviceCert, String> {
    let mut session = [0; 16];
    getrandom::fill(&mut session).map_err(|error| error.to_string())?;
    let gathering = Gathering::new(session, cert, service, threshold, total);
    gather(
        fabric,
        gathering,
        AskVerb::Approve,
        |local_device, key, ask, now| {
            with_state(local_device, |state| {
                state.signer.approve(key, ask, true, now)
            })
        },
    )
}

/// Renew this device's certificate `held` through the person's devices that
/// hold their identity's key: `cert` (unsigned) replaces `held` for the same
/// device, and `threshold` of the key's `total` devices sign it. Nobody is
/// asked to approve: a renewal adds no device and grants nothing new. Blocks
/// until it is signed or [`APPROVAL_WINDOW`] passes.
///
/// # Errors
///
/// What is still needed, plainly, when it was not signed.
pub(crate) fn renew(
    fabric: &SharedLink,
    cert: DeviceCert,
    held: DeviceCert,
    service: [u8; 32],
    threshold: u16,
    total: u16,
) -> Result<DeviceCert, String> {
    let mut session = [0; 16];
    getrandom::fill(&mut session).map_err(|error| error.to_string())?;
    let gathering = Gathering::renewal(session, cert, held, service, threshold, total);
    gather(
        fabric,
        gathering,
        AskVerb::Renew,
        |local_device, key, ask, now| {
            with_state(local_device, |state| state.signer.renew(key, ask, now))
        },
    )
}

/// How a gather ends in a person's words: what a certification asks them to approve, or a renewal
/// that needs no approval and only needs signing.
#[derive(Clone, Copy)]
enum AskVerb {
    Approve,
    Renew,
}

impl AskVerb {
    /// The word for what the person waits on, as the refusal uses it.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Renew => "sign",
        }
    }
}

/// Drive `gathering` to a signed certificate over the person's fabric. `here` answers its own
/// share, given this device's id: a certification the person just approved here, or a renewal
/// checked here.
fn gather(
    fabric: &SharedLink,
    mut gathering: Gathering,
    verb: AskVerb,
    here: impl Fn(&DeviceId, &dyn ceremony::IdentityKey, &Ask, u64) -> Reply,
) -> Result<DeviceCert, String> {
    let person = gathering.ask().cert.user_public_key;
    let session = gathering.ask().session;
    let local_device = local(fabric.as_ref())?;
    let here_key = device_key(&local_device).ok_or("this device has a malformed key")?;
    let (answers, arrived) = mpsc::channel();
    with_state(&local_device, |state| {
        state.gatherings.insert(session, answers.clone())
    });
    let _registered = Registered(local_device.clone(), session);
    // A share held here joins in.
    let share = keys(&local_device)
        .key(person)
        .ok()
        .filter(|key| key.holds_key());
    let mut asked = 0_usize;
    for device in fabric.online_device_ids() {
        if device == local_device {
            continue;
        }
        match send(fabric.as_ref(), &device, ask_action(gathering.ask())?) {
            Ok(()) => asked = asked.saturating_add(1),
            Err(error) => tracing::warn!(%error, "a device of the person could not be asked"),
        }
    }
    if let Some(key) = &share {
        let reply = here(
            &local_device,
            key.as_ref(),
            gathering.ask(),
            tilo_clock::unix_now_ns(),
        );
        let _ = answers.send((here_key, Answer { session, reply }));
    } else if asked == 0 {
        return Err(format!(
            "none of your other devices is online to {} it: turn on the device you set \
             up first, then try again",
            verb.as_str()
        ));
    }
    let deadline = Instant::now()
        .checked_add(APPROVAL_WINDOW)
        .ok_or("the approval window does not fit this clock")?;
    let mut without_key = 0_usize;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok((from, answer)) = arrived.recv_timeout(left) else {
            return Err(format!(
                "It was not {}d in time. {}",
                verb.as_str(),
                gathering.status()
            ));
        };
        if matches!(answer.reply, Reply::HoldsNoKey) {
            without_key = without_key.saturating_add(1);
        }
        match gathering.answer(from, answer) {
            Progress::Certified(cert) => return Ok(cert),
            Progress::Waiting if share.is_none() && without_key >= asked => {
                return Err(format!(
                    "none of your devices online now can {} it: turn on the device you \
                     set up first, then try again",
                    verb.as_str()
                ));
            }
            Progress::Waiting => {}
            Progress::Failed(reason) => return Err(reason),
            Progress::Send(rounds) => {
                for (to, ask) in rounds {
                    if let Some(key) = share.as_ref().filter(|_| to == here_key) {
                        let now = tilo_clock::unix_now_ns();
                        let reply = with_state(&local_device, |state| {
                            state.signer.sign(key.as_ref(), &ask, now)
                        });
                        let _ = answers.send((to, Answer { session, reply }));
                        continue;
                    }
                    let to = device_id(to).ok_or("a device answered under a malformed key")?;
                    send(fabric.as_ref(), &to, ask_action(&ask)?)?;
                }
            }
        }
    }
}

/// What a session with no fabric says when a device holding a share of the person's key would have
/// the others approve: they are not reachable.
pub(crate) fn cannot_certify_here() -> String {
    "this session runs no device fabric, so your other devices cannot approve it".into()
}

/// Answer a device asking to be the person from the `answer` step, which is the notification's own
/// path: the digest it lists, and the person's verdict. Blocks on the identity store.
pub(crate) fn answer_step(
    fabric: Option<&SharedLink>,
    ask: &str,
    approve: bool,
) -> Result<serde_json::Value, String> {
    let digest: [u8; 32] = hex::decode(ask.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("ask must be 64 hex characters")?;
    let fabric = fabric.ok_or_else(|| "the session runs no device fabric".to_string())?;
    let label =
        answer(fabric.as_ref(), digest, approve)?.ok_or("no device asks under that any more")?;
    Ok(serde_json::json!({ "ask": hex::encode(digest), "label": label, "approved": approve }))
}

/// The devices another of the person's devices found, asking at this device to be the person, as
/// the `asking` step lists them.
pub(crate) fn asking_step(fabric: Option<&SharedLink>) -> serde_json::Value {
    let asking: Vec<serde_json::Value> = fabric.map_or_else(Vec::new, |fabric| {
        asking(fabric.as_ref())
            .into_iter()
            .map(|asking| {
                serde_json::json!({
                    "ask": hex::encode(asking.digest),
                    "device": hex::encode(asking.device()),
                    "label": asking.label,
                    "code": asking.code,
                    "via": asking.via,
                    "asked_at_ms": asking.asked_at_ms,
                })
            })
            .collect()
    });
    serde_json::json!({ "asking": asking })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fabric::FabricRuntime;
    use crate::platform::fabric::PersonalFabricSendError;
    use crate::platform::fabric::{DeviceFabric, DeviceId};
    use iroh::EndpointId;
    use std::collections::{BTreeSet, HashMap};
    use threshold_identity::{SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason};

    /// The person's identity as first boot provisions it, in files.
    fn identity() -> (tempfile::TempDir, runtime_identity::SigningIdentity) {
        let root = tempfile::tempdir().expect("temp root");
        let dir = root.path().join("config/tilo/identity");
        let device = tilo_user_identity::IdentityPaths::under(root.path().join("system"));
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &dir,
            &device,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&dir)
            .load()
            .expect("registry");
        let id = registry.default_identity.expect("default identity");
        let key = tilo_identity_host::device_fleet::DeviceFleetStore::at(&dir, device)
            .identity_key(&id)
            .expect("the person's key");
        (root, key)
    }

    /// A device holding the person's key, or, with `revoked` , one the person removed.
    #[derive(Clone)]
    struct Held {
        key: Arc<runtime_identity::SigningIdentity>,
        revoked: bool,
    }

    impl ceremony::IdentityKey for Held {
        fn person(&self) -> [u8; 32] {
            self.key.person()
        }

        fn holds_key(&self) -> bool {
            self.key.holds_key()
        }

        fn sign_whole(&self, message: &[u8]) -> Result<Option<[u8; 64]>, String> {
            self.key.sign_whole(message)
        }

        fn commit(&self) -> Option<(SigningRound1Commitment, SigningSessionNonces)> {
            self.key.commit()
        }

        fn partial(
            &self,
            nonces: SigningSessionNonces,
            digest: &[u8; 32],
            commitments: &[SigningRound1Commitment],
        ) -> Result<SigningRound2Partial, String> {
            self.key.partial(nonces, digest, commitments)
        }
    }

    impl Keys for Held {
        fn key(
            &self,
            _person: [u8; 32],
        ) -> Result<Box<dyn ceremony::IdentityKey + Send + Sync>, String> {
            Ok(Box::new(self.clone()))
        }

        fn revoked(&self, _person: [u8; 32], _device: &[u8; 32]) -> bool {
            self.revoked
        }

        fn held(
            &self,
            _person: [u8; 32],
            _device: &[u8; 32],
        ) -> Option<tilo_user_identity::DeviceCert> {
            None
        }
    }

    /// A device holding none of the person's key.
    struct NoKey;

    impl Keys for NoKey {
        fn key(
            &self,
            _person: [u8; 32],
        ) -> Result<Box<dyn ceremony::IdentityKey + Send + Sync>, String> {
            Err("this device holds nothing of your identity".into())
        }

        fn revoked(&self, _person: [u8; 32], _device: &[u8; 32]) -> bool {
            false
        }

        fn held(
            &self,
            _person: [u8; 32],
            _device: &[u8; 32],
        ) -> Option<tilo_user_identity::DeviceCert> {
            None
        }
    }

    /// Two of the person's devices on one fabric, with the wire between them handed straight to the
    /// other's [`handle_action`].
    struct Wire {
        links: Mutex<HashMap<String, SharedLink>>,
        fabric: DeviceFabric,
        online: BTreeSet<DeviceId>,
    }

    struct Peer {
        device: DeviceId,
        label: &'static str,
        wire: Arc<Wire>,
    }

    impl FabricLink for Peer {
        fn device_fabric(&self) -> DeviceFabric {
            self.wire.fabric.clone()
        }

        fn online_device_ids(&self) -> BTreeSet<DeviceId> {
            self.wire.online.clone()
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.device.clone(), self.label.into()))
        }

        fn try_send_personal_fabric_action(
            &self,
            target: &DeviceId,
            action: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            let peer = self
                .wire
                .links
                .lock_recover()
                .get(target.as_str())
                .cloned()
                .ok_or_else(|| {
                    PersonalFabricSendError::Rejected("that device is not reachable".into())
                })?;
            let envelope = PersonalFabricActionEnvelope {
                request_id: [7; 16],
                source: self.device.clone(),
                target: target.clone(),
                issued_unix_ms: tilo_clock::unix_now_ms(),
                action,
            };
            if !handle_action(&peer, &envelope) {
                return Err(PersonalFabricSendError::Rejected(
                    "not a certification action".into(),
                ));
            }
            Ok([7; 16])
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// The two devices `a` and `b` , connected to each other.
    fn pair(a: [u8; 32], b: [u8; 32]) -> (SharedLink, SharedLink, DeviceId, DeviceId) {
        let first = DeviceId::new(hex::encode(a)).expect("this laptop");
        let second = DeviceId::new(hex::encode(b)).expect("the other device");
        // The devices know each other from the live plane, so the ask names the laptop the way the
        // person knows it.
        let here = FabricRuntime::restored(Some((first.clone(), "This laptop".into())), None)
            .expect("a fabric here");
        let known =
            DeviceFabric::decode_checkpoint(&here.own_snapshot().expect("this laptop's record"))
                .expect("this laptop's record");
        let wire = Arc::new(Wire {
            links: Mutex::new(HashMap::new()),
            fabric: known,
            online: BTreeSet::from([first.clone(), second.clone()]),
        });
        let peer = |device: DeviceId, label: &'static str| -> SharedLink {
            let peer: SharedLink = Arc::new(Peer {
                device,
                label,
                wire: Arc::clone(&wire),
            });
            peer
        };
        let laptop = peer(first.clone(), "This laptop");
        let other = peer(second.clone(), "Old phone");
        wire.links
            .lock_recover()
            .insert(first.as_str().to_owned(), Arc::clone(&laptop));
        wire.links
            .lock_recover()
            .insert(second.as_str().to_owned(), Arc::clone(&other));
        (laptop, other, first, second)
    }

    /// The certificate proposed for a phone, as the pairing door proposes it.
    fn proposed(person: [u8; 32]) -> (DeviceCert, [u8; 32]) {
        let candidate = tilo_identity_host::device_admin::Candidate {
            device_node_id: hex::encode([0x51; 32]),
            relay_service_node_id: hex::encode([0x52; 32]),
            label: "Phone".into(),
            verification_code: "123456".into(),
        };
        tilo_identity_host::device_admin::proposed_certificate(
            person,
            &candidate,
            tilo_clock::unix_now_ns(),
        )
        .expect("a proposal")
    }

    /// The certificate `laptop` gets from its peers for an unsigned renewal of `held` , each peer's
    /// answers sent as the session's passes would.
    fn renewed(
        laptop: &SharedLink,
        peer: &SharedLink,
        cert: DeviceCert,
        held: DeviceCert,
        threshold: u16,
        total: u16,
    ) -> Result<DeviceCert, String> {
        let (done, answers) = mpsc::channel();
        let asking = Arc::clone(laptop);
        std::thread::spawn(move || {
            let _ = done.send(renew(&asking, cert, held, [0; 32], threshold, total));
        });
        tilo_test_env::wait_for("the renewal", || {
            pump(peer.as_ref());
            answers.try_recv().ok()
        })
    }

    /// The certificate `laptop` gets for the proposed device, its peer's answers sent as the
    /// session's passes would send them.
    fn certified(
        laptop: &SharedLink,
        peer: &SharedLink,
        cert: DeviceCert,
        service: [u8; 32],
    ) -> Result<DeviceCert, String> {
        let (done, answers) = mpsc::channel();
        let asking = Arc::clone(laptop);
        std::thread::spawn(move || {
            let _ = done.send(certify(&asking, cert, service, 1, 1));
        });
        tilo_test_env::wait_for("the certification", || {
            pump(peer.as_ref());
            answers.try_recv().ok()
        })
    }

    #[test]
    fn a_device_a_key_holder_does_not_hold_the_key_for_certifies_after_the_person_approves() {
        let (_root, key) = identity();
        let person = key.person();
        let (laptop, other, laptop_id, other_id) = pair([0xa1; 32], [0xb2; 32]);
        set_keys(&laptop_id, Arc::new(NoKey));
        set_keys(
            &other_id,
            Arc::new(Held {
                key: Arc::new(key),
                revoked: false,
            }),
        );

        let (cert, service) = proposed(person);
        let early = Arc::clone(&laptop);
        let approved = std::thread::spawn(move || certify(&early, cert, service, 1, 1));

        // The person is asked on the device that holds the key, once.
        let found = tilo_test_env::wait_for("the device asking here", || {
            let (fresh, _) = newly_asking(other.as_ref());
            fresh.into_iter().next()
        });
        assert_eq!(found.label, "Phone");
        assert_eq!(found.code, ceremony::pairing_code([0x51; 32], service));
        assert_eq!(found.via, "This laptop");
        let (fresh, _) = newly_asking(other.as_ref());
        assert!(fresh.is_empty(), "the person is told once");

        // The notification's own button approves it.
        assert!(host_action(&other, &answer_key(found.digest, true)));
        let certified = approved
            .join()
            .expect("the pairing thread")
            .expect("the device was certified");
        assert_eq!(certified.device_public_key, [0x51; 32]);
        assert_eq!(certified.user_public_key, person);
        tilo_user_identity::verify_device_cert(&certified).expect("the person's key signed it");

        // The ask the person answered is gone from their screen.
        assert!(asking(other.as_ref()).is_empty());
        let (fresh, gone) = newly_asking(other.as_ref());
        assert!(fresh.is_empty());
        assert_eq!(gone, vec![found.digest]);
    }

    #[test]
    fn a_revoked_device_is_refused_before_the_person_is_asked() {
        let (_root, key) = identity();
        let person = key.person();
        let (laptop, other, laptop_id, other_id) = pair([0xc3; 32], [0xd4; 32]);
        set_keys(&laptop_id, Arc::new(NoKey));
        set_keys(
            &other_id,
            Arc::new(Held {
                key: Arc::new(key),
                revoked: true,
            }),
        );

        let (cert, service) = proposed(person);
        let reason = certified(&laptop, &other, cert, service)
            .expect_err("a device the person removed cannot join again");
        assert!(reason.contains("cannot join again"), "{reason}");
        let (fresh, _) = newly_asking(other.as_ref());
        assert!(
            fresh.is_empty(),
            "nobody is asked to approve a device the person removed"
        );
    }

    #[test]
    fn a_device_with_no_part_of_the_key_says_so_instead_of_signing() {
        let (_root, key) = identity();
        let person = key.person();
        let (laptop, other, laptop_id, other_id) = pair([0xe5; 32], [0xf6; 32]);
        set_keys(&laptop_id, Arc::new(NoKey));
        set_keys(&other_id, Arc::new(NoKey));

        let (cert, service) = proposed(person);
        let reason = certified(&laptop, &other, cert, service)
            .expect_err("a device without the key cannot approve another");
        assert!(
            reason.contains("none of your devices online now can approve it"),
            "{reason}"
        );
        let (fresh, _) = newly_asking(other.as_ref());
        assert!(fresh.is_empty(), "a device without the key asks nobody");
    }

    #[test]
    fn a_revoked_device_loses_its_record_and_its_session_on_the_live_plane() {
        let key = [0x71; 32];
        let device = DeviceId::new(hex::encode(key)).expect("the old phone");
        let cert = DeviceCert {
            user_public_key: [0x72; 32],
            device_public_key: key,
            device_label: "Old phone".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let revoked = RevocationEntry {
            user_public_key: [0x72; 32],
            revoked_device_public_key: key,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        let peer = EndpointId::from_bytes(&key).expect("an endpoint");
        let mut here = FabricRuntime::restored(None, None).expect("a fabric here");
        let there = FabricRuntime::restored(Some((device.clone(), "Old phone".into())), None)
            .expect("a fabric there");

        // The device proved itself and its frames were taken.
        here.sibling(peer, std::slice::from_ref(&cert), &[]);
        here.snapshot(peer, &there.own_snapshot().expect("its record"))
            .expect("its record is kept");
        assert!(here.device_fabric().snapshots().contains_key(&device));
        assert!(here.online_device_ids().contains(&device));

        // The person removed it: the next hello carrying the revocation ends it, and everything it
        // said is dropped rather than kept as theirs.
        here.sibling(peer, std::slice::from_ref(&cert), &[revoked]);
        assert!(
            !here.device_fabric().snapshots().contains_key(&device),
            "a revoked device's record is gone"
        );
        assert!(
            !here.online_device_ids().contains(&device),
            "a revoked device has no session"
        );
    }

    /// A joined device of `person` , certified by their root and holding none of it: the laptop's
    /// shape, whose certificate ran out.
    struct Joined {
        device_key: ed25519_dalek::SigningKey,
        paths: tilo_user_identity::IdentityPaths,
        held: DeviceCert,
    }

    impl Joined {
        /// A certificate for this same device, issued `issued_at_ns` , as a later one this device
        /// was given and the fleet already holds.
        fn at(&self, issued_at_ns: u64) -> DeviceCert {
            let mut cert = self.held.clone();
            cert.issued_at_ns = issued_at_ns;
            cert.expires_at_ns = Some(issued_at_ns.saturating_add(24 * 60 * 60 * 1_000_000_000));
            cert.signature = [0; 64];
            cert
        }

        /// An unsigned renewal of the certificate `held`, issued `at`,
        /// expiring a day after it.
        fn renewal(held: &DeviceCert, at: u64) -> DeviceCert {
            let mut cert = held.clone();
            cert.issued_at_ns = at;
            cert.expires_at_ns = Some(at.saturating_add(24 * 60 * 60 * 1_000_000_000));
            cert.signature = [0; 64];
            cert
        }

        /// An unsigned renewal of the certificate it holds, issued now.
        fn unsigned_renewal(&self) -> DeviceCert {
            Self::renewal(&self.held, tilo_clock::unix_now_ns())
        }
    }

    /// A joined device of `root` 's person, labelled `label` , whose certificate ran out: the shape
    /// this laptop has, certified by the person's root and holding no root of its own.
    fn joined_device(root: &runtime_identity::SigningIdentity, label: &str) -> Joined {
        let dir = tempfile::tempdir().expect("a directory for a joined device");
        let paths = tilo_user_identity::IdentityPaths::under(dir.path().join("system"));
        paths.ensure_directories().expect("the identity tree");
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[0x6d; 32]);
        let now = tilo_clock::unix_now_ns();
        // A day of validity, issued two days ago: it has run out.
        let mut cert = DeviceCert {
            user_public_key: root.person(),
            device_public_key: device_key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(48 * 60 * 60 * 1_000_000_000),
            expires_at_ns: Some(now.saturating_sub(24 * 60 * 60 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        let digest = tilo_user_identity::device_cert_signing_digest(&cert).expect("a digest");
        cert.signature = root.sign(&digest).expect("the person's key signs it");
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
            .expect("the lapsed certificate");
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root.person()),
            &cert,
        )
        .expect("the lapsed certificate where the identity keeps it");
        tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &root.person())
            .expect("the person's key");
        tilo_user_identity::persist::write_signing_key(&paths.device_privkey, &device_key)
            .expect("the joined device's key");
        std::mem::forget(dir);
        Joined {
            device_key,
            paths,
            held: cert,
        }
    }

    /// A [`Keys`] holding `root` , and knowing `current` as the certificate the fleet holds for a
    /// device.
    fn held_by(
        root: Arc<runtime_identity::SigningIdentity>,
        current: Option<DeviceCert>,
    ) -> HeldKeys {
        HeldKeys { root, current }
    }

    struct HeldKeys {
        root: Arc<runtime_identity::SigningIdentity>,
        current: Option<DeviceCert>,
    }

    impl Keys for HeldKeys {
        fn key(
            &self,
            _person: [u8; 32],
        ) -> Result<Box<dyn ceremony::IdentityKey + Send + Sync>, String> {
            Ok(Box::new(Held {
                key: Arc::clone(&self.root),
                revoked: false,
            }))
        }

        fn revoked(&self, _person: [u8; 32], _device: &[u8; 32]) -> bool {
            false
        }

        fn held(&self, _person: [u8; 32], device: &[u8; 32]) -> Option<DeviceCert> {
            self.current
                .clone()
                .filter(|cert| &cert.device_public_key == device)
        }
    }

    /// Two devices on one fabric: the device under test at `own_key` , and the person's other
    /// device at `peer_seed` .
    fn pair_of(
        own_key: &ed25519_dalek::SigningKey,
        peer_seed: u8,
    ) -> (SharedLink, SharedLink, DeviceId, DeviceId) {
        pair(own_key.verifying_key().to_bytes(), [peer_seed; 32])
    }

    #[test]
    fn a_joined_devices_lapsed_certificate_is_renewed_through_the_persons_devices() {
        // The person's root on one of their devices, and a joined laptop that holds nothing of it
        // and whose certificate ran out.
        let (_root_dir, root) = identity();
        let person = root.person();
        let joined = joined_device(&root, "tilo-yoga");
        assert!(
            joined.held.is_expired_at(tilo_clock::unix_now_ns()),
            "the laptop starts with a certificate that has run out"
        );

        let (laptop, other, laptop_id, other_id) = pair_of(&joined.device_key, 0x4a);
        // The laptop holds no part of the person's key; the other device holds the whole of it and
        // knows the laptop's certificate.
        set_keys(&laptop_id, Arc::new(NoKey));
        set_keys(
            &other_id,
            Arc::new(held_by(Arc::new(root), Some(joined.held.clone()))),
        );

        let renewed = renewed(
            &laptop,
            &other,
            joined.unsigned_renewal(),
            joined.held.clone(),
            1,
            1,
        )
        .expect("the person's device renews the certificate");

        tilo_user_identity::verify_device_cert(&renewed).expect("the person's key signed it");
        assert_eq!(renewed.device_public_key, joined.held.device_public_key);
        assert_eq!(renewed.user_public_key, person);
        assert!(renewed.renews(&joined.held));
        let now = tilo_clock::unix_now_ns();
        assert!(
            !renewed.is_expired_at(now),
            "the renewal is valid now, so a host admits it"
        );
    }

    #[test]
    fn a_renewal_of_a_certificate_the_fleet_replaced_is_refused() {
        let (_root_dir, root) = identity();
        let joined = joined_device(&root, "tilo-yoga");
        // The fleet holds a certificate the laptop was given since the one it
        // asks to replace: a renewal replaces the newest, or nothing. The
        // laptop proposes one issued just after the certificate it still has,
        // and the fleet already holds a later one.
        let now = tilo_clock::unix_now_ns();
        let newer = joined.at(now);
        let stale = Joined::renewal(&joined.held, joined.held.issued_at_ns.saturating_add(1));

        let (laptop, other, laptop_id, other_id) = pair_of(&joined.device_key, 0x4b);
        set_keys(&laptop_id, Arc::new(NoKey));
        set_keys(&other_id, Arc::new(held_by(Arc::new(root), Some(newer))));

        let reason = renewed(&laptop, &other, stale, joined.held.clone(), 1, 1)
            .expect_err("a stale renewal replaces nothing");
        assert!(reason.contains("does not replace"), "{reason}");
    }

    #[test]
    fn a_joined_device_renewal_is_due_once_the_certificate_lapses_and_never_twice() {
        let (_root_dir, root) = identity();
        let joined = joined_device(&root, "tilo-yoga");
        let now = tilo_clock::unix_now_ns();
        // Before it runs out, nothing is due.
        assert!(
            tilo_identity_host::provisioning::joined_device_renewal_due(
                &joined.paths,
                joined.held.issued_at_ns
            )
            .is_none()
        );
        // Once it has, the certificate it holds is what is renewed.
        let due = tilo_identity_host::provisioning::joined_device_renewal_due(&joined.paths, now)
            .expect("a lapsed joined certificate is due");
        assert_eq!(due, joined.held);
    }

    /// A clipboard nobody reads.
    struct Nowhere;

    impl crate::platform::clipboard::Clipboard for Nowhere {
        fn copy(&self, _text: String, _kept_for: Duration) {}
    }

    /// The asks waiting at `link`'s device, once one shows `code`.
    fn asked_with(link: &SharedLink, code: &str) -> Asking {
        let deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
        loop {
            if let Some(asking) = asking(link.as_ref())
                .into_iter()
                .find(|asking| asking.code == code)
            {
                return asking;
            }
            assert!(
                Instant::now() < deadline,
                "the new device was never asked here"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A host of the person whose key `root` is, on this machine, serving the web a new device
    /// opens a pairing link on.
    async fn person_host(
        root: &Arc<runtime_identity::SigningIdentity>,
        state: &std::path::Path,
    ) -> (tilo_relay::daemon::Relay, tilo_relay::web::WebHandle) {
        let person = root.person();
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
        let mut host_cert = DeviceCert {
            user_public_key: person,
            device_public_key: host_key.verifying_key().to_bytes(),
            device_label: "Home".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        let root_signer = ceremony::PersonSigner::new(Box::new(Held {
            key: Arc::clone(root),
            revoked: false,
        }));
        tilo_user_identity::sign_device_cert_async(&mut host_cert, &root_signer)
            .await
            .unwrap();
        let relay = tilo_relay::daemon::Relay::start_with_relay_mode(
            tilo_relay::RelayConfig {
                state_root: state.join("host"),
                ..tilo_relay::RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: person,
                device_signing_key: host_key,
                device_cert: host_cert,
                revocations: tilo_user_identity::RevocationList::new(),
            },
            test_relay::relay_mode(),
        )
        .await
        .unwrap();
        let web = relay
            .serve_web(tilo_relay::web::WebConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: tilo_relay::web::TlsSource::Development {
                    hosts: vec!["localhost".into(), "127.0.0.1".into()],
                    store: Some(state.join("web-dev-identity")),
                },
                allowed_origins: Vec::new(),
                plain_listen: Some("127.0.0.1:0".parse().unwrap()),
                public_origin: None,
                fetch_proxy: None,
                door: None,
                link: None,
            })
            .await
            .unwrap();
        (relay, web)
    }

    /// The whole of adding a device from this computer: it shows the pairing link, a new device
    /// opens it and asks at the host the link names, this computer puts it to the person with the
    /// code both show, and once they approve, the new device is theirs.
    #[test]
    fn a_device_that_opens_the_link_becomes_the_persons_once_they_approve_here() {
        let (_root_dir, root) = identity();
        let person = root.person();
        let root = Arc::new(root);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        // The person's host on this machine, serving the web a new device opens the link on.
        let state = tempfile::tempdir().unwrap();
        let (relay, web) = runtime.block_on(person_host(&root, state.path()));
        let origin = web.plain_origin.clone().unwrap();

        // This laptop holds the person's key and shows the link.
        let (laptop, _other, laptop_id, _) = pair([0x62; 32], [0x63; 32]);
        set_keys(
            &laptop_id,
            Arc::new(Held {
                key: Arc::clone(&root),
                revoked: false,
            }),
        );
        let adding = crate::pair_device::Adding::new(std::rc::Rc::new(Nowhere));
        let shown = adding.device(
            Ok(crate::pair_device::Place {
                site: format!("{origin}/"),
                origin: origin.clone(),
                room: "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c".into(),
                person,
            }),
            tilo_clock::unix_now_ms(),
        );
        assert!(shown.refusal.is_none(), "{:?}", shown.refusal);
        let waiting = adding.waiting().unwrap();
        watch_host(
            laptop.as_ref(),
            &waiting.origin,
            waiting.person,
            waiting.until_ms,
        );

        // The phone opens it: it names the person and the host, and the phone asks there.
        let phone = ed25519_dalek::SigningKey::from_bytes(&[0x64; 32]);
        let link = tilo_relay_client::login::PairingLink::parse(&shown.link).unwrap();
        assert_eq!(link.user_public_key, person);
        assert_eq!(link.host_origin, origin);
        let (code_shown, code) = std::sync::mpsc::channel();
        let redeemed = runtime.spawn({
            let phone = phone.clone();
            async move {
                let client = tilo_workspace_directory::client().unwrap();
                let at = tilo_relay_client::https::Origin::new(&client, &link.host_origin);
                tilo_relay_client::login::sign_in(&at, &link, "phone", "Phone", &phone, |code| {
                    let _ = code_shown.send(code.to_owned());
                })
                .await
            }
        });
        let code = code.recv_timeout(Duration::from_secs(60)).unwrap();

        // This laptop asks the person, with the code the phone shows, and they approve.
        let asked = asked_with(&laptop, &code);
        assert_eq!(asked.label, "Phone");
        assert_eq!(asked.device(), phone.verifying_key().to_bytes());
        assert_eq!(
            answer(laptop.as_ref(), asked.digest, true).unwrap(),
            Some("Phone".to_owned())
        );

        // The phone is the person's device now.
        let signed_in = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(60), redeemed).await })
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(signed_in.cert.user_public_key, person);
        assert_eq!(
            signed_in.cert.device_public_key,
            phone.verifying_key().to_bytes()
        );
        tilo_user_identity::verify_device_cert(&signed_in.cert).unwrap();
        assert!(
            asking(laptop.as_ref())
                .iter()
                .all(|asking| asking.digest != asked.digest),
            "an answered device asks no more"
        );
        drop(web);
        runtime.block_on(relay.shutdown());
    }
}
