//! The person's devices as a session reads them: the record of them in the person's own
//! workspace, followed at the device's daemon, and the daemon's reading of which of them hold a
//! session with it now.
//!
//! This is what the services that reach the person's other devices act on when the device's
//! daemon is how this session reaches them ([`DaemonFabric`]): the speaker groups and the devices
//! Settings lists read the record, and the set of devices online is the daemon's. Nothing here
//! keeps a second copy of either.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tilo_network_actor::fabric::DeviceId;
use tilo_network_actor::fabric::{PersonalFabricAction, PersonalFabricActionEnvelope, nonzero};
use tilo_resource::Invoke;
use tilo_session_client::speakers::{self, Frame};
use tilo_speaker_service::devices::{
    Listing, PersonalFabricSendError, Record, Speakers, known_from,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, NearbyDevices, OnlineDevices,
};

use crate::followed_resource::SharedFollow;
use crate::platform::fabric::Continuity;

/// How long a follow waits before asking a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// An envelope another of the person's devices sent this one: the device that sent it, the
/// envelope, and when it landed on this device's monotonic clock.
type Heard = (DeviceId, PersonalFabricActionEnvelope, u64);

/// The session's link to its daemon for the person's speakers and audio routes: what the
/// session asks the daemon to carry, and what the daemon hands it that the other devices sent.
pub(crate) struct SpeakersLink {
    asks: tokio::sync::mpsc::Sender<Frame>,
    heard: Arc<Mutex<Vec<Heard>>>,
    attached: Arc<AtomicBool>,
}

impl SpeakersLink {
    /// Open the session's link to `daemon` and keep it: a daemon that is not up yet is asked
    /// again, and a link that ends is opened again.
    pub(crate) fn attach(
        daemon: &tilo_session_client::daemon_documents::DaemonShell,
        local: Option<DeviceId>,
    ) -> Self {
        let (asks, mut queue) = tokio::sync::mpsc::channel(64);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let attached = Arc::new(AtomicBool::new(false));
        let daemon = daemon.clone();
        let flag = Arc::clone(&attached);
        let inbound = Arc::clone(&heard);
        daemon.runtime().clone().spawn(async move {
            // What the link last failed at, told once: a daemon not up yet is asked again every
            // second, and the journal hears where the link stands rather than once a second.
            let mut told: Option<&'static str> = None;
            loop {
                let shell = match daemon.link() {
                    Ok(shell) => shell,
                    Err(error) => {
                        if told != Some("link") {
                            tracing::warn!(
                                %error,
                                "the daemon's link is not up yet for the speakers; asking again"
                            );
                            told = Some("link");
                        }
                        tokio::time::sleep(ASK_AGAIN).await;
                        continue;
                    }
                };
                let stream = match shell.speakers().await {
                    Ok(stream) => stream,
                    Err(error) => {
                        if told != Some("speakers") {
                            tracing::warn!(
                                %error,
                                "the daemon serves no speakers link; asking again"
                            );
                            told = Some("speakers");
                        }
                        tokio::time::sleep(ASK_AGAIN).await;
                        continue;
                    }
                };
                told = None;
                flag.store(true, Ordering::Relaxed);
                serve(stream, &mut queue, &inbound, local.clone()).await;
                flag.store(false, Ordering::Relaxed);
                tokio::time::sleep(ASK_AGAIN).await;
            }
        });
        Self {
            asks,
            heard,
            attached,
        }
    }

    /// Ask the daemon to hand `action` to `target`, from this device `local`.
    fn send(
        &self,
        local: &DeviceId,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        if !self.attached.load(Ordering::Relaxed) {
            return Err(PersonalFabricSendError::Rejected(
                "this session's daemon link for the speakers is not up".to_owned(),
            ));
        }
        let now = tilo_clock::unix_now_ms();
        let request_id = nonzero::<16>();
        let envelope = PersonalFabricActionEnvelope {
            request_id,
            source: local.clone(),
            target: target.clone(),
            issued_unix_ms: now,
            action,
        };
        if envelope.validate_at(local, target, now).is_err() {
            return Err(PersonalFabricSendError::Rejected(
                "that is not an action one device may send another".to_owned(),
            ));
        }
        let peer = hex::decode(target.as_str())
            .ok()
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .ok_or_else(|| {
                PersonalFabricSendError::Rejected("that is not a device key".to_owned())
            })?;
        let envelope = postcard::to_allocvec(&envelope).map_err(|error| {
            PersonalFabricSendError::Rejected(format!("the action does not encode: {error}"))
        })?;
        match self.asks.try_send(Frame::Send { peer, envelope }) {
            Ok(()) => Ok(request_id),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                Err(PersonalFabricSendError::Backpressure)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(PersonalFabricSendError::Rejected(
                    "this session's daemon link for the speakers ended".to_owned(),
                ))
            }
        }
    }

    /// The envelopes the daemon handed this session since the last call.
    fn take(&self) -> Vec<Heard> {
        std::mem::take(&mut *self.heard.lock_recover())
    }
}

/// Serve `stream`, the session's end of its speakers link, until either half ends.
async fn serve(
    stream: tokio::net::UnixStream,
    queue: &mut tokio::sync::mpsc::Receiver<Frame>,
    heard: &Arc<Mutex<Vec<Heard>>>,
    local: Option<DeviceId>,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let sending = async {
        while let Some(frame) = queue.recv().await {
            if let Err(error) = speakers::write(&mut writer, &frame).await {
                tracing::warn!(%error, "the session's speakers link ended while sending");
                return;
            }
        }
    };
    let hearing = async {
        loop {
            match speakers::read(&mut reader).await {
                Ok(Frame::Heard {
                    peer,
                    envelope,
                    received_us,
                }) => {
                    let Some(local) = local.as_ref() else {
                        continue;
                    };
                    let Some((source, envelope)) = read_heard(peer, &envelope, local) else {
                        continue;
                    };
                    heard.lock_recover().push((source, envelope, received_us));
                }
                Ok(Frame::Send { .. }) => {}
                Err(error) => {
                    tracing::warn!(%error, "the session's speakers link ended while hearing");
                    return;
                }
            }
        }
    };
    tokio::select! {
        () = sending => {}
        () = hearing => {}
    }
}

/// `envelope`, read as one `source` sent this device `local`: `None` when it is not.
fn read_heard(
    peer: [u8; 32],
    envelope: &[u8],
    local: &DeviceId,
) -> Option<(DeviceId, PersonalFabricActionEnvelope)> {
    let source = DeviceId::new(hex::encode(peer)).ok()?;
    let envelope: PersonalFabricActionEnvelope = postcard::from_bytes(envelope).ok()?;
    envelope
        .validate_at(&source, local, tilo_clock::unix_now_ms())
        .ok()?;
    Some((source, envelope))
}

/// One resource the device's daemon publishes of its own, followed over the shell link: the
/// reading it last answered, and the task that asks for the next.
///
/// A resource the daemon keeps itself is read as its `session` app's record named by the resource
/// ([`tilo_session_client::daemon_documents::whole`]), not as a record of a workspace: the read
/// waits at the daemon for the reading to move, and a daemon that does not answer is asked again.
pub(crate) struct DaemonReading<T> {
    latest: Arc<Mutex<(u64, T)>>,
    follow: tokio::task::AbortHandle,
}

impl<T> Drop for DaemonReading<T> {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

impl<T> DaemonReading<T>
where
    T: serde::de::DeserializeOwned + Default + Clone + PartialEq + Send + 'static,
{
    /// Follow `resource`, which the daemon publishes of its own in `workspace`, from now on:
    /// `wake` is called each time the reading moved.
    pub(crate) fn follow(
        daemon: &tilo_session_client::daemon_documents::DaemonShell,
        workspace: &str,
        resource: &'static str,
        wake: tilo_service::Wake,
    ) -> Self {
        let latest = Arc::new(Mutex::new((0_u64, T::default())));
        let follow = {
            let (daemon, latest) = (daemon.clone(), Arc::clone(&latest));
            let workspace = workspace.to_owned();
            daemon.runtime().clone().spawn(async move {
                let mut after: Option<String> = None;
                loop {
                    let mut read = tilo_session_client::daemon_documents::whole(
                        &workspace, "session", resource,
                    );
                    read.wait_ms = after
                        .is_some()
                        .then_some(tilo_actions::remote::MAX_REMOTE_WAIT_MS);
                    read.after.clone_from(&after);
                    match daemon.read(read).await {
                        Ok(answered) => {
                            after = answered
                                .get("revision")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                            if let Some(now) = reading_value::<T>(&answered) {
                                let mut latest = latest.lock_recover();
                                if latest.1 != now {
                                    latest.0 = latest.0.wrapping_add(1);
                                    latest.1 = now;
                                    drop(latest);
                                    wake();
                                }
                            }
                        }
                        Err(error) => {
                            tilo_failure::ignored!(
                                "a follow asks again a moment later",
                                tracing::debug!(%error, resource, "a followed reading did not read")
                            );
                            after = None;
                            tokio::time::sleep(ASK_AGAIN).await;
                        }
                    }
                }
            })
        };
        Self {
            latest,
            follow: follow.abort_handle(),
        }
    }

    /// The last reading; `T`'s default before the first.
    pub(crate) fn get(&self) -> T {
        self.latest.lock_recover().1.clone()
    }
}

/// The reading `answered` answers, as `T`: its default for a resource that reads nothing, and
/// `None` while it does not read.
fn reading_value<T: serde::de::DeserializeOwned + Default>(
    answered: &serde_json::Value,
) -> Option<T> {
    let value = answered["records"]
        .as_array()
        .and_then(|records| records.first())
        .map_or(serde_json::Value::Null, |record| record["value"].clone());
    if value.is_null() {
        return Some(T::default());
    }
    match serde_json::from_value(value) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "a followed reading does not read");
            None
        }
    }
}

/// The person's devices, as their daemon has them: the record it follows, what it says is online
/// now, and this device's own identity.
pub(crate) struct DaemonFabric {
    record: SharedFollow<Devices>,
    online: DaemonReading<OnlineDevices>,
    /// The person's other devices the daemon hears on this device's local network.
    nearby: DaemonReading<NearbyDevices>,
    speakers: SpeakersLink,
    cert: DeviceCert,
    local: Option<(DeviceId, String)>,
    continuity: Continuity,
}

impl DaemonFabric {
    /// The person's devices, as `record` and `online` read at the daemon, with `cert` this
    /// device's.
    pub(crate) fn new(
        record: SharedFollow<Devices>,
        online: DaemonReading<OnlineDevices>,
        nearby: DaemonReading<NearbyDevices>,
        speakers: SpeakersLink,
        cert: DeviceCert,
        continuity: Continuity,
    ) -> Self {
        let local = DeviceId::new(hex::encode(cert.device_public_key))
            .ok()
            .map(|device| (device, cert.device_label.clone()));
        Self {
            record,
            online,
            nearby,
            speakers,
            cert,
            local,
            continuity,
        }
    }

    /// The devices the daemon says are online now, by the key each is named by.
    fn online_ids(&self) -> BTreeSet<DeviceId> {
        self.online
            .get()
            .devices
            .into_iter()
            .filter_map(|key| DeviceId::new(key).ok())
            .collect()
    }
}

impl Record for DaemonFabric {
    fn record(&self) -> Option<Devices> {
        Some((*self.record.get()).clone())
    }

    fn apply(&self, action: Invoke<Devices>) -> Result<(), String> {
        self.record.invoke(&action)
    }

    fn person(&self) -> [u8; 32] {
        self.cert.user_public_key
    }
}

impl Speakers for DaemonFabric {
    fn take_heard(&self) -> Vec<(DeviceId, PersonalFabricActionEnvelope, u64)> {
        self.speakers.take()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.online_ids()
    }

    /// The person's other devices this daemon hears on this device's local network now, so
    /// reachable here without a server.
    fn nearby_device_ids(&self) -> BTreeSet<DeviceId> {
        self.nearby
            .get()
            .devices
            .into_iter()
            .filter_map(|device| DeviceId::new(device).ok())
            .collect()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        self.local.clone()
    }

    /// A session holds no endpoint of its own: the daemon carries what it sends to another of
    /// the person's devices, and what they send here comes back as heard.
    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        let Some((local, _)) = &self.local else {
            return Err(PersonalFabricSendError::Rejected(
                "this device's key is not one a session sends from".to_owned(),
            ));
        };
        self.speakers.send(local, target, action)
    }

    fn require_media(&self) -> Result<(), String> {
        self.continuity
            .media_permitted()
            .then_some(())
            .ok_or_else(|| "media continuity is turned off in this device's settings".to_owned())
    }

    fn known(&self) -> Listing {
        known_from(
            &self.record.get(),
            &self.cert.user_public_key,
            self.local.as_ref().map(|(device, _)| device),
            &self.online_ids(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::sign_device_cert;
    use tilo_workspace_service::workspaces::person_workspace;
    use tilo_workspace_service::workspaces::person_workspace::devices::DEVICES;

    /// A daemon of the person's, holding their own workspace: the record of their devices, and
    /// the daemon's own reading of which of them are online, are what the session's fabric reads.
    #[test]
    fn the_fabric_is_the_record_and_the_daemons_online_set() {
        let daemon = crate::daemon_fixture::TestDaemon::start();
        let person = daemon.certificate().user_public_key;
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        assert_eq!(
            person,
            root.verifying_key().to_bytes(),
            "the fixture's daemon is the person's root's device"
        );
        let workspaces = daemon.workspaces().clone();
        tilo_rt::app()
            .block_on(async move { person_workspace::found(&workspaces).await })
            .expect("the runtime runs")
            .expect("the daemon holds the person's own workspace");
        let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
        let member = tilo_member_types::PersonId::from_root(person);
        let wake = std::sync::Arc::new(|| {});
        let record = crate::followed_resource::FollowedResource::<Devices>::follow(
            &daemon.shell(),
            (&workspace, DEVICES.0, DEVICES.1),
            member.as_str(),
            None,
            wake.clone(),
        );
        let mut other = DeviceCert {
            user_public_key: person,
            device_public_key: [9; 32],
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut other, &root).expect("the person's root signs the other device");
        record
            .invoke(&Devices::keeping(&other).expect("the device certificate encodes"))
            .expect("the record keeps the other device");

        // The daemon's own reading: the other device holds a session with it now, and so does
        // the device its session serves.
        daemon.set_online([
            other.device_public_key,
            daemon.certificate().device_public_key,
        ]);
        let online = DaemonReading::follow(
            &daemon.shell(),
            &workspace,
            tilo_workspace_service::workspaces::person_workspace::devices::ONLINE,
            wake,
        );
        let local = DeviceId::new(hex::encode(daemon.certificate().device_public_key))
            .expect("a device id");
        let nearby = DaemonReading::follow(
            &daemon.shell(),
            &workspace,
            tilo_workspace_service::workspaces::person_workspace::devices::NEARBY,
            std::sync::Arc::new(|| {}),
        );
        let fabric = DaemonFabric::new(
            record.shared(),
            online,
            nearby,
            SpeakersLink::attach(&daemon.shell(), Some(local)),
            daemon.certificate(),
            Continuity::default(),
        );

        let other = DeviceId::new(hex::encode(other.device_public_key)).expect("a device id");
        let deadline = tilo_test_env::hang_deadline();
        while !fabric.online_device_ids().contains(&other) {
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon's online set reaches the session"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let listing = fabric.known();
        let listed = listing
            .get(&other)
            .expect("the record lists the other device");
        assert!(listed.online, "the daemon says the other device is online");
        assert_eq!(listed.about.label, "Laptop");
        assert!(
            fabric
                .record()
                .expect("the record reads")
                .devices
                .get(&tilo_resource::Id::from_raw(other.as_str().to_owned()))
                .is_some(),
            "the record keeps it, and nothing else keeps a copy"
        );
    }

    /// The fixture's daemon, keeping its identity where this process reads its own, is the
    /// session's identity: what every part that acts as the device reads.
    #[test]
    fn the_fixtures_identity_is_the_sessions() {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let daemon = crate::daemon_fixture::TestDaemon::start();
        daemon.serve_identity(&mut home);
        let identity = crate::sync::device_identity().expect("the session reads its identity");
        assert_eq!(
            identity.device_cert.device_public_key,
            daemon.certificate().device_public_key
        );
    }

    /// A session the daemon started, with no stand-in fabric, lists the person's other device
    /// from the record and shows it online: what the daemon says is online is the session's.
    #[test]
    fn a_session_on_a_daemon_lists_the_persons_other_device_online() {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let daemon = crate::daemon_fixture::TestDaemon::start();
        daemon.serve_identity(&mut home);
        let person = daemon.certificate().user_public_key;
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let workspaces = daemon.workspaces().clone();
        tilo_rt::app()
            .block_on(async move { person_workspace::found(&workspaces).await })
            .expect("the runtime runs")
            .expect("the daemon holds the person's own workspace");
        let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
        let member = tilo_member_types::PersonId::from_root(person);
        let wake = std::sync::Arc::new(|| {});
        let record = crate::followed_resource::FollowedResource::<Devices>::follow(
            &daemon.shell(),
            (&workspace, DEVICES.0, DEVICES.1),
            member.as_str(),
            None,
            wake,
        );
        let mut other = DeviceCert {
            user_public_key: person,
            device_public_key: [9; 32],
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut other, &root).expect("the person's root signs the other device");
        record
            .invoke(&Devices::keeping(&other).expect("the device certificate encodes"))
            .expect("the record keeps the other device");
        daemon.set_online([
            other.device_public_key,
            daemon.certificate().device_public_key,
        ]);

        let mut session = crate::Session::headless(
            crate::Options {
                person: hex::encode(person),
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(home.state().to_path_buf()),
                shell_link: Some(daemon.shell_link()),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("the session starts on the daemon's link");
        let key = hex::encode(other.device_public_key);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let shown = session
                .read("session:devices")
                .and_then(|reading| reading["devices"].as_array().cloned())
                .and_then(|devices| {
                    devices
                        .into_iter()
                        .find(|device| device["id"] == key && device["online"] == true)
                });
            if shown.is_some_and(|device| device["label"] == "Laptop") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the session's devices reading lists the other device online: {:?}",
                session.read("session:devices")
            );
            session
                .step(std::time::Duration::from_millis(10))
                .expect("the session's pass");
        }
    }
}
