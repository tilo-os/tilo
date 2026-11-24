//! The person's devices and the list of their workspaces, in their own workspace.
//!
//! The person's devices hold their list: every workspace of theirs, wherever
//! it is hosted (a desktop of theirs, a paired device, a server), each with its
//! host's own signed address. The list is a resource of the person's own
//! workspace ([`IDENTITY_LIST`], a [`WorkspaceList`]), which their devices
//! change through its actions, so it reaches their devices as any resource
//! does, and every reader reads it through the record API. Any certified key of
//! theirs (a script, an agent's grant, the entry that serves the person's
//! agents) reads it back: how a key that runs on no device of theirs finds the
//! workspaces it may enter, and at which host.
//!
//! The devices the person's root certified are another resource of their own
//! workspace ([`super::person_workspace::devices`]), which every device of
//! theirs reads their devices from. This host writes there what it learns: a
//! key their root certified through it, when a session of a key was last here,
//! the agent a key's session said it runs, and a revocation it took in. A
//! device is never taken out; its root's revocation marks it.

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{IDENTITY_LIST, MAX_PERSONAL_DEVICES};
use tilo_user_identity::{DeviceCert, RevocationEntry, WorkspaceList, verify_device_cert};

use super::Workspaces;
use super::person_workspace;
use super::person_workspace::devices::{self, Devices};

/// One person's devices by key: the name each reads by, and whether a
/// revocation here refuses it.
pub(super) type DeviceNames = BTreeMap<String, (String, bool)>;

/// How often a session opening writes when a key was last here: a host serves
/// many sessions, and the record is read back rather than streamed.
const SEEN_WRITE_INTERVAL_MS: i64 = 60_000;

/// Now, ms since the epoch, as a record keeps it.
fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}

impl Workspaces {
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
            .listed()
            .filter(|(workspace, _)| !here.contains(&workspace.to_string()))
            .map(|(workspace, kept)| (workspace.to_string(), kept.name.clone()))
            .collect()
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

    /// Add `device`, a key of its person's that asked for their record, to
    /// it: a browser's root certifies the browser itself, in the tab, and the
    /// person's Tilo on the web certifies a script's key, so neither
    /// certificate reaches a host but with the key itself. A stand-in is no key
    /// of theirs.
    pub(super) async fn note_own_device(&self, device: &DeviceCert) {
        if device.capabilities.stand_in {
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
    /// keeping the key there first where the record does not hold it yet.
    async fn keep_agent(&self, cert: &DeviceCert, name: &str) {
        if cert.capabilities.stand_in {
            return;
        }
        let person = cert.user_public_key;
        let key = hex::encode(cert.device_public_key);
        let devices = self.devices_of(&person).await;
        let known = devices
            .devices
            .get(&tilo_resource::Id::from_raw(key.clone()));
        if known.is_none() {
            self.keep_issued_device(cert, "").await;
        }
        if known.and_then(|known| known.agent.as_deref()) == Some(name.trim()) {
            return;
        }
        let action = Devices::KEEP_AGENT.with((key, name.to_owned()));
        if let Err(error) = self.apply_devices(&person, action).await {
            tracing::debug!(%error, "the agent a key runs is not in its person's record");
        }
    }
}

impl Workspaces {
    /// The person's workspace list as it is in their own workspace: empty
    /// while this host does not hold that workspace.
    async fn list_of(&self, person: &[u8; 32]) -> WorkspaceList {
        let workspace = person_workspace_id(person);
        person_workspace::read_on::<WorkspaceList>(self, &workspace, IDENTITY_LIST)
            .await
            .unwrap_or_default()
    }

    /// The devices resource of the person's own workspace: empty while this
    /// host does not hold it.
    pub(crate) async fn devices_of(&self, person: &[u8; 32]) -> Devices {
        let workspace = person_workspace_id(person);
        person_workspace::read_on::<Devices>(self, &workspace, devices::DEVICES)
            .await
            .unwrap_or_default()
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

/// Who is present on which device, as one source of presence on this device hears it: what who
/// is here ([`tilo_actions::remote::Here`]) carries beyond a workspace's roster, and nothing else
/// reads.
///
/// A source tells [`presence_changed`] whenever what it attaches may have changed.
pub trait PresentDevices: Send + Sync {
    /// Add the devices present in `workspace` now to who is `here`: a person present on a device
    /// and missing from `here` is added.
    fn attach(&self, workspace: &str, here: &mut Vec<tilo_actions::remote::Here>);
}

/// The held reads of who is here, waiting on the next [`presence_changed`]: one list every read
/// shares, so a held read costs an entry here and nothing runs until presence moves.
static PRESENCE_WAITING: std::sync::Mutex<Vec<tilo_actions::remote::ResourceWake>> =
    std::sync::Mutex::new(Vec::new());

/// Who is present on which device may have changed: a link into a workspace opened or ended, or
/// a device of the person's came onto or left a live session with this one. Wakes every held
/// read of who is here, once.
pub fn presence_changed() {
    let waiting = std::mem::take(&mut *PRESENCE_WAITING.lock_recover());
    for wake in waiting {
        wake.wake();
    }
}

/// Wake `wake` on the next [`presence_changed`].
pub fn on_presence_change(wake: &tilo_actions::remote::ResourceWake) {
    let mut waiting = PRESENCE_WAITING.lock_recover();
    waiting.retain(tilo_actions::remote::ResourceWake::waiting);
    waiting.push(wake.clone());
}

/// Mark `person` (hex) present on `device` (hex) in `here`, adding them when they are missing.
pub fn present_on(here: &mut Vec<tilo_actions::remote::Here>, person: &str, device: String) {
    match here
        .iter_mut()
        .find(|entry| entry.kind == "person" && entry.who == person)
    {
        Some(entry) if !entry.devices.contains(&device) => entry.devices.push(device),
        Some(_) => {}
        None => here.push(tilo_actions::remote::Here {
            who: person.to_owned(),
            name: person.to_owned(),
            kind: "person".into(),
            works_in: None,
            devices: vec![device],
        }),
    }
}

/// The prefix of an agent session's entry in who is here: what tells a live session of an agent
/// from a person and from a member, whose ids are their own keys.
const SESSION_WHO: &str = "agent-session:";

/// The agent sessions this device serves, by the run each connection was admitted in: each is
/// present on this device from the moment its connection opens until its session ends.
#[derive(Default)]
pub(super) struct AgentSessions(std::sync::Mutex<BTreeMap<String, AgentSession>>);

/// One agent session this device serves: one MCP connection, while it lasts.
struct AgentSession {
    /// The workspace it works in.
    workspace: String,
    /// What the workspace calls it: the client's own name once it said one.
    name: String,
    /// The person stopped this session: it stays ended, however long its connection is held.
    stopped: bool,
}

impl Workspaces {
    /// A connection in `workspace` opened, admitted in the run `execution`: its agent is present
    /// on this device from now on, called `name` until its client says what it is.
    pub fn agent_opened(&self, execution: &str, workspace: &str, name: &str) {
        self.inner.agent_sessions.0.lock_recover().insert(
            execution.to_owned(),
            AgentSession {
                workspace: workspace.to_owned(),
                name: name.to_owned(),
                stopped: false,
            },
        );
        presence_changed();
    }

    /// The client of `execution`'s connection named itself: who is here calls its session
    /// `client` from now on.
    pub fn agent_named(&self, execution: &str, client: &str) {
        {
            let mut sessions = self.inner.agent_sessions.0.lock_recover();
            let Some(session) = sessions
                .get_mut(execution)
                .filter(|session| session.name != client)
            else {
                return;
            };
            session.name = client.to_owned();
        }
        presence_changed();
    }

    /// The connection of `execution` ended: its agent leaves who is here.
    pub fn agent_closed(&self, execution: &str) {
        let removed = self
            .inner
            .agent_sessions
            .0
            .lock_recover()
            .remove(execution)
            .is_some();
        if removed {
            presence_changed();
        }
    }

    /// The person stopped the agent session called `name` in `workspace`: it leaves who is here
    /// and stays ended. Whether a session that name was here.
    pub fn stop_agent(&self, workspace: &str, name: &str) -> bool {
        {
            let mut sessions = self.inner.agent_sessions.0.lock_recover();
            let Some(session) = sessions.values_mut().find(|session| {
                session.workspace == workspace && session.name == name && !session.stopped
            }) else {
                return false;
            };
            session.stopped = true;
        }
        presence_changed();
        true
    }

    /// Whether the person stopped the session of the run `execution`, so its calls end nothing.
    pub fn agent_stopped(&self, execution: &str) -> bool {
        self.inner
            .agent_sessions
            .0
            .lock_recover()
            .get(execution)
            .is_some_and(|session| session.stopped)
    }
}

impl PresentDevices for Workspaces {
    /// The agent sessions this device serves in `workspace`, each its own entry present on this
    /// device, so a run after another is a newcomer rather than the run before it moved on; and
    /// the devices holding a link of this host's into `workspace` that has not ended, each under
    /// the person its certificate names there.
    fn attach(&self, workspace: &str, here: &mut Vec<tilo_actions::remote::Here>) {
        let device = hex::encode(self.device_cert().device_public_key);
        for (execution, session) in &*self.inner.agent_sessions.0.lock_recover() {
            if session.workspace != workspace || session.stopped {
                continue;
            }
            here.push(tilo_actions::remote::Here {
                who: format!("{SESSION_WHO}{execution}"),
                name: session.name.clone(),
                kind: "agent".to_owned(),
                works_in: None,
                devices: vec![device.clone()],
            });
        }
        let Some(workspace_id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
            return;
        };
        let remotes = self
            .inner
            .link_remotes
            .lock_recover()
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        let (Some(link), Some(held)) = (
            self.inner.direct.link(workspace_id),
            self.inner.readings.held(workspace),
        ) else {
            return;
        };
        let certificates = held.replica.lock().certificates();
        for remote in remotes {
            if link.ended_link(remote).is_some() {
                continue;
            }
            let Some(person) = certificates
                .iter()
                .find(|certificate| certificate.device_public_key == remote)
                .map(|certificate| hex::encode(certificate.user_public_key))
            else {
                continue;
            };
            present_on(here, &person, hex::encode(remote));
        }
    }
}
