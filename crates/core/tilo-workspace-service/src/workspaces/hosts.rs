//! The holders of a workspace held here: this device and each one its owners
//! asked to hold the workspace as well, every one alike ([`super::holding`]):
//! another device of the owner's person, or another person's server whose
//! operator takes that work.
//!
//! Each holder signs its own address, and the address says how it holds the
//! workspace ([`CopyRole`]): as a key holder, which reads it and takes its work,
//! or as a store, which keeps and forwards its ciphertext alone. The
//! workspace's governance names its holders
//! ([`tilo_peer_state::WORKSPACE_KEEPING_DEVICES_KEY`]), so every member learns
//! of each, and how it holds the workspace, as it learns any change to the
//! workspace, and the workspace's join answers carry them all: a member
//! reaches the workspace wherever it is held. Being named there is what admits
//! a holder everywhere the workspace is held: its device holds the workspace,
//! and its person may reach the holders that do
//! ([`tilo_peer_state::GovernedAccess`]). Being named a key holder is what gives
//! it the workspace's keys ([`tilo_peer_state::PeerGovernance::key_holders`]),
//! never a part among its members: a key holder of another person reads the
//! workspace without changing who is in it. A device of the owner's own
//! person is also admitted as one of the owner's devices before it is handed
//! the workspace, whatever role it holds it in.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh_base::EndpointAddr;
use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::CopyRole;
use tilo_peer_state::actions::RemoteDevice;
use tilo_peer_state::join::{Role, TransportHint};
use tilo_server_protocol::workspaces::WorkspaceHolder;
use tilo_server_protocol::workspaces::{
    CATCH_UP_ABSENT, CATCH_UP_STANDING, CATCH_UP_WORKSPACE, CaughtUpWorkspace,
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE, HostedWorkspace, StandingPage,
    UNHOST_WORKSPACE,
};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, GovernedWorkspace, Workspaces};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::WorkspaceId;

/// How many addresses of the content a handing device keeps none of one ask
/// carries: about seventy bytes each in its frame, so an ask stays well
/// within [`tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES`]. A list no
/// longer than this goes in the catch-up itself, as a build before lists in
/// parts takes it; a longer one goes ahead of it in parts
/// ([`CATCH_UP_ABSENT`]).
const ABSENT_PER_ASK: usize = 4096;

/// How many answers a catch-up takes at most: one follows each time what the device
/// answering it lacks narrowed or its store moved, and a device that keeps writing while it
/// answers moves its store without ever catching up, so the asking is bounded.
const CATCH_UP_ANSWERS: u32 = 100;

/// How long a server taking a workspace in waits for its content to move before it answers what
/// it still lacks ([`Workspaces::content_settled`]).
const CONTENT_SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How many cursors of where a store stands one answer carries: under a
/// hundred and fifty bytes each in its frame, so an answer stays well within
/// [`tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES`] however many
/// records and authors the workspace has ([`CATCH_UP_STANDING`]).
const CURSORS_PER_ANSWER: usize = 512;

/// The most addresses the list one handing device hands here holds: a
/// workspace whose content its handing device keeps none of past this is
/// refused, rather than held in memory without bound.
const MAX_ABSENT: usize = 1 << 21;

/// The longest address such a list holds.
const MAX_ABSENT_ADDRESS: usize = 256;

/// How long a list handed in parts is kept after it was last handed to or
/// counted with: a catch-up that ended without letting it go leaves it no
/// longer than this.
const ABSENT_KEPT_FOR: Duration = Duration::from_secs(600);
/// How long a host's operation waits for another host to finish it.
const HOST_ANSWERED: Duration = Duration::from_secs(120);

/// The list of content one device handing a workspace over keeps none of
/// itself, with when it was last handed to or counted with.
type AbsentList = (Arc<BTreeSet<String>>, Instant);

/// The content each device handing a workspace over keeps none of itself,
/// as it handed the list here in parts ([`CATCH_UP_ABSENT`]), by device and
/// workspace.
#[derive(Default)]
pub(super) struct HandedAbsent(Mutex<HashMap<([u8; 32], String), AbsentList>>);

impl HandedAbsent {
    /// Add `part` to the list kept under `key`, afresh when `first`; an
    /// empty first part lets the list go. Lists not handed to or counted
    /// with for [`ABSENT_KEPT_FOR`] go first. Answers how many addresses the
    /// list holds.
    fn keep(
        &self,
        key: &([u8; 32], String),
        part: BTreeSet<String>,
        first: bool,
    ) -> Result<usize, String> {
        let now = tilo_clock::now();
        let mut lists = self.0.lock_recover();
        lists.retain(|_, (_, touched)| now.duration_since(*touched) < ABSENT_KEPT_FOR);
        if first {
            lists.remove(key);
        }
        let kept = if first && part.is_empty() {
            Some(0)
        } else if first || lists.contains_key(key) {
            let (list, touched) = lists
                .entry(key.clone())
                .or_insert_with(|| (Arc::default(), now));
            *touched = now;
            let list = Arc::make_mut(list);
            list.extend(part);
            Some(list.len())
        } else {
            None
        };
        if kept.is_some_and(|kept| kept > MAX_ABSENT) {
            lists.remove(key);
        }
        drop(lists);
        match kept {
            None => Err(
                "the first part of the list of content this device keeps none of is not here: \
                 hand the list again"
                    .into(),
            ),
            Some(kept) if kept > MAX_ABSENT => Err(format!(
                "the content the asking device keeps none of is more than the {MAX_ABSENT} \
                 addresses this server takes of it"
            )),
            Some(kept) => Ok(kept),
        }
    }

    /// The list kept under `key`, counted with now, when one is.
    fn counted_with(&self, key: &([u8; 32], String)) -> Option<Arc<BTreeSet<String>>> {
        let now = tilo_clock::now();
        let mut lists = self.0.lock_recover();
        lists.retain(|_, (_, touched)| now.duration_since(*touched) < ABSENT_KEPT_FOR);
        let list = lists.get_mut(key).map(|(list, touched)| {
            *touched = now;
            Arc::clone(list)
        });
        drop(lists);
        list
    }
}

/// The device a signed host address names.
const fn host_device(hint: &TransportHint) -> [u8; 32] {
    hint.host.device_public_key
}

impl Workspaces {
    /// Every holder of `workspace`, each its own signed address, by device
    /// key: the holders are equal, so nothing here comes first. A host the
    /// workspace's governance does not name yet (one founded here before it
    /// named itself) is listed at its own signed address all the same.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or this host's address cannot be
    /// signed.
    pub async fn hosts(&self, workspace: &str) -> Result<Vec<TransportHint>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        let own = self.transport_hint(&governed)?;
        let mut hosts = governed.peer().governance().keeping_devices();
        if !hosts
            .iter()
            .any(|hint| hint.host.device_public_key == own.host.device_public_key)
        {
            hosts.push(own);
        }
        drop(governed);
        hosts.sort_by_key(|hint| hint.host.device_public_key);
        Ok(hosts)
    }

    /// The devices that hold `workspace`'s keys now, as every device reads it
    /// from its governance ([`tilo_peer_state::PeerGovernance::key_holders`]):
    /// who can read it.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, or its governance does not read.
    pub async fn key_holders(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeSet<[u8; 32]>, String> {
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let governed = governed.lock().await;
        governed.peer().governance().key_holders(tilo_clock::wall())
    }

    /// Whether `governed`'s governance names this device a store of the
    /// workspace's ciphertext: it then asks for none of its keys and compacts
    /// none of what it cannot read.
    #[cfg_attr(
        not(feature = "device-keys"),
        allow(
            dead_code,
            reason = "sealed histories are followed only where they are read"
        )
    )]
    pub(super) fn holds_as_store(&self, governed: &GovernedWorkspace) -> bool {
        governed
            .peer()
            .governance()
            .holder_role(&self.inner.host.cert.device_public_key)
            == Some(CopyRole::Store)
    }

    /// Whether `workspace`'s governance names this device a store of its
    /// ciphertext, which reads none of it; `false` for one not governed here.
    pub async fn stores_only(&self, workspace: &str) -> bool {
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        self.holds_as_store(&governed)
    }

    /// Whether `workspace`'s governance names the device `device` among its
    /// hosts.
    pub(crate) async fn names_server(&self, workspace: &str, device: [u8; 32]) -> bool {
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        governed
            .peer()
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| host_device(hint) == device)
    }

    /// Name this host's own signed address among the workspace's hosts, so
    /// every member reads where the workspace is served from the replica it
    /// already holds. Naming it as it is named changes nothing. A host
    /// keeping another person's workspace, as neither its owner nor one of
    /// its administrators, names nothing: its holders are
    /// that person's to name, and the address it signed is the one they
    /// named. Nor does a holder its owners have not named yet, other than the
    /// device the workspace was founded on: whoever hands it the workspace
    /// names it, in the role they chose for it.
    pub(super) async fn name_own_host(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let own = host.cert.device_public_key;
        let (others, unnamed) = {
            let peer = governed.peer();
            (
                !peer.governs_history(&host.cert.user_public_key),
                peer.governance().holder_role(&own).is_none()
                    && peer.governance().founder() != Some(own),
            )
        };
        if others || unnamed {
            return Ok(());
        }
        let hint = self
            .transport_hint(governed)
            .map_err(|error| error.to_string())?;
        let now = tilo_clock::wall();
        let named = governed.change_held(
            &host.key,
            now,
            |_, candidate| {
                let named = candidate
                    .governance_mut()
                    .author_keeping_device(&hint, &host.key, now)?;
                Ok((named, named))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if named {
            self.settle(governed).await?;
        }
        Ok(())
    }

    /// Name this host's own signed address in every workspace it governs,
    /// after where it answers changed: what their governance names is where
    /// members and invitees dial this host.
    pub(super) async fn name_own_hosts(&self) {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            if let Err(error) = self.name_own_host(&mut workspace).await {
                tracing::error!(
                    workspace = %workspace.workspace(),
                    %error,
                    "the host's address is not named in the workspace's governance"
                );
            }
        }
    }

    /// Follow the relays this host answers through: each change of home relay
    /// is named in the workspaces it governs, so an invitation always carries
    /// an address a stranger can dial. A host that has lost its relays names
    /// nothing: the address it named stays what a workspace says, and a member
    /// reaches it again through the server once it comes back.
    pub(super) fn follow_own_address(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let workspaces = Arc::downgrade(&self.inner);
        let following =
            tilo_endpoint_reachability::follow(&self.inner.host.endpoint, &runtime, move |_| {
                let workspaces = workspaces.clone();
                async move {
                    let Some(inner) = workspaces.upgrade() else {
                        return;
                    };
                    Self { inner }.name_own_hosts().await;
                }
            });
        let _ = self.inner.own_address.set(following);
    }

    /// Change which hosts `workspace`'s governance names with `change`, as its
    /// governor.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed and settled"
    )]
    pub(super) async fn name_host(
        &self,
        workspace: &str,
        change: impl FnOnce(
            tilo_peer_state::PeerGovernanceMut<'_>,
            &ed25519_dalek::SigningKey,
            std::time::SystemTime,
        ) -> Result<bool, String>,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let now = tilo_clock::wall();
        let changed = governed.change_held(
            &host.key,
            now,
            |_, candidate| {
                let changed = change(candidate.governance_mut(), &host.key, now)?;
                Ok((changed, changed))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if changed {
            self.settle(&mut governed).await?;
        }
        Ok(())
    }

    /// Have the server `device`, at `addr`, hold `workspace` as well, as
    /// `role`, as its owners chose ([`Workspaces::chooses_servers`]): hand the
    /// workspace over with [`HOST_WORKSPACE`] and name the address it answers,
    /// which says that role, among the workspace's holders. A device of this
    /// device's own person is admitted to the workspace as one of the owner's
    /// devices first; another person's server takes the workspace only
    /// when its operator's work setting does. Asked again, the server is
    /// handed the workspace afresh, as `role`. Answers that address once the
    /// server says it keeps every change this device's store keeps of the
    /// workspace ([`Self::caught_up_with_this`]): only then may this device
    /// go without the workspace losing any of them. Before this answers,
    /// every other device the workspace names to keep it that can be reached
    /// takes in the change naming the new one, so whichever device goes next,
    /// the rest still reach one another.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, `device` is this device, the server
    /// cannot be reached or is not that device, or it refused or answered an
    /// address that is not its own for the workspace in that role. Or it did
    /// not say it keeps every change this device keeps: this device keeps
    /// the workspace then, and a server named for this hand-over alone is
    /// named no more.
    pub async fn attach_server(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: EndpointAddr,
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let host = &self.inner.host;
        if device == host.cert.device_public_key {
            return Err("this device holds the workspace already".into());
        }
        let reached = self.reach_host(device, addr).await?;
        let member = reached.certificate.clone();
        let named_before = self.names_server(workspace, device).await;
        let (workspace_id, arguments) = self.hand_over(workspace, &member, role).await?;
        let answer = reached.ask(HOST_WORKSPACE, arguments).await?;
        let hosted: HostedWorkspace =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let hint: TransportHint = super::decode_hex(&hosted.host_hint)?;
        let address =
            WorkspaceDeviceHintV1::from_transport_hint(&hint).map_err(|error| error.to_string())?;
        if hint.host != member
            || address.holds_keys != (role == CopyRole::KeyHolder)
            || address.workspace_id != workspace_id
        {
            return Err(
                "the device answered an address that is not its own for the workspace in the \
                 role it was asked to hold it in"
                    .into(),
            );
        }
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device(&hint, key, now)
        })
        .await?;
        let _ = self.inner.changes.send(workspace.to_owned());
        // Named, the server may link to this device's: it takes in what this
        // device keeps over that link, which only a device the workspace
        // names may.
        if let Err(error) = self
            .caught_up_with_this(workspace, workspace_id, &reached)
            .await
        {
            let unnamed = if named_before {
                Ok(false)
            } else {
                self.name_host(workspace, |governance, key, now| {
                    governance.author_keeping_device_removed(&device, key, now)
                })
                .await
                .map(|()| true)
            };
            let _ = self.inner.changes.send(workspace.to_owned());
            tracing::warn!(%workspace, server = %hex::encode(device), %error, "a server handed the workspace did not say it keeps all of it; this device keeps it");
            return Err(match unnamed {
                Ok(_) => format!("this device keeps the workspace: {error}"),
                Err(unnamed) => format!(
                    "this device keeps the workspace: {error}; the server is still named \
                     among those keeping it ({unnamed})"
                ),
            });
        }
        // A device that keeps the workspace but never hears of the one just named cannot link
        // to it once this device has gone, and nothing else would tell it: each takes in the
        // change before this answers.
        self.tell_the_others(workspace, workspace_id, device).await;
        tracing::info!(%workspace, server = %hex::encode(device), person = %hex::encode(member.user_public_key), ?role, "another device holds the workspace");
        Ok(hint)
    }

    /// Have every device that keeps `workspace`, besides this one and `named`, take in every
    /// change this device keeps, the change naming `named` among them: a device that never
    /// hears of a newly named one cannot link to it once the device that named both is gone.
    /// One that cannot be reached is no live copy to tell, and is told when it next links;
    /// nothing of the workspace is lost by answering without it.
    async fn tell_the_others(&self, workspace: &str, workspace_id: WorkspaceId, named: [u8; 32]) {
        let own = self.inner.host.cert.device_public_key;
        let Ok(devices) = self.hosts(workspace).await else {
            tracing::warn!(%workspace, "the devices that keep a workspace were not read to tell them of a newly named one");
            return;
        };
        for hint in devices {
            let device = host_device(&hint);
            if device == own || device == named {
                continue;
            }
            let told = async {
                let address = WorkspaceDeviceHintV1::from_transport_hint(&hint)
                    .map_err(|error| error.to_string())?;
                let reached = self.reach_host(device, address.endpoint_addr).await?;
                self.caught_up_with_this(workspace, workspace_id, &reached)
                    .await
            }
            .await;
            if let Err(error) = told {
                tracing::warn!(%workspace, device = %hex::encode(device), %error, "a device that keeps the workspace did not take in a newly named one; it may not reach it");
            }
        }
    }

    /// Ask `reached`, a server `workspace` names among those keeping it, to
    /// take in what this device's store keeps of it ([`CATCH_UP_WORKSPACE`]),
    /// and see in where its store stands then that it keeps every change of
    /// this device's it may read, and in what it says it lacks that it keeps
    /// all the content those changes name. The server is asked again only
    /// after an answer that narrowed what it lacks, or that says it fetched
    /// content while it answered, for what reached this device while it took
    /// in the rest: an answer that did neither is its last. What it lacks may
    /// count more than the last answer did and still follow content fetched:
    /// it counts over the resources that open there, which grow as the
    /// workspace's arrive.
    ///
    /// # Errors
    ///
    /// The server did not answer the ask (a build before it, or one that
    /// could not reach this device), or an answer left it lacking changes or
    /// content and neither narrowed it nor fetched any.
    async fn caught_up_with_this(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
        reached: &OtherHost,
    ) -> Result<(), String> {
        let Some(link) = self.inner.direct.link(workspace_id) else {
            // No link serves the workspace here: this device keeps none of
            // its changes in a store to hand over.
            return Ok(());
        };
        let server = reached.certificate.device_public_key;
        // Whether the server waits for the keys of what it cannot open yet:
        // only for content this device's resources name.
        let names_content = self.content_named(workspace).await > 0;
        // What this device keeps none of itself it cannot hand over: the
        // server is not held to it. A list longer than one ask carries goes
        // ahead in parts, and the server counts with it as it holds it.
        let absent = self.content_lacked(workspace).await;
        let parted = absent.len() > ABSENT_PER_ASK;
        if parted {
            hand_absent(reached, workspace, &absent).await?;
        }
        let absent = (!parted).then(|| absent.into_iter().map(Value::String).collect());
        let caught_up = self
            .caught_up_counting(workspace, &link, reached, names_content, absent)
            .await;
        if parted {
            // The list is the server's to let go once the catch-up ended,
            // and it does after a while unasked.
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
            arguments.insert("first".into(), Value::Bool(true));
            arguments.insert("absent".into(), Value::Array(Vec::new()));
            if let Err(error) = reached.ask(CATCH_UP_ABSENT, arguments).await {
                tracing::debug!(%workspace, server = %hex::encode(server), %error, "a server did not let go of the content this device keeps none of");
            }
        }
        caught_up
    }

    /// Ask `reached` to take in what this device keeps of `workspace`, over
    /// `link`, until it says it keeps it all, or answers that neither narrowed
    /// what it lacks, nor fetched any, nor moved where its store stands
    /// ([`Self::caught_up_with_this`]). The content this device keeps none of
    /// is `absent`, or, when `None`, the list it handed the server in parts.
    async fn caught_up_counting(
        &self,
        workspace: &str,
        link: &tilo_peer_state::direct::DirectSync,
        reached: &OtherHost,
        names_content: bool,
        absent: Option<Vec<Value>>,
    ) -> Result<(), String> {
        let server = reached.certificate.device_public_key;
        // What it lacked, could not read, and held before the last answer. Its standing moves
        // when it takes frames in even while this device keeps writing more, so a count that
        // grew is progress too, and not an answer to give up on.
        let mut before: Option<(usize, u32, Vec<tilo_peer_state::store::Cursor>)> = None;
        let mut answers: u32 = 0;
        loop {
            answers = answers.saturating_add(1);
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
            arguments.insert("names_content".into(), Value::Bool(names_content));
            arguments.insert("cursor_pages".into(), Value::Bool(true));
            match &absent {
                Some(absent) => {
                    arguments.insert("absent".into(), Value::Array(absent.clone()));
                }
                None => {
                    arguments.insert("absent_parts".into(), Value::Bool(true));
                }
            }
            let answer = reached
                .ask(CATCH_UP_WORKSPACE, arguments)
                .await
                .map_err(|error| {
                    format!(
                        "the server {} did not take in the changes this device keeps: {error}",
                        hex::encode(server)
                    )
                })?;
            let caught_up: CaughtUpWorkspace =
                serde_json::from_value(answer).map_err(|error| error.to_string())?;
            let mut cursors: Vec<tilo_peer_state::store::Cursor> =
                super::decode_hex(&caught_up.cursors)?;
            if caught_up.more_cursors {
                let rest = read_standing(reached, workspace, cursors.last()).await?;
                cursors.extend(rest);
            }
            cursors.sort_unstable();
            cursors.dedup();
            let lacked = link.lacked_by(server, &cursors)?;
            let content_missing = caught_up.content_missing;
            if lacked == 0 && content_missing == 0 {
                return Ok(());
            }
            let narrowed =
                before
                    .as_ref()
                    .is_none_or(|(lacked_before, missing_before, cursors_before)| {
                        lacked < *lacked_before
                            || content_missing < *missing_before
                            || cursors != *cursors_before
                    });
            if (!narrowed && !caught_up.fetched) || answers >= CATCH_UP_ANSWERS {
                return Err(format!(
                    "the server {} still lacks {lacked} of the changes this device keeps and \
                     {content_missing} blobs of the content they name (each resource still waiting \
                     for its keys there counts as one; {answers} answers)",
                    hex::encode(server)
                ));
            }
            before = Some((lacked, content_missing, cursors));
        }
    }

    /// Take in what the store of `caller`, a device `workspace` names among
    /// those keeping it, keeps of the workspace and this device's store lacks,
    /// over the workspace's device link ([`DirectSync::take_store`]), as the
    /// caller asks before it lets the workspace go. Answers where this
    /// device's store stands then, and how many blobs of the content the
    /// workspace's resources name it still lacks once the caller may have
    /// handed them ([`Self::content_settled`]), beside what `absent`
    /// addresses: the content the caller keeps none of either. Says too
    /// whether it fetched any while it answered.
    ///
    /// [`DirectSync::take_store`]: tilo_peer_state::direct::DirectSync::take_store
    ///
    /// # Errors
    ///
    /// The workspace is not kept here, the caller is not a device keeping it,
    /// or its changes could not be taken in.
    pub(crate) async fn catch_up_from(
        &self,
        caller: &DeviceCert,
        workspace: &str,
        names_content: bool,
        absent: &BTreeSet<String>,
    ) -> Result<(Vec<tilo_peer_state::store::Cursor>, u32, bool), String> {
        let workspace_id = WorkspaceId::parse(workspace)
            .ok_or_else(|| "a workspace is named by its id".to_owned())?;
        let address = self.handing(caller, workspace).await?;
        let cursors = self
            .take_store_of(
                workspace_id,
                address.endpoint_addr,
                address.device_certificate.device_public_key,
            )
            .await?;
        let (content_missing, fetched) = self
            .content_settled(workspace, caller.device_public_key, names_content, absent)
            .await;
        Ok((
            cursors,
            u32::try_from(content_missing).unwrap_or(u32::MAX),
            fetched,
        ))
    }

    /// The address of `caller`, a device `workspace` names among those
    /// keeping it, or the device that handed it to this one.
    ///
    /// # Errors
    ///
    /// The workspace is not kept here, or the caller is not a device keeping
    /// it.
    async fn handing(
        &self,
        caller: &DeviceCert,
        workspace: &str,
    ) -> Result<WorkspaceDeviceHintV1, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let handing = state.lock().await.handed_by.clone();
        let named = self
            .hosts(workspace)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|hint| host_device(hint) == caller.device_public_key)
            .map(|hint| WorkspaceDeviceHintV1::from_transport_hint(&hint))
            .transpose()
            .map_err(|error| error.to_string())?;
        // Until the change naming this device among those keeping the
        // workspace arrives, the device that handed it over is the one it
        // knows of.
        named
            .or_else(|| {
                handing.filter(|handing| {
                    handing.device_certificate.device_public_key == caller.device_public_key
                })
            })
            .ok_or_else(|| "only a device keeping the workspace hands its changes over".to_owned())
    }

    /// Keep `part` of the list of content `caller`, a device keeping
    /// `workspace`, keeps none of itself, for its catch-up to count with
    /// ([`Self::absent_handed`]): the list afresh when `first`, and an empty
    /// first part lets it go. Answers how many addresses the list holds.
    ///
    /// # Errors
    ///
    /// The caller is not a device keeping the workspace, the list's first
    /// part is not here (it was let go, or this device started again), or the
    /// list grew past what one device's list holds here ([`MAX_ABSENT`]).
    pub(crate) async fn keep_absent(
        &self,
        caller: &DeviceCert,
        workspace: &str,
        part: BTreeSet<String>,
        first: bool,
    ) -> Result<usize, String> {
        self.handing(caller, workspace).await?;
        if part
            .iter()
            .any(|address| address.len() > MAX_ABSENT_ADDRESS)
        {
            return Err("a content address is longer than any the workspace names".into());
        }
        self.inner.handed_absent.keep(
            &(caller.device_public_key, workspace.to_owned()),
            part,
            first,
        )
    }

    /// Where this device's store of `workspace` stands on the lineages after
    /// `after`, as `caller`, a device keeping it, reads the rest of its
    /// catch-up's answer ([`Self::standing_page`]).
    ///
    /// # Errors
    ///
    /// The workspace is not kept here, or the caller is not a device keeping
    /// it.
    pub(crate) async fn standing_after(
        &self,
        caller: &DeviceCert,
        workspace: &str,
        after: tilo_peer_state::store::Lineage,
    ) -> Result<(Vec<tilo_peer_state::store::Cursor>, bool), String> {
        let workspace_id = WorkspaceId::parse(workspace)
            .ok_or_else(|| "a workspace is named by its id".to_owned())?;
        self.handing(caller, workspace).await?;
        let cursors = self
            .inner
            .store
            .stands(workspace_id)
            .into_iter()
            .map(|(cursor, _)| cursor)
            .collect();
        Ok(self.standing_page(cursors, Some(after)))
    }

    /// The page of `cursors`, where this device's store stands, that one
    /// answer carries: the cursors of the lineages after `after`, in lineage
    /// order, at most [`CURSORS_PER_ANSWER`], and whether more follow them.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(
            clippy::unused_self,
            reason = "only a test build reads the store's own page size"
        )
    )]
    pub(crate) fn standing_page(
        &self,
        mut cursors: Vec<tilo_peer_state::store::Cursor>,
        after: Option<tilo_peer_state::store::Lineage>,
    ) -> (Vec<tilo_peer_state::store::Cursor>, bool) {
        #[cfg(any(test, feature = "test-support"))]
        let per_answer = match self
            .inner
            .cursors_per_answer
            .load(std::sync::atomic::Ordering::Acquire)
        {
            0 => CURSORS_PER_ANSWER,
            set => set,
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let per_answer = CURSORS_PER_ANSWER;
        cursors.sort_unstable();
        if let Some(after) = after {
            cursors.retain(|cursor| cursor.lineage > after);
        }
        let more = cursors.len() > per_answer;
        cursors.truncate(per_answer);
        (cursors, more)
    }

    /// Answer where this device's store stands after a catch-up
    /// ([`CATCH_UP_STANDING`]) at most `cursors` at a time from now on: what a
    /// test reads a store of a few records in many pages with.
    #[cfg(any(test, feature = "test-support"))]
    pub fn answer_cursors_in_pages_of(&self, cursors: usize) {
        self.inner
            .cursors_per_answer
            .store(cursors, std::sync::atomic::Ordering::Release);
    }

    /// The list of content the device `caller` keeps none of itself, as it
    /// handed it here in parts for `workspace` ([`Self::keep_absent`]); `None`
    /// when no list of it is here.
    pub(crate) fn absent_handed(
        &self,
        caller: [u8; 32],
        workspace: &str,
    ) -> Option<Arc<BTreeSet<String>>> {
        self.inner
            .handed_absent
            .counted_with(&(caller, workspace.to_owned()))
    }

    /// How many blobs of the content `workspace`'s resources name this device
    /// keeps none of, once `from`, the device handing the workspace over,
    /// may have handed them: none are lacked, fewer than when this was asked,
    /// or `from` answered that it keeps none of one. A resource still waiting
    /// for its keys counts as one blob lacked when `names_content`, as `from`
    /// says when its own resources name content. Content `absent` addresses,
    /// which `from` keeps none of either, is not counted. The workspace's members are
    /// asked again for all this device lacks first, and each count after that
    /// follows an answer to those asks, a write to the workspace, or a change
    /// to its member list. Says too whether that count is below the first:
    /// content arrived while it waited. A count that has not dropped within
    /// [`CONTENT_SETTLES_WITHIN`] is answered as it stands.
    async fn content_settled(
        &self,
        workspace: &str,
        from: [u8; 32],
        names_content: bool,
        absent: &BTreeSet<String>,
    ) -> (usize, bool) {
        use tokio::sync::broadcast::error::RecvError;
        // Subscribed before the asks go out, so no answer to them is missed.
        let mut answers = self.content_answers();
        // A resource waiting for its keys opens as a write lands or the
        // workspace's member list moves, which may name no content at all.
        let mut writes = self.document_writes();
        let mut changes = self.governance_changes();
        drop(self.inner.reached.send(workspace.to_owned()));
        let asked = self.content_lacking(workspace, names_content, absent).await;
        let mut lacking = asked;
        // Nothing moving for this long ends the wait, with what is still lacked: the device handing
        // the workspace over then hears that nothing narrowed, and says so, rather than waiting
        // on a resource whose keys never come.
        let quiet = tokio::time::sleep(CONTENT_SETTLES_WITHIN);
        tokio::pin!(quiet);
        while lacking > 0 {
            let lacked_by_it = tokio::select! {
                () = &mut quiet => {
                    tracing::warn!(
                        %workspace,
                        from = %hex::encode(from),
                        lacking,
                        waits_for_keys = names_content,
                        "a hand-over's content did not move for {} s: it still lacks this many \
                         blobs, counting each resource still waiting for its keys as one",
                        CONTENT_SETTLES_WITHIN.as_secs()
                    );
                    break;
                }
                answer = answers.recv() => match answer {
                    Ok(answer) if answer.workspace == workspace => {
                        answer.lacks && answer.from == from
                    }
                    Ok(_) => continue,
                    // What this missed may be its own: the count says.
                    Err(RecvError::Lagged(_)) => false,
                    Err(RecvError::Closed) => break,
                },
                write = writes.recv() => match write {
                    Ok(write) if write.workspace == workspace => false,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => false,
                    Err(RecvError::Closed) => break,
                },
                change = changes.recv() => match change {
                    Ok(moved) if moved == workspace => false,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => false,
                    Err(RecvError::Closed) => break,
                },
            };
            lacking = self.content_lacking(workspace, names_content, absent).await;
            if lacked_by_it || lacking < asked {
                break;
            }
        }
        (lacking, lacking < asked)
    }

    /// Take in what `device` at `addr`, one keeping `workspace`
    /// (`workspace_id`), keeps of it and this device's store lacks, over the
    /// workspace's device link, into this device's one store as a cold start
    /// takes a workspace in, so a page and the content kept here read it at
    /// once. Answers where this device's store stands then.
    ///
    /// # Errors
    ///
    /// No link serves the workspace here, or its changes could not be taken in.
    async fn take_store_of(
        &self,
        workspace_id: WorkspaceId,
        addr: EndpointAddr,
        device: [u8; 32],
    ) -> Result<Vec<tilo_peer_state::store::Cursor>, String> {
        let link = self
            .inner
            .direct
            .link(workspace_id)
            .ok_or("no link serves the workspace on this device")?;
        let cursors = link
            .take_store(&self.inner.host.endpoint, addr, device)
            .await
            .map_err(|refusal| {
                format!(
                    "the changes {} keeps were not taken in: {refusal}",
                    hex::encode(device)
                )
            })?;
        // The store it took is the one it keeps: every change it holds is in
        // its own history already, and what its copy needs of them follows
        // from there.
        Ok(cursors)
    }

    /// Answer the catch-up a device keeping a workspace asks for
    /// ([`CATCH_UP_WORKSPACE`]) from now on as a build before the ask does:
    /// not at all. What a test hands a workspace to such a build with.
    #[cfg(any(test, feature = "test-support"))]
    pub fn speak_no_catch_up(&self) {
        self.inner
            .speaks_no_catch_up
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether this service answers no catch-up ([`Self::speak_no_catch_up`]).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn speaks_no_catch_up_now(&self) -> bool {
        self.inner
            .speaks_no_catch_up
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Have the holder `device` of `workspace` hold it as `role` from now on,
    /// as its owners chose: its signed entry among the workspace's holders
    /// says so, and every device reads who gets the workspace's keys from it.
    /// Made a store, it leaves the workspace's key group, whose commit
    /// removing it rotates the keys, so it reads nothing written after; made a
    /// key holder, the group admits it. Another device is handed the
    /// workspace afresh in its new role at the address it is named at; this
    /// device signs its own entry. Answers the holder's address.
    ///
    /// # Errors
    ///
    /// The workspace is not held here or does not name `device` among its
    /// holders, or the holder refused or could not be reached.
    pub async fn set_holder_role(
        &self,
        workspace: &str,
        device: [u8; 32],
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let (named, own) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let named = governed
                .peer()
                .governance()
                .keeping_devices()
                .into_iter()
                .find(|hint| host_device(hint) == device);
            let own = (device == self.inner.host.cert.device_public_key)
                .then(|| self.transport_hint_as(&governed, role))
                .transpose();
            drop(governed);
            (named, own.map_err(|error| error.to_string())?)
        };
        let named =
            named.ok_or_else(|| format!("{} does not hold the workspace", hex::encode(device)))?;
        if tilo_peer_state::holder_role(&named) == role {
            return Ok(named);
        }
        let Some(own) = own else {
            let addr = WorkspaceDeviceHintV1::from_transport_hint(&named)
                .map_err(|error| error.to_string())?
                .endpoint_addr;
            return self.attach_server(workspace, device, addr, role).await;
        };
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device(&own, key, now)
        })
        .await?;
        tracing::info!(%workspace, ?role, "this device holds the workspace in another role");
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(own)
    }

    /// What `workspace` is handed to the device `member` with, to hold it as
    /// `role`: the workspace as a joining member gets it and this device's
    /// own signed address, under the workspace's id. A device of this one's
    /// own person is admitted to the workspace as one of the owner's devices
    /// first, so it takes in and writes the workspace's changes as they do;
    /// named a store, it holds no keys all the same
    /// ([`tilo_peer_state::PeerGovernance::key_holders`]).
    async fn hand_over(
        &self,
        workspace: &str,
        member: &DeviceCert,
        role: CopyRole,
    ) -> Result<(WorkspaceId, Map<String, Value>), String> {
        if member.user_public_key == self.inner.host.cert.user_public_key {
            self.enter(workspace, member)
                .await
                .map_err(|error| error.to_string())?;
        }
        let (workspace_id, holder_hint, bootstrap) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let mut bootstrap = governed
                .governed(self.serving_certificate())
                .bootstrap([0; 32], member.clone(), Role::Editor)
                .map_err(|error| error.to_string())?;
            // The host follows governance alone: records reach it over the
            // attach session, and leaving them here keeps the hand-over
            // within one action frame.
            bootstrap.records.clear();
            let hint = self
                .transport_hint(&governed)
                .map_err(|error| error.to_string())?;
            (governed.workspace_id(), hint, bootstrap)
        };
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        arguments.insert(
            "workspace_id".into(),
            Value::String(workspace_id.to_string()),
        );
        arguments.insert(
            "holder_hint".into(),
            Value::String(hex_postcard(&holder_hint)?),
        );
        arguments.insert("bootstrap".into(), Value::String(hex_postcard(&bootstrap)?));
        arguments.insert(
            "role".into(),
            serde_json::to_value(role).map_err(|error| error.to_string())?,
        );
        Ok((workspace_id, arguments))
    }

    /// Hand `workspace` to the device `device` at `addr` as its owner's
    /// choice of it does, but under the name `named`, and answer what that
    /// device said: what a test asks with whether a device that is handed a
    /// workspace takes it under any name but its own id.
    ///
    /// # Errors
    ///
    /// The device was not reached, or refused.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn hand_over_named(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: EndpointAddr,
        named: &str,
    ) -> Result<Value, String> {
        let reached = self.reach_host(device, addr).await?;
        let (_, mut arguments) = self
            .hand_over(workspace, &reached.certificate, CopyRole::KeyHolder)
            .await?;
        arguments.insert("workspace".into(), Value::String(named.to_owned()));
        reached.ask(HOST_WORKSPACE, arguments).await
    }

    /// Stop having the holder `device` hold `workspace`, as its owners chose:
    /// its address leaves the workspace's holders, and the holder is asked to
    /// stop ([`UNHOST_WORKSPACE`]) at `addr` when given. A device this one
    /// reaches hands over what it keeps first: this device takes in every
    /// change of the workspace that device's store keeps and its own lacks,
    /// so none of them goes with it. This device itself goes only once another
    /// device keeping the workspace says it keeps every change this one does
    /// ([`Self::caught_up_with_this`]). What only that holder did passes on
    /// with its entry: the workspace's compacted base is made by the holder
    /// governance names for it
    /// ([`tilo_peer_state::PeerGovernance::base_maker`]), so with this one
    /// gone the next key holder of the owner makes it. Answers whether the
    /// holder confirmed it stopped; one that could not be reached is still no
    /// longer named to members.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, its hosts could not be recorded, or
    /// the changes a device reached keeps, or this device's, were not taken
    /// in: that device keeps the workspace then.
    pub async fn detach_host(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: Option<EndpointAddr>,
    ) -> Result<bool, String> {
        let (workspace_id, known) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let known = governed
                .peer()
                .governance()
                .keeping_devices()
                .iter()
                .find(|known| host_device(known) == device)
                .map(|known| {
                    WorkspaceDeviceHintV1::from_transport_hint(known)
                        .map(|address| address.endpoint_addr)
                })
                .and_then(Result::ok);
            (governed.workspace_id(), known)
        };
        let reached = if device == self.inner.host.cert.device_public_key {
            self.caught_up_elsewhere(workspace, workspace_id).await?;
            None
        } else if let Some(addr) = addr.or(known) {
            match self.reach_host(device, addr.clone()).await {
                Ok(reached) => {
                    self.take_store_of(workspace_id, addr, device)
                        .await
                        .map_err(|error| {
                            format!("the server keeps the workspace still: {error}")
                        })?;
                    Some(reached)
                }
                Err(error) => {
                    tracing::info!(%workspace, host = %hex::encode(device), %error, "a detached host was not reached");
                    None
                }
            }
        } else {
            None
        };
        self.name_host(workspace, |governance, key, now| {
            governance.author_keeping_device_removed(&device, key, now)
        })
        .await?;
        let _ = self.inner.changes.send(workspace.to_owned());
        let Some(reached) = reached else {
            return Ok(false);
        };
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        match reached.ask(UNHOST_WORKSPACE, arguments).await {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::info!(%workspace, host = %hex::encode(device), %error, "a detached host did not say it stopped");
                Ok(false)
            }
        }
    }

    /// Have another device keeping `workspace` take in every change this
    /// device's store keeps of it ([`Self::caught_up_with_this`]), as this
    /// device does before it stops keeping the workspace itself: the first
    /// that says it keeps them all is enough.
    ///
    /// # Errors
    ///
    /// No other device keeps the workspace, or none of them said it keeps
    /// every change this one does.
    async fn caught_up_elsewhere(
        &self,
        workspace: &str,
        workspace_id: WorkspaceId,
    ) -> Result<(), String> {
        let own = self.inner.host.cert.device_public_key;
        let mut why = Vec::new();
        let others = self
            .hosts(workspace)
            .await
            .map_err(|error| error.to_string())?;
        for hint in others.iter().filter(|hint| host_device(hint) != own) {
            let caught_up = async {
                let address = WorkspaceDeviceHintV1::from_transport_hint(hint)
                    .map_err(|error| error.to_string())?;
                let reached = self
                    .reach_host(host_device(hint), address.endpoint_addr)
                    .await?;
                self.caught_up_with_this(workspace, workspace_id, &reached)
                    .await
            }
            .await;
            match caught_up {
                Ok(()) => return Ok(()),
                Err(error) => why.push(error),
            }
        }
        Err(if why.is_empty() {
            "this device keeps the workspace: no other device keeps it to take its changes".into()
        } else {
            format!(
                "this device keeps the workspace: no other device keeping it took in its \
                 changes ({})",
                why.join("; ")
            )
        })
    }

    /// Whether `caller` chooses which hosts hold `governed`: a device of the
    /// workspace's owner, who decides where it lives; or that person's server key, which
    /// acts for them from a script or a command. Never an agent's grant, and
    /// never a device for being of the person whose host this is: every
    /// holder answers it alike.
    pub(super) fn chooses_servers(governed: &GovernedWorkspace, caller: &DeviceCert) -> bool {
        Self::speaks_for_person(governed, caller)
            && caller.user_public_key == governed.peer().owner()
    }

    /// Whether `caller` speaks for its person in `governed`: a device of theirs, or their
    /// server key there, never an agent's grant.
    pub(super) fn speaks_for_person(governed: &GovernedWorkspace, caller: &DeviceCert) -> bool {
        caller.capabilities.agent.is_none()
            || caller
                .capabilities
                .own_key_in(governed.peer().workspace_id())
    }

    /// The servers keeping `workspace`, as its governance names them: this
    /// one, the server asked, first, then each other one by the name it signs
    /// into its own address.
    ///
    /// # Errors
    ///
    /// The workspace is not held here.
    pub async fn keeping_devices(&self, workspace: &str) -> Result<Vec<KeepingDevice>, String> {
        let hosts = self
            .hosts(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut holders = holders_of(&hosts);
        let here = hex::encode(self.inner.host.cert.device_public_key);
        holders.sort_by_key(|holder| holder.device != here);
        Ok(holders
            .into_iter()
            .map(|holder| KeepingDevice {
                here: holder.device == here,
                device: holder.device,
                person: holder.person,
                label: holder.label,
                reads: holder.reads,
                cloud: holder.cloud,
            })
            .collect())
    }

    /// Have the server whose own line is `host` hold `workspace` as well, as
    /// `role`: the line names its certificate and where it is dialled
    /// (`tilo_server_protocol::web::ServerInfo::web_host_json`), its operator
    /// hands it over, and the server is then handed the workspace
    /// ([`Self::attach_server`]), which it takes as its own rules for whom it
    /// keeps workspaces say.
    ///
    /// # Errors
    ///
    /// As [`Self::attach_server`], or `host` is no host's own line.
    pub async fn add_server(
        &self,
        workspace: &str,
        host: &str,
        role: CopyRole,
    ) -> Result<TransportHint, String> {
        let (device, addr) = named_host(host)?;
        self.attach_server(workspace, device, addr, role).await
    }

    /// Stop having the holder `named` hold `workspace`: named by its device
    /// key (64 hex) or by the name [`Self::keeping_devices`] lists it under.
    /// Answers whether the holder confirmed it let the workspace go
    /// ([`Self::detach_host`]).
    ///
    /// # Errors
    ///
    /// The holder is not one of the workspace's, or no holder of it goes by
    /// `named`.
    pub async fn remove_server(&self, workspace: &str, named: &str) -> Result<bool, String> {
        let named = named.trim();
        let device = match device_key(named) {
            Some(device) => device,
            None => self
                .keeping_devices(workspace)
                .await?
                .into_iter()
                .find(|holder| holder.label == named)
                .and_then(|holder| device_key(&holder.device))
                .ok_or_else(|| format!("no server keeping the workspace goes by {named:?}"))?,
        };
        if !self.names_server(workspace, device).await {
            return Err(format!("{named} does not hold the workspace"));
        }
        self.detach_host(workspace, device, None).await
    }

    /// Move `workspace` from the server `from` (its device key, or the name
    /// [`Self::keeping_devices`] lists it under) to the server whose own line
    /// is `host` ([`Self::move_holder`]).
    ///
    /// # Errors
    ///
    /// As [`Self::move_holder`], or `from` names no holder and `host` is no
    /// host's own line.
    pub async fn move_server(
        &self,
        workspace: &str,
        from: &str,
        host: &str,
    ) -> Result<bool, String> {
        let from = from.trim();
        let old = match device_key(from) {
            Some(device) => device,
            None => self
                .keeping_devices(workspace)
                .await?
                .into_iter()
                .find(|holder| holder.label == from)
                .and_then(|holder| device_key(&holder.device))
                .ok_or_else(|| format!("no server keeping the workspace goes by {from:?}"))?,
        };
        let (new, addr) = named_host(host)?;
        self.move_holder(workspace, old, new, addr)
            .await
            .map_err(|error| {
                error
                    .replace("the old server", from)
                    .replace("the new server", host)
            })
    }

    /// Move `workspace` from the server `old` to the server `new` at `addr`:
    /// `new` keeps it as well first, in the role `old` kept it in, and says it
    /// keeps every change this device keeps ([`Self::attach_server`]); then
    /// `old` hands over what it keeps and keeps the workspace no more
    /// ([`Self::detach_host`]). Answers whether `old` confirmed it let the
    /// workspace go.
    ///
    /// # Errors
    ///
    /// `old` does not keep the workspace, or `new` did not take it: the
    /// workspace then stays where it was. Or `new` took it and `old` could
    /// not be let go: both keep it then, and the error says so.
    pub async fn move_holder(
        &self,
        workspace: &str,
        old: [u8; 32],
        new: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<bool, String> {
        if !self.names_server(workspace, old).await {
            return Err("the old server does not keep the workspace".into());
        }
        let role = self
            .hosts(workspace)
            .await
            .ok()
            .and_then(|hosts| {
                hosts
                    .iter()
                    .find(|hint| host_device(hint) == old)
                    .map(tilo_peer_state::holder_role)
            })
            .unwrap_or(CopyRole::KeyHolder);
        self.attach_server(workspace, new, addr, role)
            .await
            .map_err(|error| {
                format!(
                    "the new server did not take the workspace, which stays where it was: {error}"
                )
            })?;
        self.detach_host(workspace, old, None).await.map_err(|error| {
            format!("the new server keeps the workspace now, and so does the old server still: {error}")
        })
    }

    /// `device`, a host at `addr`, reached for its host operations: its
    /// certificate, as its dispatch hands it, and the dispatch it is asked on.
    /// The connection pins the device, whoever's it is.
    async fn reach_host(&self, device: [u8; 32], addr: EndpointAddr) -> Result<OtherHost, String> {
        let host = &self.inner.host;
        let actions = Arc::new(self.action_service().remote(
            &host.endpoint,
            &addr,
            &tilo_server_protocol::WorkspaceId::nil().to_string(),
        ));
        if actions.device() != device {
            return Err(format!(
                "the device reached is {}, not {}",
                hex::encode(actions.device()),
                hex::encode(device)
            ));
        }
        let certificate = actions
            .certificate()
            .await
            .map_err(|error| format!("the host did not say who it is: {error}"))?;
        Ok(OtherHost {
            certificate,
            actions,
        })
    }
}

/// Another host, reached for its host operations ([`Workspaces::reach_host`]).
struct OtherHost {
    /// Its certificate, as its dispatch handed it.
    certificate: DeviceCert,
    /// Its actions, asked over the link this host holds to it.
    actions: Arc<RemoteDevice>,
}

impl OtherHost {
    /// Ask the host the host operation `action` with `arguments`, and answer
    /// what it produced.
    async fn ask(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let request = host_invocation(action, arguments);
        match Arc::clone(&self.actions)
            .invoke_until_settled(request, HOST_ANSWERED)
            .await
        {
            Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                Ok(output.value.unwrap_or_default())
            }
            Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
            Some(Ok(_)) => Err("the host did not finish the call".to_owned()),
            Some(Err(error)) => Err(error.to_string()),
            None => Err("the host did not answer".to_owned()),
        }
    }
}

/// Where `reached`'s store of `workspace` stands on the lineages after the
/// one `last` names, read page by page ([`CATCH_UP_STANDING`]).
///
/// # Errors
///
/// The server did not answer a page, or answered one it cannot mean.
async fn read_standing(
    reached: &OtherHost,
    workspace: &str,
    last: Option<&tilo_peer_state::store::Cursor>,
) -> Result<Vec<tilo_peer_state::store::Cursor>, String> {
    let mut read = Vec::new();
    let mut after = last.map(|cursor| cursor.lineage);
    while let Some(lineage) = after {
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        arguments.insert(
            "after".into(),
            Value::String(hex::encode(
                postcard::to_stdvec(&lineage).map_err(|error| error.to_string())?,
            )),
        );
        let answer = reached
            .ask(CATCH_UP_STANDING, arguments)
            .await
            .map_err(|error| {
                format!(
                    "the server {} did not say where its store of the workspace stands: {error}",
                    hex::encode(reached.certificate.device_public_key)
                )
            })?;
        let page: StandingPage =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let cursors: Vec<tilo_peer_state::store::Cursor> = super::decode_hex(&page.cursors)?;
        // Each page goes on after the last: one that does not is the end.
        after = cursors
            .last()
            .map(|cursor| cursor.lineage)
            .filter(|next| page.more && *next > lineage);
        read.extend(cursors);
    }
    Ok(read)
}

/// Hand `reached` the list of content this device keeps none of itself of
/// `workspace`, `absent`, in parts each one ask carries ([`CATCH_UP_ABSENT`]),
/// for the catch-up after to count with.
///
/// # Errors
///
/// The server did not take a part: a build before lists in parts knows no
/// such action.
async fn hand_absent(
    reached: &OtherHost,
    workspace: &str,
    absent: &BTreeSet<String>,
) -> Result<(), String> {
    let addresses: Vec<&String> = absent.iter().collect();
    for (index, part) in addresses.chunks(ABSENT_PER_ASK).enumerate() {
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        arguments.insert("first".into(), Value::Bool(index == 0));
        arguments.insert(
            "absent".into(),
            Value::Array(
                part.iter()
                    .map(|address| Value::String((*address).clone()))
                    .collect(),
            ),
        );
        reached
            .ask(CATCH_UP_ABSENT, arguments)
            .await
            .map_err(|error| {
                format!(
                    "the server {} did not take the list of the {} blobs of content this device \
                     keeps none of: {error}",
                    hex::encode(reached.certificate.device_public_key),
                    absent.len()
                )
            })?;
    }
    Ok(())
}

/// The invocation of the host operation `action` with `arguments`.
fn host_invocation(action: &str, arguments: Map<String, Value>) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// `value` as the hand-over sends it: postcard in hex.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, String> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| error.to_string())
}

/// One host of a workspace, as its owners list them ([`Workspaces::keeping_devices`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeepingDevice {
    /// The host's device key, hex: what removing it may name it by.
    pub device: String,
    /// The person the host runs for, by their user public key, hex.
    pub person: String,
    /// Whether this is the server asked: the one the list was read from.
    pub here: bool,
    /// What it calls itself, as its own signed address names it.
    pub label: String,
    /// Whether it holds the workspace's keys: it reads the workspace. A store
    /// keeps its ciphertext and reads none of it.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider, whose
    /// operator can read what it holds.
    pub cloud: bool,
}

/// `holders` as a member reads them: each named by its own signed address. A
/// holder that names itself nowhere goes by its device's label, and one whose
/// address does not decode reads as holding no keys.
pub(super) fn holders_of(holders: &[TransportHint]) -> Vec<WorkspaceHolder> {
    let mut keepers: Vec<WorkspaceHolder> = holders
        .iter()
        .map(|hint| {
            let payload = WorkspaceDeviceHintV1::from_transport_hint(hint).ok();
            let label = payload
                .as_ref()
                .map(|payload| payload.label.trim())
                .filter(|label| !label.is_empty())
                .unwrap_or(&hint.host.device_label);
            WorkspaceHolder {
                device: hex::encode(hint.host.device_public_key),
                person: hex::encode(hint.host.user_public_key),
                label: label.to_owned(),
                reads: payload.as_ref().is_some_and(|payload| payload.holds_keys),
                cloud: payload.as_ref().is_some_and(|payload| payload.cloud),
            }
        })
        .collect();
    keepers.sort_by(|a, b| a.label.cmp(&b.label));
    keepers
}

/// `text` as a device key, when it is 64 hex characters.
fn device_key(text: &str) -> Option<[u8; 32]> {
    use hex::FromHex as _;
    <[u8; 32]>::from_hex(text).ok()
}

/// The host `text` names, as the host's own line says
/// (`tilo_server_protocol::web::ServerInfo::web_host_json`): its certificate,
/// where it is dialled, and the web origin its links name. Its operator hands
/// it over, as a ticket is handed over; no site of the host's is read to learn
/// where it is.
fn named_host(text: &str) -> Result<([u8; 32], EndpointAddr), String> {
    let host: tilo_server_protocol::web::PageHost = serde_json::from_str(text.trim())
        .map_err(|error| format!("that is not the host's own web-host.json line: {error}"))?;
    let address = host
        .address
        .ok_or("that host names no address to reach it at")?;
    Ok((host.server_cert.device_public_key, address.endpoint_addr))
}

impl Workspaces {
    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `workspace`, saying the role this device holds it in as
    /// its governance names it (a key holder until it names one): the relay
    /// this host is reached through and the addresses a stranger can dial,
    /// never the ones only the network around this host knows
    /// (`tilo_delivery_protocol::workspace_join::reachable`).
    pub(super) fn transport_hint(
        &self,
        workspace: &GovernedWorkspace,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        self.transport_hint_as(workspace, self.own_role(workspace))
    }

    /// The role this device's governance names it in `workspace`, a key
    /// holder until it names one.
    fn own_role(&self, workspace: &GovernedWorkspace) -> CopyRole {
        workspace
            .peer()
            .governance()
            .holder_role(&self.inner.host.cert.device_public_key)
            .unwrap_or(CopyRole::KeyHolder)
    }

    /// The address an invitation names: this device's once it is reached through one of its
    /// relays, which wake for it and are waited for up to a net report, because a link signed
    /// before this device reached one names only the network it stands in. A device that reaches
    /// none in that time names the address it has, so one with no way out still hands out a link.
    /// With nothing held back, a service that opened before its endpoint reached one of its
    /// relays ([`tilo_endpoint_reachability::Reaching::WhenNeeded`]) waits for it the same way;
    /// any other names the address the endpoint has. Asked before the workspace is locked: the
    /// wait belongs to the invitation, not to everything the workspace answers.
    pub(super) async fn invitation_addr(&self) -> EndpointAddr {
        match self
            .reachability()
            .reached_addr("an invitation names this device's address")
        {
            Some(reached) => reached.await,
            None if self.inner.host.reaching
                == tilo_endpoint_reachability::Reaching::WhenNeeded =>
            {
                tilo_endpoint_reachability::await_relay(&self.inner.host.endpoint).await
            }
            None => self.endpoint_addr(),
        }
    }

    /// The hint an invitation of `workspace` carries, signed for this device answering at `addr`.
    pub(super) fn invitation_hint(
        &self,
        workspace: &GovernedWorkspace,
        addr: &EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        self.signed_hint(workspace, self.own_role(workspace), addr)
    }

    /// [`Self::transport_hint`], saying this device holds `workspace` as
    /// `role`: a store neither holds its keys nor takes its work.
    pub(super) fn transport_hint_as(
        &self,
        workspace: &GovernedWorkspace,
        role: CopyRole,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let addr = self.endpoint_addr();
        self.signed_hint(workspace, role, &addr)
    }

    /// Sign `workspace`'s answer for this device holding it `role`, answering at `addr`.
    fn signed_hint(
        &self,
        workspace: &GovernedWorkspace,
        role: CopyRole,
        addr: &EndpointAddr,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let holds_keys = role == CopyRole::KeyHolder;
        let host = &self.inner.host;
        // An invitation is handed to whoever the link reaches, so what this
        // host answers at inside its own network stays out of it.
        let endpoint_addr = tilo_delivery_protocol::workspace_join::reachable(addr);
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        let claim = host.holder();
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
            version: 1,
            holds_keys,
            runs_work: holds_keys && !host.work.takes_nothing(),
            label: claim.label,
            cloud: claim.cloud,
            origin: claim.origin,
            workspace_id: workspace.workspace_id(),
            endpoint_addr,
            user_public_key: host.cert.user_public_key,
            device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }

    /// Name this keeper among the holders of every workspace it serves as
    /// `label`, which its signed addresses carry from now on: a server's name
    /// its person changed in their admin workspace, so members read the name
    /// it goes by now.
    pub async fn name_holder(&self, label: &str) {
        let label = label.trim();
        self.claim(|claim| {
            if claim.label == label {
                return false;
            }
            claim.label.clear();
            claim.label.push_str(label);
            true
        })
        .await;
    }

    /// Change what this keeper says of itself among the holders with
    /// `change`, and sign its address anew in every workspace it serves when
    /// that changed anything.
    async fn claim(&self, change: impl FnOnce(&mut crate::workspaces::HolderClaim) -> bool) {
        {
            let mut claim = self.inner.host.holder.write_recover();
            if !change(&mut claim) {
                return;
            }
        }
        self.name_own_hosts().await;
    }
}
