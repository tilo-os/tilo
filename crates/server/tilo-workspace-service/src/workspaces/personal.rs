//! The person's workspace list, kept here for their keys to read
//! ([`ServiceOperation::KeepPersonalList`], [`ServiceOperation::ReadPersonalList`]).
//!
//! The person's devices hold their list: every workspace of theirs, wherever
//! it is hosted (a desktop of theirs, a paired device, a server), each with its
//! host's own signed address. One of their devices keeps a copy here, and any
//! certified key of theirs (a script, an agent's grant, the entry that serves
//! the person's agents) reads it back: how a key that runs on no device of
//! theirs finds the workspaces it may enter, and at which host.
//!
//! The same record keeps the person's devices: every key their root
//! certified, as its certificate says. Each device that signs a certificate
//! adds it: a desktop holding the root sends its fleet with its list, and
//! this host adds what it carried when the root on a page answered a pairing
//! ([`Workspaces::keep_issued_device`]), and a device of theirs that reads
//! or keeps the record is in it too, since a browser's root certifies the
//! browser in its own tab. A device is never taken out; its
//! root's revocation, which this host keeps with every other, marks it.
//!
//! The copy is the last one a device of the person kept, one file per person
//! (`personal-lists/<root>.json`). Reading it fills in the workspaces this
//! host keeps for them, as this host lists them and with its own signed
//! address, so a person with no device that kept a list still reads the
//! workspaces here. An agent's grant reads only what its scope reaches.
//!
//! A browser keeps one key per open tab, since a key has one writer; a page
//! asking for the record names its browser's tab keys, which are added and
//! kept together under the browser's id, so the list says which keys are one
//! browser's.
//!
//! [`ServiceOperation::KeepPersonalList`]: crate::service_authority::ServiceOperation::KeepPersonalList
//! [`ServiceOperation::ReadPersonalList`]: crate::service_authority::ServiceOperation::ReadPersonalList

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_server_session::workspaces::{
    KeepingDeviceAddress, MAX_BROWSER_DEVICES, MAX_PERSONAL_DEVICES, PersonalBrowser,
    PersonalDevice, PersonalList, PersonalWorkspace,
};
use tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
use tilo_user_identity::{DeviceCert, RevocationList, verify_device_cert};

use super::Workspaces;

/// The directory beside the workspaces' records the lists are kept in.
const DIR: &str = "personal-lists";

/// What is kept of one person's list: the list, the device that kept it, and
/// what the sessions of their keys said of them.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Kept {
    /// The device that kept it, hex.
    by: String,
    /// When, nanoseconds since the epoch.
    at_ns: u64,
    /// Its workspaces; the devices are kept beside it.
    list: PersonalList,
    /// Every key the person's root certified that a device added, one per
    /// key, the newest certificate of each.
    #[serde(default)]
    devices: Vec<DeviceCert>,
    /// Each browser's tab keys, hex, by the browser's id: as a page of it
    /// last named them, each one of `devices`, each in one browser at most.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    browsers: BTreeMap<String, Vec<String>>,
    /// The agent each key runs, hex, as the sessions it opened here named it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    agents: BTreeMap<String, String>,
    /// When each key was last here, hex, ms since the epoch: the last session
    /// of it that opened or ended on this host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    seen: BTreeMap<String, i64>,
    /// The name the person gave a key, hex, where they named it: what their
    /// lists show instead of the name its certificate carries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    names: BTreeMap<String, String>,
}

/// What one person's record on this host holds, as their keys read it.
#[derive(Default)]
struct Record {
    /// The workspaces their devices kept.
    list: PersonalList,
    /// Every key their root certified that was added here.
    devices: Vec<DeviceCert>,
    /// The browser each tab key is of, its id, by the key's hex.
    browser_of: BTreeMap<String, String>,
    /// The agent each key runs, by its hex.
    agents: BTreeMap<String, String>,
    /// When each key was last here, by its hex.
    seen: BTreeMap<String, i64>,
    /// The name the person gave a key, by its hex.
    names: BTreeMap<String, String>,
}

/// How often a session opening writes when a key was last here: a host serves
/// many sessions, and the record is read back rather than streamed.
const SEEN_WRITE_INTERVAL_MS: i64 = 60_000;

/// Now, ms since the epoch, as a record keeps it.
fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}

/// The lists people's devices kept on this host.
pub(super) struct PersonalLists {
    dir: PathBuf,
    /// Held while a list is written, so two keeps land one after the other.
    writing: std::sync::Mutex<()>,
}

impl PersonalLists {
    pub(super) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.join(DIR),
            writing: std::sync::Mutex::new(()),
        }
    }

    fn path(&self, person: &[u8; 32]) -> PathBuf {
        self.dir.join(format!("{}.json", hex::encode(person)))
    }

    /// Keep `list` as `device`'s person's, replacing the workspaces kept and
    /// adding the devices it carries. `revoked` is what was taken back: a
    /// device it refuses reads by no name any more, here and wherever a key
    /// is added or named.
    fn keep(
        &self,
        device: &DeviceCert,
        mut list: PersonalList,
        now: u64,
        revoked: &RevocationList,
    ) -> Result<(), String> {
        list.check()?;
        let person = device.user_public_key;
        let carried: Vec<DeviceCert> = list.devices.drain(..).map(|one| one.cert).collect();
        let _writing = self.lock();
        let mut kept = self.read(&person).unwrap_or_default();
        kept.by = hex::encode(device.device_public_key);
        kept.at_ns = now;
        kept.list = list;
        add(
            &mut kept.devices,
            &mut kept.names,
            &person,
            carried,
            revoked,
        );
        self.write(&person, &kept)
    }

    /// Add `certs`, keys `person`'s root certified, to their record, keeping
    /// whatever else it holds.
    fn add_devices(
        &self,
        person: &[u8; 32],
        certs: Vec<DeviceCert>,
        revoked: &RevocationList,
    ) -> Result<(), String> {
        let _writing = self.lock();
        let mut kept = self.read(person).unwrap_or_default();
        if !add(&mut kept.devices, &mut kept.names, person, certs, revoked) {
            return Ok(());
        }
        self.write(person, &kept)
    }

    /// Keep `browser`, the tab keys a page of `person`'s names, as that
    /// browser's: each certificate their root signed for a device of theirs
    /// is added, and the browser's keys are the ones of them their record
    /// holds, replacing what it named before.
    fn keep_browser(
        &self,
        person: &[u8; 32],
        browser: PersonalBrowser,
        revoked: &RevocationList,
    ) -> Result<(), String> {
        if !PersonalBrowser::is_id(&browser.id) {
            return Err("a browser is named by 32 lowercase hex characters".to_owned());
        }
        // The newest, when a page names more than a browser keeps.
        let older = browser.certs.len().saturating_sub(MAX_BROWSER_DEVICES);
        let certs: Vec<DeviceCert> = browser
            .certs
            .into_iter()
            .skip(older)
            .filter(|cert| cert.capabilities.agent.is_none() && !cert.capabilities.stand_in)
            .collect();
        let _writing = self.lock();
        let mut kept = self.read(person).unwrap_or_default();
        let added = add(
            &mut kept.devices,
            &mut kept.names,
            person,
            certs.clone(),
            revoked,
        );
        // Only keys the record holds, which `add` verified as the person's.
        let mut keys: Vec<String> = Vec::new();
        for cert in &certs {
            let key = hex::encode(cert.device_public_key);
            if !keys.contains(&key)
                && kept
                    .devices
                    .iter()
                    .any(|known| known.device_public_key == cert.device_public_key)
            {
                keys.push(key);
            }
        }
        // A key is one browser's: a page naming it takes it from any other.
        for (id, others) in &mut kept.browsers {
            if *id != browser.id {
                others.retain(|key| !keys.contains(key));
            }
        }
        kept.browsers.retain(|_, others| !others.is_empty());
        let changed = if keys.is_empty() {
            kept.browsers.remove(&browser.id).is_some()
        } else {
            kept.browsers.insert(browser.id, keys.clone()).as_ref() != Some(&keys)
        };
        if !added && !changed {
            return Ok(());
        }
        self.write(person, &kept)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Name `key`, a device of `person`'s, `name`: what their lists read it by
    /// from now on. A name another of their devices reads by is refused, so a
    /// list never shows two devices alike; a device that was taken back
    /// reads by none.
    fn name_device(
        &self,
        person: &[u8; 32],
        key: &str,
        name: &str,
        revoked: &RevocationList,
    ) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > MAX_CERTIFICATION_LABEL_BYTES
            || name.chars().any(char::is_control)
        {
            return Err(format!(
                "a device's name is 1 to {MAX_CERTIFICATION_LABEL_BYTES} characters, without control characters"
            ));
        }
        let _writing = self.lock();
        let mut kept = self.read(person).unwrap_or_default();
        if !kept
            .devices
            .iter()
            .any(|known| hex::encode(known.device_public_key) == key)
        {
            return Err("that key is not a device of this person's".to_owned());
        }
        let taken = kept.devices.iter().any(|known| {
            let other = hex::encode(known.device_public_key);
            other != key
                && !revoked.refuses(known)
                && kept
                    .names
                    .get(&other)
                    .map_or(known.device_label.as_str(), String::as_str)
                    == name
        });
        if taken {
            return Err(format!(
                "another of this person's devices is already called “{name}”"
            ));
        }
        if kept.names.get(key).map(String::as_str) == Some(name) {
            return Ok(());
        }
        kept.names.insert(key.to_owned(), name.to_owned());
        self.write(person, &kept)
    }

    /// The devices `person`'s record holds here, each with the name the person
    /// gave it where they named it.
    fn certs(&self, person: &[u8; 32]) -> Vec<(DeviceCert, Option<String>)> {
        self.read(person)
            .map(|kept| {
                kept.devices
                    .into_iter()
                    .map(|cert| {
                        let name = kept
                            .names
                            .get(&hex::encode(cert.device_public_key))
                            .cloned();
                        (cert, name)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn write(&self, person: &[u8; 32], kept: &Kept) -> Result<(), String> {
        let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let path = self.path(person);
        let staged = path.with_extension("json.new");
        std::fs::write(&staged, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&staged, &path).map_err(|error| error.to_string())
    }

    /// What `person`'s devices kept here; `None` when none did.
    fn read(&self, person: &[u8; 32]) -> Option<Kept> {
        let Ok(bytes) = std::fs::read(self.path(person)) else {
            return None;
        };
        match serde_json::from_slice::<Kept>(&bytes) {
            Ok(kept) => Some(kept),
            Err(error) => {
                tracing::warn!(%error, "a kept workspace list does not read; answering none");
                None
            }
        }
    }

    /// What `person`'s record here holds: the list their devices kept last,
    /// their devices, which browser each tab key is of, the agent each key
    /// runs and when each was last here; empty when they kept no list.
    fn of(&self, person: &[u8; 32]) -> Record {
        let Some(kept) = self.read(person) else {
            return Record::default();
        };
        Record {
            list: kept.list,
            devices: kept.devices,
            browser_of: kept
                .browsers
                .into_iter()
                .flat_map(|(id, keys)| keys.into_iter().map(move |key| (key, id.clone())))
                .collect(),
            agents: kept.agents,
            seen: kept.seen,
            names: kept.names,
        }
    }

    /// Say that `cert`, a key of `person`'s, was here at `at_ms`, a session of
    /// it opening (`opening`) or ending. Only a record that holds the key says
    /// it: this writes one key's time, never a session's worth of state, and a
    /// key that has no record here was never one of the person's they kept.
    /// An opening is written at most once a minute; an ending always is, so
    /// the record keeps when the device left even right after it came.
    fn note_seen(&self, person: &[u8; 32], cert: &DeviceCert, at_ms: i64, opening: bool) {
        let _writing = self.lock();
        let Some(mut kept) = self.read(person) else {
            return;
        };
        if !kept
            .devices
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key)
        {
            return;
        }
        let key = hex::encode(cert.device_public_key);
        if opening
            && kept
                .seen
                .get(&key)
                .is_some_and(|seen| at_ms.saturating_sub(*seen) < SEEN_WRITE_INTERVAL_MS)
        {
            return;
        }
        if kept.seen.get(&key) == Some(&at_ms) {
            return;
        }
        kept.seen.insert(key, at_ms);
        if let Err(error) = self.write(person, &kept) {
            tracing::debug!(%error, "a key's session is not in the person's record");
        }
    }

    /// Say that `cert`, a key of `person`'s, runs the agent `name`. The key is
    /// added to their record when it is not there yet (`add` checks that their
    /// root signed it), so a computer set up with `tilo login` on any host of
    /// theirs is listed with the agent it runs.
    fn note_agent(
        &self,
        person: &[u8; 32],
        cert: &DeviceCert,
        name: &str,
        revoked: &RevocationList,
    ) {
        let _writing = self.lock();
        let mut kept = self.read(person).unwrap_or_default();
        add(
            &mut kept.devices,
            &mut kept.names,
            person,
            vec![cert.clone()],
            revoked,
        );
        let key = hex::encode(cert.device_public_key);
        if kept.agents.get(&key) == Some(&name.to_owned()) {
            return;
        }
        kept.agents.insert(key, name.to_owned());
        if let Err(error) = self.write(person, &kept) {
            tracing::debug!(%error, "the agent a key runs is not in the person's record");
        }
    }
}

/// Add each of `certs` that `person`'s root signed to `devices`: a key not
/// there yet, or a newer certificate of one that is. Whether any was.
///
/// A key that enrolls under a name another of the person's devices already
/// reads by is kept under the next free number (`Claude Code on laptop`,
/// `Claude Code on laptop 2`), so two computers set up in the same way still
/// read apart; a name the person gave the key themselves stays theirs. A
/// device that was taken back (`revoked` refuses it) reads by no name any
/// more: the next device may take the name it had.
fn add(
    devices: &mut Vec<DeviceCert>,
    names: &mut BTreeMap<String, String>,
    person: &[u8; 32],
    certs: Vec<DeviceCert>,
    revoked: &RevocationList,
) -> bool {
    let mut added = false;
    for cert in certs {
        if cert.user_public_key != *person || verify_device_cert(&cert).is_err() {
            continue;
        }
        let full = devices.len() >= MAX_PERSONAL_DEVICES;
        let key = hex::encode(cert.device_public_key);
        let kept_label = cert.device_label.clone();
        let changed = match devices
            .iter_mut()
            .find(|known| known.device_public_key == cert.device_public_key)
        {
            Some(known) if known.issued_at_ns < cert.issued_at_ns => {
                *known = cert;
                true
            }
            None if !full => {
                devices.push(cert);
                true
            }
            Some(_) | None => false,
        };
        if !changed {
            continue;
        }
        added = true;
        if names.contains_key(&key) {
            continue;
        }
        let taken = |name: &str| {
            devices.iter().any(|known| {
                let other = hex::encode(known.device_public_key);
                other != key
                    && !revoked.refuses(known)
                    && names
                        .get(&other)
                        .map_or(known.device_label.as_str(), String::as_str)
                        == name
            })
        };
        if taken(&kept_label) {
            let free = (2..=MAX_PERSONAL_DEVICES)
                .map(|number| format!("{kept_label} {number}"))
                .find(|candidate| !taken(candidate));
            if let Some(free) = free {
                names.insert(key, free);
            }
        }
    }
    added
}

/// The sessions of people's devices on this host, each sent its person's
/// workspace list whenever it changes: what the peer, its folders and the page
/// read as it happens, rather than asking the host again and again.
pub(super) struct PersonalPushes {
    /// What names each watch, so the session ending takes its own away.
    next: std::sync::atomic::AtomicU64,
    sessions: std::sync::Mutex<BTreeMap<u64, Watched>>,
    /// Told whenever a list changed, so the sessions are sent the new ones.
    changed: Arc<tokio::sync::Notify>,
}

/// One device's session here, and where the lists its person's host keeps are
/// pushed to it: the session's own outbound queue.
struct Watched {
    cert: DeviceCert,
    outbound: tokio::sync::mpsc::Sender<crate::transport::Envelope>,
}

impl PersonalPushes {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Watched>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for PersonalPushes {
    fn default() -> Self {
        Self {
            next: std::sync::atomic::AtomicU64::new(1),
            sessions: std::sync::Mutex::default(),
            changed: Arc::default(),
        }
    }
}

impl Workspaces {
    /// Send `device`'s person's workspace list to this session of theirs
    /// whenever it changes, answering the watch to end with
    /// [`Self::unwatch_personal`] as the session ends.
    pub fn watch_personal(
        &self,
        device: &DeviceCert,
        outbound: tokio::sync::mpsc::Sender<crate::transport::Envelope>,
    ) -> u64 {
        let watch = self
            .inner
            .personal_pushes
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.personal_pushes.lock().insert(
            watch,
            Watched {
                cert: device.clone(),
                outbound,
            },
        );
        watch
    }

    /// Stop sending the list to the session `watch` names.
    pub fn unwatch_personal(&self, watch: u64) {
        self.inner.personal_pushes.lock().remove(&watch);
    }

    /// A person's workspace list changed: every session of their devices is
    /// sent the new one.
    pub(super) fn personal_changed(&self) {
        self.inner.personal_pushes.changed.notify_one();
    }

    /// Follow the change of every person's list: each is sent the new one, as
    /// often as it changes.
    pub(super) fn follow_personal_changes(&self) {
        let workspaces = Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let changed = workspaces
                .upgrade()
                .map(|inner| Arc::clone(&inner.personal_pushes.changed));
            let Some(changed) = changed else { return };
            loop {
                changed.notified().await;
                let Some(inner) = workspaces.upgrade() else {
                    return;
                };
                Self { inner }.push_personal_lists().await;
            }
        });
    }

    /// Send every watched session its person's workspace list as it is now.
    async fn push_personal_lists(&self) {
        let watched: Vec<(u64, Watched)> = {
            let sessions = self.inner.personal_pushes.lock();
            sessions
                .iter()
                .map(|(watch, session)| {
                    (
                        *watch,
                        Watched {
                            cert: session.cert.clone(),
                            outbound: session.outbound.clone(),
                        },
                    )
                })
                .collect()
        };
        for (watch, session) in watched {
            let Ok(list) = serde_json::to_vec(&self.personal_of(&session.cert).await) else {
                continue;
            };
            match session
                .outbound
                .try_send(crate::transport::Envelope::PersonalList { list })
            {
                // A session already busy with an older list reads the next
                // change's; one whose connection ended holds nothing.
                Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    self.inner.personal_pushes.lock().remove(&watch);
                }
            }
        }
    }

    /// Keep `list` as `device`'s person's list, already permitted to.
    pub(super) fn keep_personal(
        &self,
        device: &DeviceCert,
        list: PersonalList,
    ) -> Result<usize, String> {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return Err("only a device of the person's own keeps their workspace list".to_owned());
        }
        let count = list.workspaces.len();
        self.inner
            .personal
            .keep(device, list, tilo_clock::unix_now_ns(), &self.revocations())?;
        // Every other device of theirs reads the list it kept.
        self.personal_changed();
        Ok(count)
    }

    /// Name `key`, one of `device`'s person's devices, `name`: what their
    /// lists read it by from now on, instead of the name it enrolled with.
    pub(super) fn name_device(
        &self,
        device: &DeviceCert,
        key: &str,
        name: &str,
    ) -> Result<(), String> {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return Err("only a device of the person's own names their devices".to_owned());
        }
        self.inner
            .personal
            .name_device(&device.user_public_key, key, name, &self.revocations())?;
        // What every list of theirs shows the device as.
        self.personal_changed();
        Ok(())
    }

    /// Whether another device of `person`'s reads by `label`: one their record
    /// here holds, which a revocation this host keeps has not refused. What
    /// the host reads as it names a device pairing with them, so no two
    /// devices of one person read alike.
    #[must_use]
    pub fn device_label_taken(&self, person: &[u8; 32], label: &str) -> bool {
        let revocations = self.revocations();
        self.inner
            .personal
            .certs(person)
            .into_iter()
            .any(|(cert, name)| {
                !revocations.refuses(&cert)
                    && name.as_deref().unwrap_or(cert.device_label.as_str()) == label
            })
    }

    /// `device`'s person's list as `device` reads it: the one their devices
    /// kept here, in their order, each workspace this host keeps for them
    /// named and placed as this host knows it, then the ones here the list
    /// does not name yet; limited to what an agent's grant reaches.
    pub(super) fn personal(
        &self,
        device: &DeviceCert,
        here: &[tilo_server_session::workspaces::WorkspaceListing],
        only: &super::actions::host::Listed,
    ) -> PersonalList {
        let name = self.host_name();
        let mut local: Vec<PersonalWorkspace> = Vec::new();
        for listed in here {
            // The reader reached this host to ask: it names itself, and signs
            // no address a person with hundreds of workspaces here would wait on.
            let host = Some(KeepingDeviceAddress {
                name: name.clone(),
                hint: None,
            });
            local.push(PersonalWorkspace {
                workspace: listed.workspace.clone(),
                name: listed.name.clone(),
                host,
                role: listed.role.clone(),
                labels: listed.labels.clone(),
                unattended: listed.unattended,
            });
        }
        let reaches = |workspace: &str| {
            device
                .capabilities
                .agent
                .is_none_or(|scope| scope.reaches(workspace))
        };
        // Only those the listing names, when it names some.
        let wanted = |kept: &PersonalWorkspace| only.holds(&kept.workspace, &kept.labels);
        let Record {
            list,
            devices: certs,
            browser_of,
            agents,
            seen,
            names,
        } = self.inner.personal.of(&device.user_public_key);
        let mut workspaces: Vec<PersonalWorkspace> = Vec::new();
        for kept in list.workspaces {
            if !reaches(&kept.workspace)
                || !wanted(&kept)
                || workspaces
                    .iter()
                    .any(|known| known.workspace == kept.workspace)
            {
                continue;
            }
            match local.iter().position(|one| one.workspace == kept.workspace) {
                Some(at) => workspaces.push(local.remove(at)),
                None => workspaces.push(kept),
            }
        }
        workspaces.extend(local);
        // The person's own keys read their devices; an agent's grant reads
        // the workspaces it may enter, and nothing of the person's devices.
        let devices = if device.capabilities.agent.is_some() {
            Vec::new()
        } else {
            let revocations = self.revocations();
            let online = self.inner.online.lock_recover();
            certs
                .into_iter()
                .map(|cert| {
                    let key = hex::encode(cert.device_public_key);
                    PersonalDevice {
                        name: names.get(&key).cloned(),
                        revoked: revocations.refuses(&cert),
                        browser: browser_of.get(&key).cloned(),
                        agent: agents.get(&key).cloned(),
                        last_active_ms: seen.get(&key).copied(),
                        online: online.get(&cert.device_public_key).copied().unwrap_or(0) > 0,
                        cert,
                    }
                })
                .collect()
        };
        PersonalList {
            workspaces,
            devices,
        }
    }

    /// Add `device`, one of its person's own devices that asked for their
    /// record, to it: a browser's root certifies the browser itself, in the
    /// tab, so its certificate reaches no host but on the browser's own
    /// session. An agent's grant and a stand-in are no device of theirs.
    pub(super) fn note_own_device(&self, device: &DeviceCert) {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return;
        }
        self.keep_issued_device(device, "");
    }

    /// Keep `browser`, the tab keys a page of `device`'s person names as it
    /// asks for their record, as that browser's (see [`LIST_PERSONAL`]).
    /// Only a device of the person's own names them: an agent's grant and a
    /// stand-in are no browser of theirs.
    ///
    /// [`LIST_PERSONAL`]: tilo_server_session::workspaces::LIST_PERSONAL
    pub(super) fn note_browser(&self, device: &DeviceCert, browser: PersonalBrowser) {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return;
        }
        if let Err(error) =
            self.inner
                .personal
                .keep_browser(&device.user_public_key, browser, &self.revocations())
        {
            tracing::debug!(%error, "a page's browser is not in the person's record");
        }
    }

    /// Add `cert`, a key its person's root just certified through this host
    /// (a pairing the root answered here, an agent's grant), to their
    /// record, named `label`: the name the host gave it, so the person's
    /// lists show two devices set up alike apart.
    pub fn keep_issued_device(&self, cert: &DeviceCert, label: &str) {
        let key = hex::encode(cert.device_public_key);
        let held = self
            .inner
            .personal
            .of(&cert.user_public_key)
            .devices
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key);
        if !held {
            let added = self.inner.personal.add_devices(
                &cert.user_public_key,
                vec![cert.clone()],
                &self.revocations(),
            );
            match added {
                Ok(()) => self.personal_changed(),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "a key the person's root certified here is not in their record"
                    );
                    return;
                }
            }
        }
        if label.is_empty() || cert.capabilities.agent.is_some() || cert.capabilities.stand_in {
            return;
        }
        match self.inner.personal.name_device(
            &cert.user_public_key,
            &key,
            label,
            &self.revocations(),
        ) {
            Ok(()) => self.personal_changed(),
            Err(error) => {
                tracing::warn!(%error, "a device the host named is not named in the person's record");
            }
        }
    }

    /// A session of `cert`'s device opened here: their record says the key is
    /// here now, and when it was last.
    pub(crate) fn session_opened(&self, cert: &DeviceCert) {
        self.session_opened_at(cert, now_ms());
    }

    /// The session of `cert`'s device that was open here has ended: their
    /// record says when it was here last.
    pub(crate) fn session_ended(&self, cert: &DeviceCert) {
        self.session_ended_at(cert, now_ms());
    }

    /// [`Self::session_opened`] at `at_ms`, for tests that say when.
    pub(crate) fn session_opened_at(&self, cert: &DeviceCert, at_ms: i64) {
        let mut online = self.inner.online.lock_recover();
        let count = online.entry(cert.device_public_key).or_default();
        *count = count.saturating_add(1);
        drop(online);
        self.inner
            .personal
            .note_seen(&cert.user_public_key, cert, at_ms, true);
    }

    /// [`Self::session_ended`] at `at_ms`, for tests that say when.
    pub(crate) fn session_ended_at(&self, cert: &DeviceCert, at_ms: i64) {
        if let Some(count) = self
            .inner
            .online
            .lock_recover()
            .get_mut(&cert.device_public_key)
        {
            *count = count.saturating_sub(1);
        }
        self.inner
            .personal
            .note_seen(&cert.user_public_key, cert, at_ms, false);
    }

    /// Say that `cert`, a key of its person's, runs the agent `name`: their
    /// record keeps it for every person's host, and the host's own person's
    /// MCP server lists the run.
    pub(crate) fn note_agent(
        &self,
        cert: &DeviceCert,
        name: &str,
        stop: &Arc<tokio::sync::Notify>,
    ) {
        self.inner
            .personal
            .note_agent(&cert.user_public_key, cert, name, &self.revocations());
        if cert.user_public_key == self.inner.host.cert.user_public_key
            && cert.device_public_key != self.inner.host.cert.device_public_key
        {
            self.inner.mcp_server.note(cert, name, stop);
        }
    }

    /// This host as people know it: the name it offers workspaces of their
    /// own under, else its device's label.
    fn host_name(&self) -> String {
        self.own().map_or_else(
            || self.inner.host.cert.device_label.clone(),
            |own| own.offer().host.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn key_of(root: &SigningKey, label: &str) -> DeviceCert {
        let key = SigningKey::from_bytes(&rand::random());
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: Some(tilo_clock::unix_now_ns().saturating_add(86_400_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn name_of(lists: &PersonalLists, person: &[u8; 32], cert: &DeviceCert) -> Option<String> {
        let key = hex::encode(cert.device_public_key);
        lists
            .of(person)
            .devices
            .iter()
            .find(|known| known.device_public_key == cert.device_public_key)
            .map(|_| {
                lists
                    .of(person)
                    .names
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| cert.device_label.clone())
            })
    }

    #[test]
    fn two_computers_enrolled_under_one_name_read_apart_and_the_person_names_them() {
        let dir = tempfile::tempdir().unwrap();
        let lists = PersonalLists::new(dir.path());
        let none = RevocationList::new();
        let root = SigningKey::from_bytes(&rand::random());
        let person = root.verifying_key().to_bytes();
        let desktop = key_of(&root, "studio");
        let first = key_of(&root, "Claude Code on laptop");
        let second = key_of(&root, "Claude Code on laptop");
        let list = PersonalList {
            devices: vec![PersonalDevice {
                cert: first.clone(),
                name: None,
                revoked: false,
                browser: None,
                agent: None,
                last_active_ms: None,
                online: false,
            }],
            ..PersonalList::default()
        };
        lists.keep(&desktop, list, 1, &none).unwrap();
        lists
            .add_devices(&person, vec![second.clone()], &none)
            .unwrap();

        assert_eq!(
            name_of(&lists, &person, &first).as_deref(),
            Some("Claude Code on laptop"),
            "the first keeps the name it enrolled with"
        );
        assert_eq!(
            name_of(&lists, &person, &second).as_deref(),
            Some("Claude Code on laptop 2"),
            "the second reads apart from it"
        );

        // The person names it what they know it as.
        lists
            .name_device(
                &person,
                &hex::encode(second.device_public_key),
                "Build server",
                &none,
            )
            .unwrap();
        assert_eq!(
            name_of(&lists, &person, &second).as_deref(),
            Some("Build server")
        );
        assert_eq!(
            name_of(&lists, &person, &first).as_deref(),
            Some("Claude Code on laptop"),
            "naming one leaves the other as it was"
        );

        // A name another of their devices reads by is refused, and so is a
        // key that is not theirs.
        assert!(
            lists
                .name_device(
                    &person,
                    &hex::encode(second.device_public_key),
                    "Claude Code on laptop",
                    &none
                )
                .is_err(),
            "two devices never read alike"
        );
        assert!(
            lists
                .name_device(&person, &"00".repeat(32), "Nowhere", &none)
                .is_err(),
            "only a device of the person's is named"
        );
        assert!(
            lists
                .name_device(&person, &hex::encode(second.device_public_key), "  ", &none)
                .is_err(),
            "a device needs a name"
        );
    }

    #[test]
    fn a_name_a_device_that_was_taken_back_read_by_is_free_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let lists = PersonalLists::new(dir.path());
        let mut revoked = RevocationList::new();
        let root = SigningKey::from_bytes(&rand::random());
        let person = root.verifying_key().to_bytes();
        let first = key_of(&root, "Chrome on Mac");
        let second = key_of(&root, "Chrome on Mac");
        lists
            .add_devices(&person, vec![first.clone(), second.clone()], &revoked)
            .unwrap();
        assert_eq!(
            name_of(&lists, &person, &second).as_deref(),
            Some("Chrome on Mac 2")
        );

        // The person takes the second browser back.
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: person,
            revoked_device_public_key: second.device_public_key,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        revoked.insert(entry).unwrap();

        // The next browser pairs under the lowest free number, the one the
        // removed browser read by. It was then named a second time on top of
        // that, "Chrome on Mac 2 2", and the one after "Chrome on Mac 2 3".
        let third = key_of(&root, "Chrome on Mac 2");
        lists
            .add_devices(&person, vec![third.clone()], &revoked)
            .unwrap();
        assert_eq!(
            name_of(&lists, &person, &third).as_deref(),
            Some("Chrome on Mac 2"),
            "a device is named once"
        );
        // The name it was given as it paired is kept as its name.
        lists
            .name_device(
                &person,
                &hex::encode(third.device_public_key),
                "Chrome on Mac 2",
                &revoked,
            )
            .unwrap();
        // One more browser of the same kind takes the lowest free number.
        let fourth = key_of(&root, "Chrome on Mac");
        lists
            .add_devices(&person, vec![fourth.clone()], &revoked)
            .unwrap();
        assert_eq!(
            name_of(&lists, &person, &fourth).as_deref(),
            Some("Chrome on Mac 3")
        );
        assert_eq!(
            name_of(&lists, &person, &first).as_deref(),
            Some("Chrome on Mac"),
            "the first keeps the name it enrolled with"
        );
    }
}
