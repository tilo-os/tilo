//! Adding a device to the person from this device.
//!
//! A new device that opened the pairing link this device shows ([`crate::pairing_link`]) asks at
//! the host the link names. While the link works this device waits there for it
//! ([`watch_host`]), on its own connection as the person, not on any workspace's: the device joins
//! the person, whichever workspace the link opened. It is put to the person in a list and a
//! notification naming the new device and its code ([`asking`]), and approving it from the
//! notification's own buttons has the person's root sign its certificate ([`answer`]), which the
//! host hands the new device. Only a device holding the person's whole root signs: one holding
//! less says so.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crate::platform::fabric::{DeviceId, FabricLink, SharedLink};
use tilo_lock::MutexRecover as _;
use tilo_server_client::DeviceSigner;

/// How long the person has to approve a new device here. The new device waits 270 seconds for its
/// pairing; the exchange after the approval needs part of that.
pub(crate) const APPROVAL_WINDOW: Duration = Duration::from_secs(200);

/// The host action id prefix a device asking to be the person is answered under:
/// `<prefix><digest>:approve` or `<prefix><digest>:decline` , which the session's loop hands back
/// from the notification's buttons.
const ANSWER_PREFIX: &str = "device-answer:";

/// The person's root as this device holds it.
pub(crate) trait Keys: Send + Sync {
    /// The person's root, as the signer of their answers.
    ///
    /// # Errors
    ///
    /// When the person's identity cannot be opened here.
    fn key(&self, person: [u8; 32]) -> Result<Arc<dyn DeviceSigner + Send + Sync>, String>;
}

/// [`Keys`] from the session's identity store.
struct StoreKeys;

impl Keys for StoreKeys {
    fn key(&self, person: [u8; 32]) -> Result<Arc<dyn DeviceSigner + Send + Sync>, String> {
        let key = crate::identity_admin::fleet_store()?
            .identity_key(&hex::encode(person))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(key))
    }
}

/// A new device asking to be the person, waiting for them here.
#[derive(Clone, Debug)]
pub(crate) struct Asking {
    /// Names it in the notification's answer.
    pub(crate) digest: [u8; 32],
    /// What the new device calls itself.
    pub(crate) label: String,
    /// The code the new device shows.
    pub(crate) code: String,
    /// The person's device that found it: empty for one that asked at a host.
    pub(crate) via: String,
    pub(crate) asked_at_ms: u64,
    /// The host it waits at, which the link this device showed names.
    origin: String,
    host: tilo_server_client::ServerAddress,
    person: [u8; 32],
}

/// A host this device waits at for the new devices its pairing link sends there.
struct Watch {
    /// Until when, Unix-epoch milliseconds: when the link shown runs out.
    until_ms: u64,
    /// Whether it still waits; a wait that failed is not started again for the same link.
    running: bool,
}

/// One device's part in adding devices.
#[derive(Default)]
struct State {
    asking: Vec<Asking>,
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

/// What wakes the session's loop when a new device asks here.
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
/// here. `None` when no device asks under `digest`; else the device's label.
/// Blocks on the host.
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
    answer_at_host(&local, asking.host, asking.person, digest, approve)?;
    Ok(Some(asking.label))
}

/// Answer the device asking under `digest` at `host`: approved, the person's key certifies it there.
/// Blocks on the host.
fn answer_at_host(
    local: &DeviceId,
    host: tilo_server_client::ServerAddress,
    person: [u8; 32],
    digest: [u8; 32],
    approve: bool,
) -> Result<(), String> {
    let signer = keys(local).key(person)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let approver = tilo_server_client::MemberApprover::new(host);
        let answered = tilo_server_client::certification::answer_pairing(
            &approver,
            signer.as_ref(),
            digest,
            approve,
        )
        .await;
        approver.close().await;
        answered.map(drop).map_err(|error| error.to_string())
    })
}

/// Wait at the host at `origin` for the new devices asking to be `person` there, until `until_ms`:
/// the pairing link this device shows names that host, and a device that opens it asks there. Each
/// is put to the person as any device asking to be them ([`asking`]), and answered there
/// ([`answer`]). Asking again while it waits only moves when it stops; a wait that failed is not
/// started again until a new link is shown.
pub(crate) fn watch_host(
    fabric: &dyn FabricLink,
    origin: &str,
    host: tilo_server_client::ServerAddress,
    person: [u8; 32],
    until_ms: u64,
) {
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
            if let Err(error) = wait_at_host(&local, &origin, &host, person) {
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
fn wait_at_host(
    local: &DeviceId,
    origin: &str,
    host: &tilo_server_client::ServerAddress,
    person: [u8; 32],
) -> Result<(), String> {
    let signer = keys(local).key(person)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let approver = tilo_server_client::MemberApprover::new(host.clone());
        let mut known = Vec::new();
        let waited = loop {
            let until_ms = with_state(local, |state| {
                state.watching.get(origin).map_or(0, |watch| watch.until_ms)
            });
            if tilo_clock::unix_now_ms() >= until_ms {
                break Ok(());
            }
            let pending =
                match tilo_server_client::next_pairings(&approver, signer.as_ref(), known.clone())
                    .await
                {
                    Ok(pending) => pending,
                    Err(error) => break Err(error.to_string()),
                };
            known = pending.iter().map(|entry| entry.request_digest).collect();
            follow_host(local, origin, host, person, pending);
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
    host: &tilo_server_client::ServerAddress,
    person: [u8; 32],
    pending: Vec<tilo_server_protocol::certify::PendingCertification>,
) {
    let expired = expired_before();
    with_state(local, |state| {
        let digests: Vec<[u8; 32]> = pending.iter().map(|entry| entry.request_digest).collect();
        state
            .asking
            .retain(|asking| asking.origin != origin || digests.contains(&asking.digest));
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
                origin: origin.to_owned(),
                host: host.clone(),
                person,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fabric::{PersonDevices, PersonalFabricAction, PersonalFabricSendError};
    use std::collections::BTreeSet;
    use std::time::Instant;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    /// The person's identity as first boot provisions it, in files.
    fn identity() -> (tempfile::TempDir, tilo_identity::store::SigningIdentity) {
        let root = tempfile::tempdir().expect("temp root");
        let dir = root.path().join("config/tilo/identity");
        let device = tilo_device_identity::IdentityPaths::under(root.path().join("system"));
        tilo_identity::provisioning::ensure_collaboration_identity(
            &dir,
            &device,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let registry = tilo_identity::store::IdentityRegistryStore::at(&dir)
            .load()
            .expect("registry");
        let id = registry.default_identity.expect("default identity");
        let key = tilo_identity::device_fleet::DeviceFleetStore::at(&dir, device)
            .identity_key(&id)
            .expect("the person's key");
        (root, key)
    }

    /// A device holding the person's whole root.
    struct Held(Arc<tilo_identity::store::SigningIdentity>);

    impl Keys for Held {
        fn key(&self, _person: [u8; 32]) -> Result<Arc<dyn DeviceSigner + Send + Sync>, String> {
            Ok(Arc::clone(&self.0) as Arc<dyn DeviceSigner + Send + Sync>)
        }
    }

    /// This device on a fabric of its own: it reaches none of the person's other devices.
    struct Alone(DeviceId);

    impl FabricLink for Alone {
        fn devices(&self) -> PersonDevices {
            PersonDevices::new(Vec::new(), Vec::new())
        }

        fn online_device_ids(&self) -> BTreeSet<DeviceId> {
            BTreeSet::from([self.0.clone()])
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.0.clone(), "This laptop".into()))
        }

        fn try_send_personal_fabric_action(
            &self,
            _target: &DeviceId,
            _action: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            Err(PersonalFabricSendError::Rejected(
                "no other device is reachable".into(),
            ))
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// The device `key` , as the session's fabric names it.
    fn this_device(key: [u8; 32]) -> (SharedLink, DeviceId) {
        let id = DeviceId::new(hex::encode(key)).expect("this laptop");
        let link: SharedLink = Arc::new(Alone(id.clone()));
        (link, id)
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
        root: &Arc<tilo_identity::store::SigningIdentity>,
        state: &std::path::Path,
    ) -> (tilo_server::daemon::Server, tilo_server::web::WebHandle) {
        let person = root.device_public_key();
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
        let mut server_cert = DeviceCert {
            user_public_key: person,
            device_public_key: host_key.verifying_key().to_bytes(),
            device_label: "Home".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert_async(&mut server_cert, root.as_ref())
            .await
            .unwrap();
        let server = tilo_server::daemon::Server::start_with_relay_mode(
            tilo_server::ServerConfig::loopback(state.join("host")),
            tilo_server::daemon::ServerIdentity {
                user_public_key: person,
                device_signing_key: host_key,
                device_cert: server_cert,
                revocations: tilo_user_identity::RevocationList::new(),
            },
            test_server::relay_mode(),
        )
        .await
        .unwrap();
        let web = server
            .serve_web(tilo_server::web::WebConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: tilo_server::web::TlsSource::Development {
                    hosts: vec!["localhost".into(), "127.0.0.1".into()],
                    store: Some(state.join("web-dev-identity")),
                },
                allowed_origins: Vec::new(),
                plain_listen: Some("127.0.0.1:0".parse().unwrap()),
                public_origin: None,
                fetch_proxy: None,
                wait_list: Arc::default(),
                link: None,
                site: None,
            })
            .await
            .unwrap();
        (server, web)
    }

    /// The whole of adding a device from this computer: it shows the pairing link, a new device
    /// opens it and asks at the host the link names, this computer puts it to the person with the
    /// code both show, and once they approve, the new device is theirs.
    #[test]
    fn a_device_that_opens_the_link_becomes_the_persons_once_they_approve_here() {
        let (_root_dir, root) = identity();
        let person = root.group_public_key();
        let root = Arc::new(root);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        // The person's host on this machine, serving the web a new device opens the link on.
        let state = tempfile::tempdir().unwrap();
        let (server, web) = runtime.block_on(person_host(&root, state.path()));
        let origin = web.plain_origin.clone().unwrap();

        // This laptop holds the person's key and shows the link.
        let (laptop, laptop_id) = this_device([0x62; 32]);
        set_keys(&laptop_id, Arc::new(Held(Arc::clone(&root))));
        let adding = crate::pair_device::Adding::new(std::rc::Rc::new(Nowhere), None);
        let shown = adding.device(
            Ok(crate::pair_device::Place {
                site: format!("{origin}/"),
                origin: origin.clone(),
                workspace: "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c".into(),
                host: tilo_server_client::ServerAddress::at(server.node_addr()),
                person,
            }),
            tilo_clock::unix_now_ms(),
        );
        assert!(shown.refusal.is_none(), "{:?}", shown.refusal);
        let waiting = adding.waiting().unwrap();
        watch_host(
            laptop.as_ref(),
            &waiting.origin,
            waiting.host,
            waiting.person,
            waiting.until_ms,
        );

        // The phone asks the host the link names over the device link, and shows the code.
        let phone = ed25519_dalek::SigningKey::from_bytes(&[0x64; 32]);
        let link = tilo_server_client::login::PairingLink::parse(&shown.link).unwrap();
        assert_eq!(link.user_public_key, person);
        assert_eq!(link.host_origin, origin);
        let at = tilo_server_client::ServerAddress::at(server.node_addr());
        let (code_shown, code) = std::sync::mpsc::channel();
        let redeemed = runtime.spawn({
            let phone = phone.clone();
            async move {
                let invite = link.invite.unwrap_or([0; 16]);
                tilo_server_client::certify::certify(
                    &at,
                    link.user_public_key,
                    "Phone",
                    invite,
                    &phone,
                    |code| {
                        let _ = code_shown.send(code.to_owned());
                    },
                )
                .await
            }
        });
        let code = code.recv_timeout(Duration::from_secs(60)).unwrap();

        // This laptop asks the person, with the code the phone shows, and they approve.
        let asked = asked_with(&laptop, &code);
        assert_eq!(asked.label, "Phone");
        assert_eq!(
            answer(laptop.as_ref(), asked.digest, true).unwrap(),
            Some("Phone".to_owned())
        );

        // The phone is the person's device now.
        let (cert, _server_cert, _label) = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(60), redeemed).await })
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(cert.user_public_key, person);
        assert_eq!(cert.device_public_key, phone.verifying_key().to_bytes());
        tilo_user_identity::verify_device_cert(&cert).unwrap();
        assert!(
            asking(laptop.as_ref())
                .iter()
                .all(|asking| asking.digest != asked.digest),
            "an answered device asks no more"
        );
        drop(web);
        runtime.block_on(server.shutdown());
    }
}
