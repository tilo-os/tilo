//! The person's workspace list, kept here for their keys to read
//! ([`HostOperation::KeepPersonalList`], [`HostOperation::ReadPersonalList`]).
//!
//! The person's devices hold their list: every workspace of theirs, wherever
//! it is hosted (a desktop of theirs, a paired device, a relay), each with its
//! host's own signed address. One of their devices keeps a copy here, and any
//! certified key of theirs (a script, an agent's grant, the door that serves
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
//! [`HostOperation::KeepPersonalList`]: crate::host_authority::HostOperation::KeepPersonalList
//! [`HostOperation::ReadPersonalList`]: crate::host_authority::HostOperation::ReadPersonalList

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tilo_relay_session::workspaces::{
    MAX_BROWSER_DEVICES, MAX_PERSONAL_DEVICES, PersonalBrowser, PersonalDevice, PersonalList,
    PersonalWorkspace, WorkspaceHostAddress,
};
use tilo_user_identity::{DeviceCert, verify_device_cert};

use super::Workspaces;

/// The directory beside the workspaces' records the lists are kept in.
const DIR: &str = "personal-lists";

/// What is kept of one person's list: the list, and the device that kept it.
#[derive(serde::Serialize, serde::Deserialize)]
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
    /// adding the devices it carries.
    fn keep(&self, device: &DeviceCert, mut list: PersonalList, now: u64) -> Result<(), String> {
        list.check()?;
        let person = device.user_public_key;
        let carried: Vec<DeviceCert> = list.devices.drain(..).map(|one| one.cert).collect();
        let _writing = self.lock();
        let mut kept = self.read(&person).unwrap_or_else(|| Kept {
            by: String::new(),
            at_ns: 0,
            list: PersonalList::default(),
            devices: Vec::new(),
            browsers: BTreeMap::new(),
        });
        kept.by = hex::encode(device.device_public_key);
        kept.at_ns = now;
        kept.list = list;
        add(&mut kept.devices, &person, carried);
        self.write(&person, &kept)
    }

    /// Add `certs`, keys `person`'s root certified, to their record, keeping
    /// whatever else it holds.
    fn add_devices(&self, person: &[u8; 32], certs: Vec<DeviceCert>) -> Result<(), String> {
        let _writing = self.lock();
        let mut kept = self.read(person).unwrap_or_else(|| Kept {
            by: String::new(),
            at_ns: 0,
            list: PersonalList::default(),
            devices: Vec::new(),
            browsers: BTreeMap::new(),
        });
        if !add(&mut kept.devices, person, certs) {
            return Ok(());
        }
        self.write(person, &kept)
    }

    /// Keep `browser`, the tab keys a page of `person`'s names, as that
    /// browser's: each certificate their root signed for a device of theirs
    /// is added, and the browser's keys are the ones of them their record
    /// holds, replacing what it named before.
    fn keep_browser(&self, person: &[u8; 32], browser: PersonalBrowser) -> Result<(), String> {
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
        let mut kept = self.read(person).unwrap_or_else(|| Kept {
            by: String::new(),
            at_ns: 0,
            list: PersonalList::default(),
            devices: Vec::new(),
            browsers: BTreeMap::new(),
        });
        let added = add(&mut kept.devices, person, certs.clone());
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

    /// The list `person`'s devices kept here last, their devices, and which
    /// browser each tab key is of; empty when none did.
    fn of(&self, person: &[u8; 32]) -> (PersonalList, Vec<DeviceCert>, BTreeMap<String, String>) {
        self.read(person)
            .map(|kept| {
                let browser_of = kept
                    .browsers
                    .into_iter()
                    .flat_map(|(id, keys)| keys.into_iter().map(move |key| (key, id.clone())))
                    .collect();
                (kept.list, kept.devices, browser_of)
            })
            .unwrap_or_default()
    }
}

/// Add each of `certs` that `person`'s root signed to `devices`: a key not
/// there yet, or a newer certificate of one that is. Whether any was.
fn add(devices: &mut Vec<DeviceCert>, person: &[u8; 32], certs: Vec<DeviceCert>) -> bool {
    let mut added = false;
    for cert in certs {
        if cert.user_public_key != *person || verify_device_cert(&cert).is_err() {
            continue;
        }
        let full = devices.len() >= MAX_PERSONAL_DEVICES;
        match devices
            .iter_mut()
            .find(|known| known.device_public_key == cert.device_public_key)
        {
            Some(known) if known.issued_at_ns < cert.issued_at_ns => {
                *known = cert;
                added = true;
            }
            None if !full => {
                devices.push(cert);
                added = true;
            }
            Some(_) | None => {}
        }
    }
    added
}

impl Workspaces {
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
            .keep(device, list, tilo_clock::unix_now_ns())?;
        Ok(count)
    }

    /// `device`'s person's list as `device` reads it: the one their devices
    /// kept here, in their order, each workspace this host keeps for them
    /// named and placed as this host knows it, then the ones here the list
    /// does not name yet; limited to what an agent's grant reaches.
    pub(super) fn personal(
        &self,
        device: &DeviceCert,
        here: &[tilo_relay_session::workspaces::WorkspaceListing],
    ) -> PersonalList {
        let name = self.host_name();
        let mut local: Vec<PersonalWorkspace> = Vec::new();
        for listed in here {
            // The reader reached this host to ask: it names itself, and signs
            // no address a person with hundreds of workspaces here would wait on.
            let host = Some(WorkspaceHostAddress {
                name: name.clone(),
                hint: None,
            });
            local.push(PersonalWorkspace {
                workspace: listed.workspace.clone(),
                name: listed.name.clone(),
                host,
                role: listed.role.clone(),
            });
        }
        let reaches = |workspace: &str| {
            device
                .capabilities
                .agent
                .is_none_or(|scope| scope.reaches(workspace))
        };
        let (list, certs, browser_of) = self.inner.personal.of(&device.user_public_key);
        let mut workspaces: Vec<PersonalWorkspace> = Vec::new();
        for kept in list.workspaces {
            if !reaches(&kept.workspace)
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
            certs
                .into_iter()
                .map(|cert| PersonalDevice {
                    revoked: revocations.refuses(&cert),
                    browser: browser_of
                        .get(&hex::encode(cert.device_public_key))
                        .cloned(),
                    cert,
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
        self.keep_issued_device(device);
    }

    /// Keep `browser`, the tab keys a page of `device`'s person names as it
    /// asks for their record, as that browser's (see [`LIST_PERSONAL`]).
    /// Only a device of the person's own names them: an agent's grant and a
    /// stand-in are no browser of theirs.
    ///
    /// [`LIST_PERSONAL`]: tilo_relay_session::workspaces::LIST_PERSONAL
    pub(super) fn note_browser(&self, device: &DeviceCert, browser: PersonalBrowser) {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return;
        }
        if let Err(error) = self
            .inner
            .personal
            .keep_browser(&device.user_public_key, browser)
        {
            tracing::debug!(%error, "a page's browser is not in the person's record");
        }
    }

    /// Add `cert`, a key its person's root just certified through this host
    /// (a pairing the root answered here, an agent's grant), to their
    /// record: the root that signed it is on a page, which keeps no record
    /// of its own.
    pub fn keep_issued_device(&self, cert: &DeviceCert) {
        if let Err(error) = self
            .inner
            .personal
            .add_devices(&cert.user_public_key, vec![cert.clone()])
        {
            tracing::warn!(%error, "a key the person's root certified here is not in their record");
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
