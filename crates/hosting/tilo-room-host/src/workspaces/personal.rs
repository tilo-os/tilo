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
//! The copy is the last one a device of the person kept, one file per person
//! (`personal-lists/<root>.json`). Reading it fills in the workspaces this
//! host keeps for them, as this host lists them and with its own signed
//! address, so a person with no device that kept a list still reads the
//! workspaces here. An agent's grant reads only what its scope reaches.
//!
//! [`HostOperation::KeepPersonalList`]: crate::host_authority::HostOperation::KeepPersonalList
//! [`HostOperation::ReadPersonalList`]: crate::host_authority::HostOperation::ReadPersonalList

use std::path::{Path, PathBuf};

use tilo_relay_session::workspaces::{PersonalList, PersonalWorkspace, WorkspaceHostAddress};
use tilo_user_identity::DeviceCert;

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
    list: PersonalList,
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

    /// Keep `list` as `device`'s person's, replacing what was kept.
    fn keep(&self, device: &DeviceCert, list: PersonalList, now: u64) -> Result<(), String> {
        list.check()?;
        let kept = Kept {
            by: hex::encode(device.device_public_key),
            at_ns: now,
            list,
        };
        let bytes = serde_json::to_vec(&kept).map_err(|error| error.to_string())?;
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let path = self.path(&device.user_public_key);
        let staged = path.with_extension("json.new");
        std::fs::write(&staged, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&staged, &path).map_err(|error| error.to_string())
    }

    /// The list `person`'s devices kept here last; empty when none did.
    fn of(&self, person: &[u8; 32]) -> PersonalList {
        let Ok(bytes) = std::fs::read(self.path(person)) else {
            return PersonalList::default();
        };
        serde_json::from_slice::<Kept>(&bytes).map_or_else(
            |error| {
                tracing::warn!(%error, "a kept workspace list does not read; answering none");
                PersonalList::default()
            },
            |kept| kept.list,
        )
    }
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
        let mut workspaces: Vec<PersonalWorkspace> = Vec::new();
        for kept in self.inner.personal.of(&device.user_public_key).workspaces {
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
        PersonalList { workspaces }
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
