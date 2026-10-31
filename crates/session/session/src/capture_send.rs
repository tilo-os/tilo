//! Sending the last screenshot to one of the person's other devices.
//!
//! A send is an ordinary Nearby Send: the screenshot is staged into this
//! device's private spool, offered to the device as a fabric action, and its
//! bytes stream from that spool; the receiving device's person still decides
//! where it lands there. Staging copies and hashes every byte, so it happens
//! on a worker of its own that owns the spool, never on the session's pass:
//! the pass hands it the send and takes what became of it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use tilo_capture_studio::{Refusal, Sent};
use tilo_device_fabric::link::{NearbySpoolOffer, SharedLink};
use tilo_device_fabric::{DeviceId, PersonalFabricAction};
use tilo_nearby_service::{MAX_TRANSFER_BYTES, NearbyTransferStore, SecureNearbySource};
use tilo_network_actor::NearbyTransferCheckpoint;
use tilo_service::Wake;
use ui::widget::ScreenshotSendDevice;

/// Where the spool this session stages into lives: beside what the rest of
/// the session keeps, private to the person.
#[must_use]
pub(crate) fn spool_root() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo").join("nearby-transfers"))
}

/// The person's other devices that are here now, as the studio's "Send…"
/// lists them: online, never this device, by their own names.
#[must_use]
pub(crate) fn devices(link: &SharedLink) -> Vec<ScreenshotSendDevice> {
    let local = link
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let online = link.online_device_ids();
    let fabric = link.device_fabric();
    let mut devices: Vec<ScreenshotSendDevice> = fabric
        .online_devices()
        .filter(|presence| online.contains(&presence.device))
        .filter(|presence| local.as_ref() != Some(&presence.device))
        .map(|presence| ScreenshotSendDevice {
            id: presence.device.as_str().to_owned(),
            label: presence.label.clone(),
        })
        .collect();
    devices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    devices
}

/// The screenshots on their way to the person's other devices, and what
/// became of the last one.
pub(crate) struct Sends {
    /// The person's devices, when this session runs on one.
    link: Option<SharedLink>,
    /// The worker that owns the spool; `None` when this session stages
    /// nothing or its worker could not start.
    jobs: Option<Sender<Job>>,
    /// What became of each send the worker finished.
    done: Receiver<Sent>,
}

/// One staged send, as the pass hands it to the worker.
struct Job {
    device: DeviceId,
    /// How the person sees that device named.
    label: String,
    path: PathBuf,
}

impl Sends {
    /// The sends of a session that stages into `spool` and offers on `link`.
    pub(crate) fn new(link: Option<SharedLink>, spool: Option<PathBuf>, wake: Wake) -> Self {
        let (jobs, mailbox) = channel();
        let (answers, done) = channel();
        let mut worker = Some(jobs);
        if let (Some(link), Some(spool)) = (&link, spool) {
            let link = std::sync::Arc::clone(link);
            let spawned = std::thread::Builder::new()
                .name("tilo-capture-send".into())
                .spawn(move || work(spool, link, mailbox, answers, wake));
            if let Err(error) = spawned {
                tracing::warn!(%error, "no thread takes what a screenshot is sent by");
                worker = None;
            }
        }
        Self {
            link,
            jobs: worker,
            done,
        }
    }

    /// Offer the screenshot at `path` to the device the person picked, by the
    /// id the studio listed. The device is resolved again here: the picker's
    /// id is never trusted, and one that left since is refused at once.
    ///
    /// Returns what became of it when it was refused before anything was
    /// staged; the worker reports the rest through [`Self::take`].
    pub(crate) fn send(&self, id: &str, path: PathBuf) -> Option<Sent> {
        let Some(link) = &self.link else {
            return Some(Sent::refused(id.to_owned(), Refusal::SyncOff));
        };
        let Some(device) = devices(link)
            .into_iter()
            .find(|device| device.id == id)
            .and_then(|device| DeviceId::new(&device.id).ok().map(|id| (id, device.label)))
        else {
            return Some(Sent::refused(id.to_owned(), Refusal::Offline));
        };
        let Some(jobs) = &self.jobs else {
            return Some(Sent::refused(
                device.1,
                Refusal::Refused("the send worker is not running".to_owned()),
            ));
        };
        let job = Job {
            device: device.0,
            label: device.1,
            path,
        };
        if jobs.send(job).is_err() {
            return Some(Sent::refused(
                id.to_owned(),
                Refusal::Refused("the send worker stopped".to_owned()),
            ));
        }
        None
    }

    /// What became of a send the worker finished since the last pass.
    pub(crate) fn take(&self) -> Option<Sent> {
        self.done.try_recv().ok()
    }
}

/// Stage every send the pass hands over and offer it, until the pass is gone.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the thread owns the spool, the link and its ends of both channels"
)]
fn work(spool: PathBuf, link: SharedLink, jobs: Receiver<Job>, done: Sender<Sent>, wake: Wake) {
    let mut store: Option<NearbyTransferStore> = None;
    while let Ok(job) = jobs.recv() {
        let sent = offer(&mut store, &spool, &link, job);
        if done.send(sent).is_err() {
            return;
        }
        wake();
    }
}

/// Stage the screenshot one send names into the spool and offer it to its
/// device, with the staged bytes on their way to it.
fn offer(
    store: &mut Option<NearbyTransferStore>,
    spool: &Path,
    link: &SharedLink,
    job: Job,
) -> Sent {
    let Job {
        device,
        label,
        path,
    } = job;
    let Some(folder) = path.parent() else {
        return Sent::refused(label, Refusal::NoFolder { path });
    };
    if !path.is_file() {
        return Sent::refused(label, Refusal::Gone { path });
    }
    let source = match SecureNearbySource::open_beneath(folder, &path) {
        Ok(source) => source,
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    let bytes = match source.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    if bytes > MAX_TRANSFER_BYTES {
        return Sent::refused(
            label,
            Refusal::TooLarge {
                bytes,
                limit: MAX_TRANSFER_BYTES,
            },
        );
    }
    let store = match spooled(store, spool) {
        Ok(store) => store,
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    let id = tilo_device_fabric::random::nonzero::<16>();
    let staged = match store.stage_secure_with_id(
        source,
        device.as_str().to_owned(),
        tilo_clock::unix_now_ms(),
        id,
    ) {
        Ok(staged) => staged,
        Err(error) => return Sent::refused(label, Refusal::Refused(error)),
    };
    let offer = PersonalFabricAction::NearbyTransferOffer {
        transfer_id: staged.id,
        name: staged.name.clone(),
        kind: staged.kind,
        total_bytes: staged.total_bytes,
        content_hash: staged.content_hash,
    };
    let request_id = match link.try_send_personal_fabric_action(&device, offer) {
        Ok(request_id) => request_id,
        Err(error) => {
            let _ = store.cancel_outgoing(staged.id);
            return Sent::refused(label, Refusal::Refused(error.to_string()));
        }
    };
    let file = match std::fs::File::open(store.object_path(staged.id)) {
        Ok(file) => file,
        Err(error) => {
            cancel(link, store, &device, request_id, staged.id);
            return Sent::refused(label, Refusal::Refused(error.to_string()));
        }
    };
    let checkpoint = NearbyTransferCheckpoint {
        transfer_id: staged.id,
        request_id,
        total_bytes: staged.total_bytes,
        // The offer is out; the bytes start from the top of the object, and
        // the receiving device keeps what it already holds only if this
        // prefix is what it has.
        resume_offset: 0,
        prefix_hash: *blake3::hash(&[]).as_bytes(),
        content_hash: staged.content_hash,
    };
    if let Err(error) = link.share_nearby_spool(&device, NearbySpoolOffer { checkpoint, file }) {
        cancel(link, store, &device, request_id, staged.id);
        return Sent::refused(label, Refusal::Refused(error.to_string()));
    }
    Sent::offered(label)
}

/// The spool this worker stages into, opened on its first send.
fn spooled<'a>(
    store: &'a mut Option<NearbyTransferStore>,
    spool: &Path,
) -> Result<&'a mut NearbyTransferStore, String> {
    if store.is_none() {
        *store = Some(NearbyTransferStore::open(
            spool.to_path_buf(),
            tilo_clock::unix_now_ms(),
        )?);
    }
    store
        .as_mut()
        .ok_or_else(|| "the transfer spool did not open".to_owned())
}

/// Nothing will stream what was offered: the receiving device is told to
/// discard it, and the staged bytes go.
fn cancel(
    link: &SharedLink,
    store: &mut NearbyTransferStore,
    device: &DeviceId,
    request_id: [u8; 16],
    transfer_id: [u8; 16],
) {
    let discard = PersonalFabricAction::NearbyTransferCancel {
        request_id,
        transfer_id,
    };
    if let Err(error) = link.send_personal_fabric_action(device, discard) {
        tracing::warn!(%error, "a device was not told a screenshot offer was withdrawn");
    }
    if let Err(error) = store.cancel_outgoing(transfer_id) {
        tracing::warn!(%error, "a staged screenshot was not removed");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError};
    use tilo_device_fabric::{
        DeviceFabric, DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
        FabricCapabilities, FabricStamp, OwnerSession,
    };
    use tilo_nearby_service::TransferKind;

    use super::*;

    /// One of the person's devices as it last published itself.
    fn device(id: &str, label: &str, online: bool, counter: u64) -> DeviceFabricSnapshot {
        let owner = DeviceId::new(id).expect("a device id");
        DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: label.to_owned(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities::default(),
                online,
                last_seen_unix_ms: 10,
                wake_routes: Vec::new(),
                session: OwnerSession::default(),
                stamp: FabricStamp {
                    counter,
                    device: owner,
                },
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            unlock: tilo_device_fabric::unlock::UnlockFabricState::default(),
            speakers: tilo_device_fabric::speaker_group::SpeakerFabricState::default(),
        }
    }

    /// The person's fabric as a session sees it, recording what it is asked
    /// to carry instead of carrying it.
    struct FakeFabric {
        fabric: DeviceFabric,
        local: DeviceId,
        online: BTreeSet<DeviceId>,
        /// Every action offered, with the device it was offered to.
        actions: Mutex<Vec<(DeviceId, PersonalFabricAction)>>,
        /// The spool object each stream carried, by transfer.
        streams: Mutex<BTreeMap<[u8; 16], Vec<u8>>>,
    }

    impl FakeFabric {
        /// This device and Ada's phone, both here.
        fn new() -> Self {
            let local = DeviceId::new("device-this").expect("a device id");
            let phone = DeviceId::new("device-phone").expect("a device id");
            let mut fabric = DeviceFabric::default();
            fabric
                .upsert(device("device-this", "This laptop", true, 1))
                .expect("this device");
            fabric
                .upsert(device("device-phone", "Ada's phone", true, 2))
                .expect("the phone");
            let online = [local.clone(), phone].into_iter().collect();
            Self {
                fabric,
                local,
                online,
                actions: Mutex::new(Vec::new()),
                streams: Mutex::new(BTreeMap::new()),
            }
        }

        fn actions(&self) -> Vec<(DeviceId, PersonalFabricAction)> {
            self.actions.lock().expect("the recorded actions").clone()
        }

        fn streams(&self) -> BTreeMap<[u8; 16], Vec<u8>> {
            self.streams.lock().expect("the recorded streams").clone()
        }
    }

    impl FabricLink for FakeFabric {
        fn device_fabric(&self) -> DeviceFabric {
            self.fabric.clone()
        }

        fn online_device_ids(&self) -> BTreeSet<DeviceId> {
            self.online.clone()
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.local.clone(), "This laptop".to_owned()))
        }

        fn send_personal_fabric_action(
            &self,
            target: &DeviceId,
            action: PersonalFabricAction,
        ) -> Result<[u8; 16], String> {
            self.try_send_personal_fabric_action(target, action)
                .map_err(|error| error.to_string())
        }

        fn try_send_personal_fabric_action(
            &self,
            target: &DeviceId,
            action: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            self.actions
                .lock()
                .expect("the recorded actions")
                .push((target.clone(), action));
            Ok([7; 16])
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }

        fn share_nearby_spool(
            &self,
            _target: &DeviceId,
            offer: NearbySpoolOffer,
        ) -> Result<(), PersonalFabricSendError> {
            let mut bytes = Vec::new();
            let mut file = offer.file;
            file.read_to_end(&mut bytes)
                .map_err(|error| PersonalFabricSendError::Rejected(error.to_string()))?;
            self.streams
                .lock()
                .expect("the recorded streams")
                .insert(offer.checkpoint.transfer_id, bytes);
            Ok(())
        }
    }

    /// A screenshot the studio wrote, and the link that would carry it.
    fn fixture() -> (tempfile::TempDir, PathBuf, Arc<FakeFabric>) {
        let dir = tempfile::tempdir().expect("a folder");
        let shot = dir.path().join("Screenshot 1.png");
        std::fs::write(&shot, b"the screenshot's bytes").expect("a screenshot");
        (dir, shot, Arc::new(FakeFabric::new()))
    }

    /// A send's wake, and what the session's loop hears on it.
    fn waking() -> (Wake, std::sync::mpsc::Receiver<()>) {
        let (heard, woken) = std::sync::mpsc::channel();
        let wake: Wake = Arc::new(move || {
            let _ = heard.send(());
        });
        (wake, woken)
    }

    #[test]
    fn the_picker_lists_the_other_devices_that_are_here() {
        let (_dir, _shot, fake) = fixture();
        let link: SharedLink = fake;
        assert_eq!(
            devices(&link),
            vec![ScreenshotSendDevice {
                id: "device-phone".into(),
                label: "Ada's phone".into(),
            }],
            "this device is never offered itself"
        );
    }

    #[test]
    fn a_screenshot_is_offered_to_the_device_and_streamed_from_the_spool() {
        let (dir, shot, fake) = fixture();
        let spool = dir.path().join("spool");
        let link: SharedLink = Arc::clone(&fake) as SharedLink;
        let (wake, woken) = waking();
        let sends = Sends::new(Some(link), Some(spool), wake);
        assert_eq!(
            sends.send("device-phone", shot.clone()),
            None,
            "taken to be staged off the session's pass"
        );
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the send is answered");
        assert_eq!(sends.take(), Some(Sent::offered("Ada's phone".into())));

        let actions = fake.actions();
        assert_eq!(actions.len(), 1, "one offer, and nothing else: {actions:?}");
        let (target, offer) = actions.into_iter().next().expect("the offer");
        assert_eq!(target.as_str(), "device-phone");
        let PersonalFabricAction::NearbyTransferOffer {
            transfer_id,
            name,
            kind,
            total_bytes,
            content_hash,
        } = offer
        else {
            panic!("a Nearby offer");
        };
        assert_eq!(name, "Screenshot 1.png");
        assert_eq!(kind, TransferKind::File);
        assert_eq!(
            total_bytes,
            std::fs::metadata(&shot).expect("the screenshot").len(),
            "the screenshot's own bytes"
        );
        assert_eq!(
            content_hash,
            *blake3::hash(b"the screenshot's bytes").as_bytes()
        );
        let streams = fake.streams();
        assert_eq!(
            streams.get(&transfer_id).map(Vec::as_slice),
            Some(b"the screenshot's bytes".as_slice()),
            "what leaves is what the studio wrote"
        );
    }

    #[test]
    fn a_device_that_left_is_refused_with_nothing_staged() {
        let (dir, shot, fake) = fixture();
        let spool = dir.path().join("spool");
        let link: SharedLink = fake.clone();
        let (wake, _woken) = waking();
        let sends = Sends::new(Some(link), Some(spool.clone()), wake);
        assert_eq!(
            sends.send("device-gone", shot),
            Some(Sent::refused("device-gone".into(), Refusal::Offline)),
            "the picker's id is never trusted"
        );
        assert!(fake.actions().is_empty(), "nothing was offered");
        assert!(!spool.exists(), "nothing was staged");
    }

    #[test]
    fn a_capture_that_is_gone_is_refused_and_nothing_stays_staged() {
        let (dir, _shot, fake) = fixture();
        let spool = dir.path().join("spool");
        let link: SharedLink = fake.clone();
        let (wake, woken) = waking();
        let sends = Sends::new(Some(link), Some(spool), wake);
        let gone = dir.path().join("Screenshot 2.png");
        assert_eq!(sends.send("device-phone", gone.clone()), None);
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the send is answered");
        assert_eq!(
            sends.take(),
            Some(Sent::refused(
                "Ada's phone".into(),
                Refusal::Gone { path: gone }
            ))
        );
        assert!(fake.actions().is_empty(), "nothing was offered");
        assert!(fake.streams().is_empty(), "nothing was streamed");
    }

    #[test]
    fn a_session_with_no_fabric_says_so_instead_of_sending() {
        let (dir, shot, _fake) = fixture();
        let (wake, _woken) = waking();
        let sends = Sends::new(None, Some(dir.path().join("spool")), wake);
        assert_eq!(
            sends.send("device-phone", shot),
            Some(Sent::refused("device-phone".into(), Refusal::SyncOff))
        );
    }
}
