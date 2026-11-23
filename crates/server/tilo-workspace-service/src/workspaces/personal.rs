//! The person's workspace list and devices, read from their own workspace
//! ([`ServiceOperation::KeepPersonalList`], [`ServiceOperation::ReadPersonalList`]).
//!
//! The person's devices hold their list: every workspace of theirs, wherever
//! it is hosted (a desktop of theirs, a paired device, a server), each with its
//! host's own signed address. The list is a resource of the person's own
//! workspace (`identity.workspaces`, a
//! [`WorkspaceDirectory`]), the same one their sessions read and write, so it
//! reaches their devices as any resource does rather than being kept in a
//! second place beside them. Any certified key of theirs (a script, an agent's
//! grant, the entry that serves the person's agents) reads it back: how a key
//! that runs on no device of theirs finds the workspaces it may enter, and at
//! which host.
//!
//! The devices the person's root certified are another resource of their own
//! workspace ([`super::person_workspace::devices`]), which every device of
//! theirs reads their devices from. This host writes there what it learns: a
//! key their root certified through it, when a session of a key was last here,
//! the agent a key's session said it runs, and a revocation it took in. A
//! device is never taken out; its root's revocation marks it.
//!
//! A browser keeps one key per open tab, since a key has one writer; a page
//! asking for the record names its browser's tab keys, which are kept together
//! in `so.tilo.shell`'s `tab-groups` of the person's own workspace, so a list
//! of their devices says which keys are one browser's.
//!
//! [`ServiceOperation::KeepPersonalList`]: crate::service_authority::ServiceOperation::KeepPersonalList
//! [`ServiceOperation::ReadPersonalList`]: crate::service_authority::ServiceOperation::ReadPersonalList

use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    KeepingDeviceAddress, MAX_BROWSER_DEVICES, MAX_PERSONAL_DEVICES, PersonalBrowser,
    PersonalList, PersonalWorkspace,
};
use tilo_user_identity::{
    DeviceCert, RevocationEntry, WorkspaceDirectory, WorkspaceMeta, verify_device_cert,
};

use super::Workspaces;

/// One person's devices by key: the name each reads by, and whether a
/// revocation here refuses it.
pub(super) type DeviceNames = BTreeMap<String, (String, bool)>;
use super::person_workspace;
use super::person_workspace::devices::{self, Devices};

/// The resource of the person's own workspace their list of workspaces is
/// kept in, as their sessions read and write it.
const WORKSPACES: (&str, &str) = ("so.tilo.identity", "identity.workspaces");

/// The resource of the person's own workspace each browser's tab keys are
/// kept in: a page naming its tabs says which keys are one browser's.
const TAB_GROUPS: (&str, &str) = ("so.tilo.shell", "tab-groups");

/// How often a session opening writes when a key was last here: a host serves
/// many sessions, and the record is read back rather than streamed.
const SEEN_WRITE_INTERVAL_MS: i64 = 60_000;

/// Now, ms since the epoch, as a record keeps it.
fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}

/// The counter a person's workspace list moves when a write of it lands: what
/// a device following its list waits on, rather than asking the host again and
/// again.
pub(super) struct PersonalPushes {
    changed: tokio::sync::watch::Sender<u64>,
}

impl Default for PersonalPushes {
    fn default() -> Self {
        Self {
            changed: tokio::sync::watch::channel(0).0,
        }
    }
}

impl Workspaces {
    /// The list changed: every device following its person's list reads it
    /// again.
    pub fn personal_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.personal_pushes.changed.subscribe()
    }

    /// A person's workspace list changed.
    pub(super) fn personal_changed(&self) {
        self.inner
            .personal_pushes
            .changed
            .send_modify(|changed| *changed = changed.wrapping_add(1));
    }

    /// The workspaces `person`'s own list names that this server does not govern, each by
    /// its id and the name the list gives it: what another server keeps for them, as a
    /// device of theirs last said. Nothing this server governs is among them, whoever is in
    /// it.
    pub async fn kept_elsewhere(&self, person: &[u8; 32]) -> Vec<(String, String)> {
        let here: std::collections::BTreeSet<String> = self
            .list()
            .await
            .into_iter()
            .map(|summary| summary.workspace)
            .collect();
        self.list_of(person)
            .await
            .list_workspaces()
            .into_iter()
            .filter(|kept| !here.contains(&kept.id.to_string()))
            .map(|kept| (kept.id.to_string(), kept.name))
            .collect()
    }

    /// Keep `list` as `device`'s person's list, already permitted to.
    pub(super) async fn keep_personal(
        &self,
        device: &DeviceCert,
        list: PersonalList,
    ) -> Result<usize, String> {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return Err("only a device of the person's own keeps their workspace list".to_owned());
        }
        list.check()?;
        let count = list.workspaces.len();
        let person = device.user_public_key;
        let workspace = person_workspace_id(&person);
        let device_key = self.serving_certificate().device_public_key;
        let workspaces = list.workspaces.clone();
        person_workspace::edit_document_on(self, &workspace, WORKSPACES, move |document| {
            let mut directory = WorkspaceDirectory::load(person, device_key, &document.save())
                .map_err(|error| error.to_string())?;
            let named: std::collections::BTreeSet<String> = workspaces
                .iter()
                .map(|entry| entry.workspace.clone())
                .collect();
            for kept in directory.list_workspaces() {
                if !named.contains(&kept.id.to_string()) {
                    directory
                        .remove_workspace(kept.id)
                        .map_err(|error| error.to_string())?;
                }
            }
            for (index, entry) in workspaces.iter().enumerate() {
                let Some(id) = tilo_user_identity::WorkspaceId::parse(&entry.workspace) else {
                    return Err(format!("{:?} names no workspace", entry.workspace));
                };
                directory
                    .add_workspace(id, &entry.name)
                    .map_err(|error| error.to_string())?;
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a list is shorter than f64 counts exactly"
                )]
                let order = index as f64;
                directory
                    .set_order(id, order)
                    .map_err(|error| error.to_string())?;
                if let Some(host) = &entry.host
                    && let Some(hint) = host.hint.as_deref()
                {
                    let bytes = hex::decode(hint).map_err(|error| error.to_string())?;
                    let Ok(address) = host.address() else {
                        continue;
                    };
                    let key = address.device_certificate.device_public_key;
                    directory
                        .set_host_hint(id, key, &bytes)
                        .map_err(|error| error.to_string())?;
                    directory
                        .assign_host(id, key)
                        .map_err(|error| error.to_string())?;
                }
            }
            let mut source =
                AutoCommit::load(&directory.save()).map_err(|error| error.to_string())?;
            tilo_automerge::ApplyIncoming::merge_incoming(document, &mut source)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .await?;
        // Every other device of theirs reads the list it kept.
        self.personal_changed();
        Ok(count)
    }

    /// Whether another device of `person`'s reads by `label`: one their record
    /// here holds, which a revocation this host keeps has not refused. What
    /// the host reads as it names a device pairing with them, so no two
    /// devices of one person read alike.
    #[must_use]
    pub fn device_label_taken(&self, person: &[u8; 32], label: &str) -> bool {
        let names = self.inner.device_names.lock_recover();
        names.get(person).is_some_and(|known| {
            known
                .values()
                .any(|(name, refused)| !*refused && name == label)
        })
    }

    /// `device`'s person's list as `device` reads it: the one in their own
    /// workspace, in their order, each workspace this host keeps for them
    /// named and placed as this host knows it, then the ones here the list
    /// does not name yet; limited to what an agent's grant reaches.
    #[expect(
        clippy::too_many_lines,
        reason = "one reading of the list: its own entries, then the ones only this host knows"
    )]
    pub(super) async fn personal(
        &self,
        device: &DeviceCert,
        here: &[tilo_server_protocol::workspaces::WorkspaceListing],
        only: &super::actions::host::Listed,
    ) -> PersonalList {
        let name = self.host_name();
        let person = device.user_public_key;
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
                enter_at: None,
                made_at_ms: listed.made_at_ms,
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
        let directory = self.list_of(&person).await;
        let mut workspaces: Vec<PersonalWorkspace> = Vec::new();
        for kept in ordered(&directory) {
            let workspace = kept.id.to_string();
            // A workspace this host governs is listed as its governance has the person, never as
            // a device of theirs last kept it: one they left, or were taken out of, is in no list
            // of theirs here, so their devices let it go.
            let governed_here = self.inner.memberships.serves(&workspace);
            if (governed_here && !local.iter().any(|one| one.workspace == workspace))
                || !reaches(&workspace)
                || !wanted(&PersonalWorkspace {
                    workspace: workspace.clone(),
                    name: kept.name.clone(),
                    host: None,
                    role: None,
                    labels: BTreeMap::default(),
                    unattended: false,
                    enter_at: None,
                    made_at_ms: None,
                })
                || workspaces.iter().any(|known| known.workspace == workspace)
            {
                continue;
            }
            let host = directory
                .host_hint(kept.id)
                .and_then(|hint| KeepingDeviceAddress::of_hint(&hint));
            if let Some(at) = local.iter().position(|one| one.workspace == workspace) {
                workspaces.push(local.remove(at));
            } else {
                // A workspace this host records as the person's own is kept here, whatever a
                // device's list said of where it is: a key's session asks it here, not at a
                // device that may be gone.
                let own = self.own().and_then(|own| own.owner_of(&workspace)) == Some(person);
                let host = if own {
                    Some(KeepingDeviceAddress {
                        name: name.clone(),
                        hint: None,
                    })
                } else {
                    host
                };
                workspaces.push(PersonalWorkspace {
                    workspace,
                    name: kept.name,
                    host,
                    role: None,
                    labels: BTreeMap::default(),
                    unattended: false,
                    enter_at: None,
                    made_at_ms: None,
                });
            }
        }
        workspaces.extend(local);
        PersonalList { workspaces }
    }

    /// Add `device`, one of its person's own devices that asked for their
    /// record, to it: a browser's root certifies the browser itself, in the
    /// tab, so its certificate reaches no host but on the browser's own
    /// session. An agent's grant and a stand-in are no device of theirs.
    pub(super) async fn note_own_device(&self, device: &DeviceCert) {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return;
        }
        self.keep_issued_device(device, "").await;
    }

    /// Mark the device `entry` names taken back in its person's record, where
    /// this host holds it and the record holds that device of theirs: every
    /// list of their devices shows it revoked.
    pub(super) async fn mark_revoked(&self, entry: &RevocationEntry) {
        let person = entry.user_public_key;
        let key = hex::encode(entry.revoked_device_public_key);
        let devices = self.devices_of(&person).await;
        let held = devices
            .listed(&person)
            .into_iter()
            .any(|listed| listed.key == key && !listed.device.revoked);
        if !held {
            return;
        }
        if let Err(error) = self
            .apply_devices(&person, Devices::REVOKE_DEVICE.with(key))
            .await
        {
            tracing::warn!(%error, "a revoked device is not marked in its person's record");
        }
        self.refresh_names(&person).await;
    }

    /// Keep `browser`, the tab keys a page of `device`'s person names as it
    /// asks for their record, as that browser's (see [`LIST_PERSONAL`]).
    /// Only a device of the person's own names them: an agent's grant and a
    /// stand-in are no browser of theirs.
    ///
    /// [`LIST_PERSONAL`]: tilo_server_protocol::workspaces::LIST_PERSONAL
    pub(super) async fn note_browser(&self, device: &DeviceCert, browser: PersonalBrowser) {
        if device.capabilities.agent.is_some() || device.capabilities.stand_in {
            return;
        }
        if !PersonalBrowser::is_id(&browser.id) {
            return;
        }
        // The newest, when a page names more than a browser keeps.
        let older = browser.certs.len().saturating_sub(MAX_BROWSER_DEVICES);
        let certs: Vec<DeviceCert> = browser
            .certs
            .into_iter()
            .skip(older)
            .filter(|cert| cert.capabilities.agent.is_none() && !cert.capabilities.stand_in)
            .collect();
        for cert in &certs {
            self.keep_issued_device(cert, "").await;
        }
        let person = device.user_public_key;
        // Only keys their root signed for them: the resource holds no other.
        let devices = self.devices_of(&person).await;
        let keys: Vec<String> = certs
            .iter()
            .map(|cert| hex::encode(cert.device_public_key))
            .filter(|key| {
                devices
                    .devices
                    .get(&tilo_resource::Id::from_raw(key.clone()))
                    .is_some()
            })
            .collect();
        let workspace = person_workspace_id(&person);
        let browser_id = browser.id;
        let chosen = keys.clone();
        let applied =
            person_workspace::edit_document_on(self, &workspace, TAB_GROUPS, move |document| {
                // A key is one browser's: a page naming it takes it from any other.
                let others: Vec<String> = document
                    .keys(ROOT)
                    .filter(|key| key.starts_with("tab:"))
                    .filter(|key| {
                        document
                            .get(ROOT, key.as_str())
                            .ok()
                            .flatten()
                            .and_then(|(value, _)| value.into_string().ok())
                            .is_some_and(|id| id != browser_id)
                    })
                    .filter(|key| chosen.contains(&key.trim_start_matches("tab:").to_owned()))
                    .collect();
                for key in others {
                    document
                        .delete(ROOT, key.as_str())
                        .map_err(|error| error.to_string())?;
                }
                let named: std::collections::BTreeSet<String> =
                    chosen.iter().map(|key| format!("tab:{key}")).collect();
                for key in document
                    .keys(ROOT)
                    .filter(|key| key.starts_with("tab:"))
                    .filter(|key| {
                        document
                            .get(ROOT, key.as_str())
                            .ok()
                            .flatten()
                            .and_then(|(value, _)| value.into_string().ok())
                            .as_deref()
                            == Some(browser_id.as_str())
                            && !named.contains(key)
                    })
                    .collect::<Vec<_>>()
                {
                    document
                        .delete(ROOT, key.as_str())
                        .map_err(|error| error.to_string())?;
                }
                for key in &chosen {
                    document
                        .put(ROOT, format!("tab:{key}"), browser_id.clone())
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = applied {
            tracing::debug!(%error, "a page's browser is not in the person's record");
        }
    }

    /// Add `cert`, a key its person's root just certified through this host
    /// (a pairing the root answered here, an agent's grant), to their
    /// record, named `label`: the name the host gave it, so the person's
    /// lists show two devices set up alike apart.
    ///
    /// The record itself is a resource of the person's own workspace, written
    /// on this host's runtime. The cache the host answers a pairing from is
    /// brought up to date here first, so a device issued now is named apart
    /// from one issued a moment later.
    pub async fn keep_issued_device(&self, cert: &DeviceCert, label: &str) {
        if verify_device_cert(cert).is_err() {
            return;
        }
        let person = cert.user_public_key;
        let Ok(certificate) = postcard::to_allocvec(cert) else {
            return;
        };
        let key = hex::encode(cert.device_public_key);
        let name = if label.is_empty()
            || cert.capabilities.agent.is_some()
            || cert.capabilities.stand_in
        {
            None
        } else {
            self.refresh_names(&person).await;
            Some(self.free_name(&person, &key, label))
        };
        self.inner
            .device_names
            .lock_recover()
            .entry(person)
            .or_default()
            .insert(
                key.clone(),
                (
                    name.clone().unwrap_or_else(|| cert.device_label.clone()),
                    false,
                ),
            );
        let action = Devices::KEEP_DEVICE.with((key.clone(), hex::encode(certificate)));
        if let Err(error) = self.apply_devices(&person, action).await {
            tracing::warn!(%error, "a key the person's root certified here is not in their record");
            return;
        }
        if let Some(name) = name
            && let Err(error) = self
                .apply_devices(&person, Devices::NAME_DEVICE.with((key, name)))
                .await
        {
            tracing::warn!(%error, "a device the host named is not named in the person's record");
        }
    }

    /// A session of `cert`'s device opened here: their record says when the
    /// key was here last.
    pub(crate) async fn session_opened(&self, cert: &DeviceCert) {
        self.session_opened_at(cert, now_ms()).await;
    }

    /// The session of `cert`'s device that was open here has ended: their
    /// record says when it was here last.
    pub(crate) async fn session_ended(&self, cert: &DeviceCert) {
        self.session_ended_at(cert, now_ms()).await;
    }

    /// [`Self::session_opened`] at `at_ms`, for tests that say when.
    pub(crate) async fn session_opened_at(&self, cert: &DeviceCert, at_ms: i64) {
        self.note_seen(cert, at_ms, true).await;
    }

    /// [`Self::session_ended`] at `at_ms`, for tests that say when.
    pub(crate) async fn session_ended_at(&self, cert: &DeviceCert, at_ms: i64) {
        self.note_seen(cert, at_ms, false).await;
    }

    /// Say that `cert`, a key of its person's, runs the agent `name`: their
    /// record says so of the key, and the host's own person's MCP server lists
    /// the run.
    pub(crate) fn note_agent(
        &self,
        cert: &DeviceCert,
        name: &str,
        stop: &Arc<tokio::sync::Notify>,
    ) {
        if cert.user_public_key == self.inner.host.cert.user_public_key
            && cert.device_public_key != self.inner.host.cert.device_public_key
        {
            self.inner.mcp_server.note(cert, name, stop);
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (workspaces, cert, name) = (self.clone(), cert.clone(), name.to_owned());
            runtime.spawn(async move { workspaces.keep_agent(&cert, &name).await });
        }
    }

    /// Keep `name` as the agent `cert`'s key runs in its person's record,
    /// where the record holds the key and says otherwise.
    async fn keep_agent(&self, cert: &DeviceCert, name: &str) {
        let person = cert.user_public_key;
        let key = hex::encode(cert.device_public_key);
        let devices = self.devices_of(&person).await;
        let Some(known) = devices
            .devices
            .get(&tilo_resource::Id::from_raw(key.clone()))
        else {
            return;
        };
        if known.agent.as_deref() == Some(name.trim()) {
            return;
        }
        let action = Devices::KEEP_AGENT.with((key, name.to_owned()));
        if let Err(error) = self.apply_devices(&person, action).await {
            tracing::debug!(%error, "the agent a key runs is not in its person's record");
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

impl Workspaces {
    /// The person's workspace list as it is in their own workspace: empty
    /// while this host does not hold that workspace.
    pub(super) async fn list_of(&self, person: &[u8; 32]) -> WorkspaceDirectory {
        let workspace = person_workspace_id(person);
        let device = self.serving_certificate().device_public_key;
        match person_workspace::document_on(self, &workspace, WORKSPACES).await {
            Ok(Some(bytes)) => {
                WorkspaceDirectory::load(*person, device, &bytes).unwrap_or_else(|error| {
                    tracing::warn!(%error, "the person's workspace list does not read");
                    WorkspaceDirectory::new(*person, device)
                })
            }
            _ => WorkspaceDirectory::new(*person, device),
        }
    }

    /// The devices resource of the person's own workspace: empty while this
    /// host does not hold it.
    pub(crate) async fn devices_of(&self, person: &[u8; 32]) -> Devices {
        let workspace = person_workspace_id(person);
        person_workspace::read_on::<Devices>(self, &workspace, devices::DEVICES)
            .await
            .unwrap_or_default()
    }

    /// The devices present here now: each holds a link a workspace this host
    /// serves admitted and has not ended, as presence counts a device here.
    pub(crate) fn present_devices(&self) -> std::collections::BTreeSet<[u8; 32]> {
        let admitted = self.inner.link_remotes.lock_recover().clone();
        let mut present = std::collections::BTreeSet::new();
        for (workspace_id, remotes) in admitted {
            let Some(link) = self.inner.direct.link(workspace_id) else {
                continue;
            };
            present.extend(
                remotes
                    .into_iter()
                    .filter(|remote| link.ended_link(*remote).is_none()),
            );
        }
        present
    }

    /// Each tab key's browser, by key, as the person's own workspace holds it.
    pub(crate) async fn tab_groups(&self, person: &[u8; 32]) -> BTreeMap<String, String> {
        let workspace = person_workspace_id(person);
        let Ok(Some(bytes)) = person_workspace::document_on(self, &workspace, TAB_GROUPS).await
        else {
            return BTreeMap::new();
        };
        let Ok(document) = AutoCommit::load(&bytes) else {
            return BTreeMap::new();
        };
        document
            .keys(ROOT)
            .filter_map(|key| {
                let key = key.strip_prefix("tab:")?.to_owned();
                let browser = document
                    .get(ROOT, format!("tab:{key}").as_str())
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok())?;
                Some((key, browser))
            })
            .collect()
    }

    /// Apply `action` to the person's devices resource.
    pub(crate) async fn apply_devices(
        &self,
        person: &[u8; 32],
        action: tilo_resource::Invoke<Devices>,
    ) -> Result<(), String> {
        let workspace = person_workspace_id(person);
        person_workspace::apply_on(self, &workspace, devices::DEVICES, action).await
    }

    /// Keep `value` as the fact `name` of the device `key` of `person`'s.
    async fn keep_fact(
        &self,
        person: &[u8; 32],
        key: &str,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        let devices = self.devices_of(person).await;
        let certificate = devices
            .devices
            .get(&tilo_resource::Id::from_raw(key.to_owned()))
            .map(|kept| kept.certificate.clone())
            .ok_or_else(|| "that key is not a device of this person's".to_owned())?;
        self.apply_devices(
            person,
            Devices::KEEP_FACT.with((
                key.to_owned(),
                certificate,
                name.to_owned(),
                value.to_owned(),
            )),
        )
        .await
    }

    /// Say that a session of `cert`'s device was here at `at_ms`, opening or
    /// ending. Only a device the person's record holds says it; an opening is
    /// written at most once a minute, and an ending always is.
    async fn note_seen(&self, cert: &DeviceCert, at_ms: i64, opening: bool) {
        let person = cert.user_public_key;
        let key = hex::encode(cert.device_public_key);
        let devices = self.devices_of(&person).await;
        let Some(known) = devices
            .devices
            .get(&tilo_resource::Id::from_raw(key.clone()))
        else {
            return;
        };
        if opening
            && known
                .seen_ms()
                .is_some_and(|seen| at_ms.saturating_sub(seen) < SEEN_WRITE_INTERVAL_MS)
        {
            return;
        }
        if known.seen_ms() == Some(at_ms) {
            return;
        }
        if let Err(error) = self
            .keep_fact(
                &person,
                &key,
                devices::SEEN_FACT,
                &hex::encode(at_ms.to_le_bytes()),
            )
            .await
        {
            tracing::debug!(%error, "a key's session is not in the person's record");
        }
    }

    /// A free name for the device `key`, one of `person`'s devices enrolled
    /// under `base`: `base` itself when no other of their devices reads by it,
    /// else the next numbered one (`Claude Code on laptop 2`). A device that
    /// was taken back reads by no name any more, so its name is free.
    fn free_name(&self, person: &[u8; 32], key: &str, base: &str) -> String {
        let taken = |candidate: &str| {
            let names = self.inner.device_names.lock_recover();
            names.get(person).is_some_and(|known| {
                known
                    .iter()
                    .any(|(other, (name, refused))| other != key && !*refused && name == candidate)
            })
        };
        if !taken(base) {
            return base.to_owned();
        }
        (2..=MAX_PERSONAL_DEVICES)
            .map(|number| format!("{base} {number}"))
            .find(|candidate| !taken(candidate))
            .unwrap_or_else(|| base.to_owned())
    }

    /// Bring the names the host answers a pairing from up to date with
    /// `person`'s devices as their own workspace lists them, each marked
    /// where it was taken back.
    async fn refresh_names(&self, person: &[u8; 32]) {
        let revocations = self.revocations();
        let known: DeviceNames = self
            .devices_of(person)
            .await
            .listed(person)
            .into_iter()
            .map(|listed| {
                let refused = listed.device.revoked || revocations.refuses(&listed.cert);
                (listed.key, (listed.device.called(&listed.cert), refused))
            })
            .collect();
        self.inner
            .device_names
            .lock_recover()
            .insert(*person, known);
    }
}

/// The workspaces of a directory, in the person's order.
pub(super) fn ordered(directory: &WorkspaceDirectory) -> Vec<WorkspaceMeta> {
    let mut out = directory.list_workspaces();
    out.sort_by(|left, right| {
        let key = |entry: &WorkspaceMeta| entry.order.unwrap_or(f64::INFINITY);
        key(left)
            .total_cmp(&key(right))
            .then_with(|| left.id.cmp(&right.id))
    });
    out
}
