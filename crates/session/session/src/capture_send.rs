//! Sending the last screenshot: to one of the person's other devices, or into this workspace's
//! files.
//! A device send is an ordinary Nearby Send, so it goes through the session's one transfer engine:
//! the screenshot is staged into this device's private spool, offered to the device as a fabric
//! action, and its bytes stream from that spool; the receiving device's person still decides where
//! it lands there. A workspace save copies the screenshot into the workspace's `Screenshots` folder
//! through the same engine every other copy takes, where the person's collaborators see it like
//! anything else placed there. That copy reads and writes files - possibly over a remote workspace
//! mount - so it happens on a worker of its own, never on the session's pass: the pass hands the
//! worker the save and takes what became of it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::platform::fabric::DeviceId;
use tilo_capture_studio::{Refusal, Sent};
use tilo_service::Wake;

use host_fs::transfer::Failure;

use crate::nearby_transfer::NearbyTransfer;

/// Folder inside the workspace files that sent screenshots land in.
pub(crate) const WORKSPACE_SCREENSHOTS_DIR: &str = "Screenshots";

/// The screenshots on their way to the person's other devices and into the workspace's files, and
/// what became of the last one.
pub(crate) struct Sends {
    /// The session's transfer engine, when this session runs on a device fabric: every send to
    /// another device is one of its transfers.
    nearby: Option<Arc<NearbyTransfer>>,
    /// The worker that writes into the workspace's files; `None` when there is no workspace or its
    /// worker could not start.
    jobs: Option<Sender<Job>>,
    /// The workspace's files root, when one is mounted for this session.
    workspace: Option<PathBuf>,
    /// Where the engine and the worker report what became of a send.
    answers: Sender<Sent>,
    /// What became of each send, as the pass polls for it.
    done: Receiver<Sent>,
    /// This session's wake, turned when the engine or the worker answers.
    wake: Wake,
}

/// One save, as the pass hands it to the worker.
enum Job {
    /// Copy the screenshot at `path` into the files under `root` .
    Workspace { path: PathBuf, root: PathBuf },
}

impl Sends {
    /// The sends of a session whose transfers go through `nearby` and whose workspace saves land
    /// under `workspace` .
    pub(crate) fn new(
        nearby: Option<Arc<NearbyTransfer>>,
        workspace: Option<PathBuf>,
        wake: Wake,
    ) -> Self {
        let (jobs, mailbox) = channel();
        let (answers, done) = channel();
        let mut worker = None;
        if workspace.is_some() {
            let answers = answers.clone();
            let waking = Arc::clone(&wake);
            let spawned = std::thread::Builder::new()
                .name("tilo-capture-send".into())
                .spawn(move || work(mailbox, answers, waking));
            match spawned {
                Ok(_) => worker = Some(jobs),
                Err(error) => {
                    tracing::warn!(%error, "no thread takes what a screenshot is saved by");
                }
            }
        }
        Self {
            nearby,
            jobs: worker,
            workspace,
            answers,
            done,
            wake,
        }
    }

    /// Offer the screenshot at `path` to the device the person picked, by the id the studio listed.
    /// The device is resolved again here: the picker's id is never trusted, and one that left since
    /// is refused at once.
    /// Returns what became of it when it was refused before anything was staged; the engine reports
    /// the rest through [`Self::take`].
    pub(crate) fn send(&self, id: &str, path: PathBuf) -> Option<Sent> {
        let Some(nearby) = &self.nearby else {
            return Some(Sent::refused(id.to_owned(), Refusal::SyncOff));
        };
        if !nearby.has_link() {
            return Some(Sent::refused(id.to_owned(), Refusal::SyncOff));
        }
        let Some((device, label)) = nearby
            .devices()
            .into_iter()
            .find(|device| device.id == id)
            .and_then(|device| DeviceId::new(&device.id).ok().map(|id| (id, device.label)))
        else {
            return Some(Sent::refused(id.to_owned(), Refusal::Offline));
        };
        let answers = self.answers.clone();
        let wake = Arc::clone(&self.wake);
        nearby.studio_send(
            device,
            label,
            path,
            Box::new(move |sent| {
                let _ = answers.send(sent);
                wake();
            }),
        )
    }

    /// Copy the screenshot at `path` into this workspace's files, on the worker: the mount may be
    /// remote, and its write has no place on the pass.
    /// Returns what became of it when the worker cannot take it; the worker reports the rest
    /// through [`Self::take`].
    pub(crate) fn send_to_workspace(&self, path: PathBuf) -> Option<Sent> {
        let Some(root) = &self.workspace else {
            return Some(Sent::not_saved(Refusal::NoWorkspace));
        };
        let Some(jobs) = &self.jobs else {
            return Some(Sent::not_saved(Refusal::NotSaved(
                "the send worker is not running".to_owned(),
            )));
        };
        if jobs
            .send(Job::Workspace {
                path,
                root: root.clone(),
            })
            .is_err()
        {
            return Some(Sent::not_saved(Refusal::NotSaved(
                "the send worker stopped".to_owned(),
            )));
        }
        None
    }

    /// What became of a send since the last pass.
    pub(crate) fn take(&self) -> Option<Sent> {
        self.done.try_recv().ok()
    }
}

/// Take every save the pass hands over and write it, until the pass is gone.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the thread owns its end of the channel and the session's wake"
)]
fn work(jobs: Receiver<Job>, done: Sender<Sent>, wake: Wake) {
    while let Ok(job) = jobs.recv() {
        let sent = match job {
            Job::Workspace { path, root } => save(&path, &root),
        };
        if done.send(sent).is_err() {
            return;
        }
        wake();
    }
}

/// Copy the screenshot at `path` into the files under `root` , and say the name it landed under.
fn save(path: &Path, root: &Path) -> Sent {
    if !path.is_file() {
        return Sent::not_saved(Refusal::Gone {
            path: path.to_path_buf(),
        });
    }
    match into_workspace(path, root) {
        Ok(saved) => Sent::saved(in_workspace_name(&saved)),
        Err(why) => Sent::not_saved(why),
    }
}

/// How the receipt names the copy at `saved` : its name in the folder.
fn in_workspace_name(saved: &Path) -> String {
    saved.file_name().map_or_else(
        || saved.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Copy `source` into `root`'s [`WORKSPACE_SCREENSHOTS_DIR`] folder through the
/// session's one transfer engine: it is written privately in that folder,
/// flushed, and only then takes its own name, or the next free numbered one.
/// Two sends never replace each other, and a send cut off part-way leaves no
/// half screenshot under a name a later send would take for whole.
fn into_workspace(source: &Path, root: &Path) -> Result<PathBuf, Refusal> {
    let folder = root.join(WORKSPACE_SCREENSHOTS_DIR);
    std::fs::create_dir_all(&folder).map_err(|error| Refusal::CreateFailed {
        folder: folder.clone(),
        error: error.to_string(),
    })?;
    host_fs::transfer::transfer(source, &folder, false, &mut ()).map_err(|failure| match failure {
        Failure::NoSourceName => Refusal::NoFileName {
            path: source.to_path_buf(),
        },
        Failure::NamesExhausted { .. } => Refusal::NoFreeName { folder },
        failure => Refusal::NotSaved(failure.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::platform::fabric::{
        DeviceFabric, DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
        FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
    };
    use crate::platform::fabric::{
        FabricLink, NearbySpoolOffer, PersonalFabricSendError, SharedLink,
    };
    use tilo_nearby_service::TransferKind;

    use super::*;
    use crate::platform::fabric::Continuity;

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
            unlock: crate::platform::fabric::UnlockFabricState::default(),
            speakers: crate::platform::fabric::SpeakerFabricState::default(),
        }
    }

    /// The person's fabric as a session sees it, recording what it is asked to carry instead of
    /// carrying it.
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

    /// What the person's settings say with nothing turned off: every facet of continuity on.
    fn permitted() -> Continuity {
        let continuity = Continuity::default();
        let document: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({"continuity": {"enabled": true, "file_transfer": true}}),
        )
        .expect("a settings document");
        continuity.read(&document);
        continuity
    }

    /// The sends of a session on `link` , staging into `spool` , with a real transfer engine behind
    /// them.
    fn sending(
        link: Option<SharedLink>,
        spool: Option<PathBuf>,
        workspace: Option<PathBuf>,
        wake: Wake,
    ) -> Sends {
        let nearby = NearbyTransfer::open(link, spool, permitted(), Arc::clone(&wake))
            .expect("a transfer engine");
        Sends::new(Some(nearby), workspace, wake)
    }

    #[test]
    fn the_picker_lists_the_other_devices_that_are_here() {
        let (dir, _shot, fake) = fixture();
        let (wake, _woken) = waking();
        let sends = sending(
            Some(Arc::clone(&fake) as SharedLink),
            Some(dir.path().join("spool")),
            None,
            wake,
        );
        assert_eq!(
            sends
                .nearby
                .as_ref()
                .map(|nearby| nearby.devices())
                .unwrap_or_default(),
            vec![ui::widget::ScreenshotSendDevice {
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
        let sends = sending(Some(link), Some(spool), None, wake);
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
        let sends = sending(Some(link), Some(spool.clone()), None, wake);
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
        let sends = sending(Some(link), Some(spool), None, wake);
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
        let sends = Sends::new(None, None, wake);
        assert_eq!(
            sends.send("device-phone", shot),
            Some(Sent::refused("device-phone".into(), Refusal::SyncOff))
        );
        assert!(
            !dir.path().join("spool").exists(),
            "a session with no engine stages nothing"
        );
    }

    #[test]
    fn a_session_whose_files_do_not_leave_says_so() {
        let (dir, shot, fake) = fixture();
        let link: SharedLink = fake.clone();
        let (wake, _woken) = waking();
        let nearby = NearbyTransfer::open(
            Some(link),
            Some(dir.path().join("spool")),
            Continuity::default(),
            Arc::clone(&wake),
        )
        .expect("a transfer engine");
        let sends = Sends::new(Some(nearby), None, wake);
        assert_eq!(
            sends.send("device-phone", shot),
            Some(Sent::refused("Ada's phone".into(), Refusal::SyncOff)),
            "the person turned file transfer off"
        );
        assert!(fake.actions().is_empty(), "nothing was offered");
    }

    #[test]
    fn a_screenshot_is_copied_into_the_workspace_under_its_own_name() {
        let (_dir, shot, _fake) = fixture();
        let workspace = tempfile::tempdir().expect("a workspace");
        let (wake, woken) = waking();
        let sends = Sends::new(
            None,
            Some(workspace.path().to_path_buf()),
            Arc::clone(&wake),
        );
        assert_eq!(
            sends.send_to_workspace(shot.clone()),
            None,
            "copied off the session's pass: the mount may be remote"
        );
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the save is answered");
        assert_eq!(sends.take(), Some(Sent::saved("Screenshot 1.png".into())));
        let saved = workspace
            .path()
            .join("Screenshots")
            .join("Screenshot 1.png");
        assert_eq!(
            std::fs::read(&saved).expect("the copy"),
            b"the screenshot's bytes",
            "what lands is what the studio wrote"
        );

        // A second send of a file of the same name is a second screenshot: it lands beside the
        // first instead of replacing it.
        let sends = Sends::new(
            None,
            Some(workspace.path().to_path_buf()),
            Arc::clone(&wake),
        );
        assert_eq!(sends.send_to_workspace(shot), None);
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the second save is answered");
        assert_eq!(sends.take(), Some(Sent::saved("Screenshot 1 2.png".into())));
        assert_eq!(
            std::fs::read(&saved).expect("the first copy"),
            b"the screenshot's bytes",
            "the first copy is untouched"
        );
        assert!(
            workspace
                .path()
                .join("Screenshots")
                .join("Screenshot 1 2.png")
                .is_file()
        );
        let mut names: Vec<String> = std::fs::read_dir(workspace.path().join("Screenshots"))
            .expect("the folder")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "Screenshot 1 2.png".to_owned(),
                "Screenshot 1.png".to_owned()
            ],
            "nothing is left staged beside the copies"
        );
    }

    #[test]
    fn a_send_whose_predecessor_was_cut_off_still_lands_whole() {
        let (_dir, shot, _fake) = fixture();
        let workspace = tempfile::tempdir().expect("a workspace");
        let folder = workspace.path().join("Screenshots");
        std::fs::create_dir_all(folder.join(".tilo-transfer-cut-off")).expect("a cut-off staging");
        std::fs::write(
            folder.join(".tilo-transfer-cut-off/payload"),
            b"half a screenshot",
        )
        .expect("a half copy");
        let (wake, woken) = waking();
        let sends = Sends::new(
            None,
            Some(workspace.path().to_path_buf()),
            Arc::clone(&wake),
        );
        assert_eq!(sends.send_to_workspace(shot), None);
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the save is answered");
        assert_eq!(sends.take(), Some(Sent::saved("Screenshot 1.png".into())));
        assert_eq!(
            std::fs::read(folder.join("Screenshot 1.png")).expect("the copy"),
            b"the screenshot's bytes"
        );
        assert_eq!(
            std::fs::read(folder.join(".tilo-transfer-cut-off/payload")).expect("the half copy"),
            b"half a screenshot",
            "a copy cut off part-way is never published"
        );
    }

    #[test]
    fn a_screenshot_is_not_saved_when_its_folder_cannot_be_made() {
        let (_dir, shot, _fake) = fixture();
        let workspace = tempfile::tempdir().expect("a workspace");
        std::fs::write(workspace.path().join("Screenshots"), b"in the way").expect("a file");
        let (wake, woken) = waking();
        let sends = Sends::new(
            None,
            Some(workspace.path().to_path_buf()),
            Arc::clone(&wake),
        );
        assert_eq!(sends.send_to_workspace(shot), None);
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the save is answered");
        assert!(
            matches!(
                sends.take(),
                Some(Sent::NotSaved {
                    why: Refusal::CreateFailed { .. }
                })
            ),
            "the folder cannot be made"
        );
        assert_eq!(
            std::fs::read(workspace.path().join("Screenshots")).expect("the file"),
            b"in the way",
            "nothing was written through it"
        );
    }

    #[test]
    fn a_session_with_no_workspace_says_so_and_writes_nothing() {
        let (dir, shot, _fake) = fixture();
        let (wake, _woken) = waking();
        let sends = Sends::new(None, None, wake);
        assert_eq!(
            sends.send_to_workspace(shot),
            Some(Sent::not_saved(Refusal::NoWorkspace)),
            "there is no workspace to save into"
        );
        assert!(
            !dir.path().join("Screenshots").exists(),
            "nothing was written"
        );
    }

    #[test]
    fn a_screenshot_that_is_gone_is_not_saved() {
        let (_dir, _shot, _fake) = fixture();
        let workspace = tempfile::tempdir().expect("a workspace");
        let gone = workspace.path().join("Screenshot 7.png");
        let (wake, woken) = waking();
        let sends = Sends::new(
            None,
            Some(workspace.path().to_path_buf()),
            Arc::clone(&wake),
        );
        assert_eq!(sends.send_to_workspace(gone.clone()), None);
        woken
            .recv_timeout(Duration::from_secs(60))
            .expect("the save is answered");
        assert_eq!(
            sends.take(),
            Some(Sent::not_saved(Refusal::Gone { path: gone }))
        );
        assert!(
            !workspace.path().join("Screenshots").exists(),
            "nothing was written"
        );
    }
}
