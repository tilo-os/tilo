//! One link per enrolled workspace: dialling the host its enrollment pins,
//! holding the link ([`hold`]) over the direct lane, admitting what arrives
//! into this device's owner, and publishing what the owner holds.
mod cursor;
mod hold;
mod reach;

pub use hold::{Holder, Link, hold};
pub use reach::{Linked, OwnHost, Reach};

use super::publication::SealRecord;
use crate::direct::DirectSync;
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::{Reseal, join::TransportHint};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::sync::{Arc, Mutex};
use tilo_identity_host::custody::HostIdentity;
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// An enrolled workspace to hold a link for.
#[derive(Clone)]
pub struct Connect {
    owner: NativeWorkspaceOwner,
    workspace: WorkspaceId,
}
impl std::fmt::Debug for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NativeConnect")
            .field(&self.workspace)
            .finish()
    }
}

impl NativeWorkspaceOwner {
    /// Where `workspace`'s host is reached, with the device its enrollment
    /// pins: the address a plane of the workspace dials that host at,
    /// decoded from the hint the host signed. `None` while the workspace is
    /// not held here or its enrollment names no iroh address.
    #[must_use]
    pub fn host_addr(&self, workspace: WorkspaceId) -> Option<([u8; 32], iroh::EndpointAddr)> {
        let hint = self
            .with_current(workspace, |view| Ok(view.transport_hint()?.clone()))
            .ok()?;
        let (payload, _) =
            tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::authenticate_transport_hint(
                &hint,
            )
            .ok()?;
        Some((hint.host.device_public_key, payload.endpoint_addr))
    }
}

impl Connect {
    /// # Errors
    /// `workspace` has no committed enrollment.
    pub fn new(owner: NativeWorkspaceOwner, workspace: WorkspaceId) -> Result<Self, String> {
        owner.with_current(workspace, |_| Ok(()))?;
        Ok(Self { owner, workspace })
    }
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    /// What this device must be, now, to act as `workspace`'s own member: the
    /// certificate it holds for the workspace is the one the workspace's
    /// record names, admitted in the workspace's roster, and neither it nor
    /// the workspace's host is revoked. Along with it, the hint the enrollment
    /// pins and the owner's continuity.
    pub fn material(
        &self,
        identity: &HostIdentity,
    ) -> Result<(TransportHint, DeviceCert, u64), String> {
        let (cert, continuity) = self.custody(identity)?;
        let hint = self
            .owner
            .with_current(self.workspace, |view| Ok(view.transport_hint()?.clone()))?;
        Ok((hint, cert, continuity))
    }
    /// Whether this device may act as `workspace`'s own member now, and the
    /// continuity its record is at ([`Self::material`]).
    pub fn custody(&self, identity: &HostIdentity) -> Result<(DeviceCert, u64), String> {
        self.owner.with_current(self.workspace, |view| {
            let (cert, entries) = identity.current_native_material(view.workspace_id())?;
            if &cert != view.device() {
                return Err("native custody differs from enrollment".into());
            }
            view.peer()
                .permissions(cert.device_public_key, std::time::SystemTime::now())
                .map_err(|error| error.to_string())?;
            if !view.peer().certificates().contains(&cert) {
                return Err("selected certificate differs from current admitted roster".into());
            }
            if its_root_revoked(
                entries.into_iter().chain(view.peer().revocations()),
                view.workspace_host(),
            ) {
                return Err("native host is revoked".into());
            }
            Ok((cert, view.continuity()))
        })
    }
    /// The hosts the workspace's governor named besides itself
    /// ([`crate::workspace_hosts_of`]), in the order governance holds
    /// them: where a member's link goes while the governor is out of
    /// reach. A host the workspace revoked, or one naming this device, is left
    /// out.
    pub fn hosts(&self, own: &[u8; 32]) -> Result<Vec<TransportHint>, String> {
        self.owner.with_current(self.workspace, |view| {
            let governor = view.transport_hint()?.host.device_public_key;
            let revocations = view.peer().revocations();
            Ok(view
                .peer()
                .governance()
                .workspace_hosts()
                .into_iter()
                .filter(|hint| {
                    let device = hint.host.device_public_key;
                    device != governor
                        && &device != own
                        && !its_root_revoked(revocations.iter().cloned(), &hint.host)
                })
                .collect())
        })
    }

    /// The host to dial after `after` (the first when `None`)
    /// among [`Self::hosts`]: where the workspace's link goes next while its
    /// governor is out of reach. `None` once every host was tried.
    pub fn next_host(
        &self,
        own: &[u8; 32],
        after: Option<&TransportHint>,
    ) -> Result<Option<TransportHint>, String> {
        let hosts = self.hosts(own)?;
        let next = after
            .and_then(|after| hosts.iter().position(|host| host == after))
            .map_or(0, |index| index.saturating_add(1));
        Ok(hosts.into_iter().nth(next))
    }

    /// Whether this device is the workspace's host itself: the host its
    /// enrollment pins is this very device. Such a workspace is never dialled:
    /// its link pairs with the host in this process.
    #[must_use]
    pub fn hosted_here(&self, identity: &HostIdentity) -> bool {
        self.owner
            .with_current(self.workspace, |view| {
                let (cert, _) = identity.current_native_material(view.workspace_id())?;
                Ok(view.transport_hint()?.host.device_public_key == cert.device_public_key)
            })
            .unwrap_or(false)
    }
    /// The renewal of the certificate the workspace's record holds, when this
    /// device's custody holds one: the device proves its membership with it
    /// ([`crate::owner::enroll::renew_membership`]) before its links dial again,
    /// as [`Self::material`] refuses custody that differs from the record.
    pub fn renewal(&self, identity: &HostIdentity) -> Result<Option<DeviceCert>, String> {
        self.owner.with_current(self.workspace, |view| {
            let (cert, _) = identity.current_native_material(view.workspace_id())?;
            Ok(crate::owner::renewal::renews(&cert, view.device()).then_some(cert))
        })
    }
    /// This device's end of the workspace's link, syncing the replica its
    /// owner decides on: what a link brings is durable as it lands.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn link(&self, key: SigningKey) -> Result<DirectSync, String> {
        DirectSync::over(self.owner.replica(self.workspace)?, key)
            .owning(self.owner.clone(), self.workspace)
    }
    /// The owner of the workspace's record.
    #[must_use]
    pub fn owner(&self) -> &NativeWorkspaceOwner {
        &self.owner
    }
}

/// A live link's pin; closed when the link ends.
///
/// The pin is the host it reached and the certificate it signs as. The owner's
/// continuity moves with any security change of the workspace, another member
/// joining included; the link follows it in place once this device's own
/// standing was checked again ([`Connect::material`]), rather than ending and
/// redialling every member for one person's join.
pub struct Scope {
    connect: Connect,
    hint: TransportHint,
    /// The host the link reached: the governor `hint` names, or a host the
    /// workspace's governance names while the governor is out of reach.
    via: TransportHint,
    device: DeviceCert,
    /// The owner continuity the link last validated against.
    continuity: std::sync::atomic::AtomicU64,
    live: Mutex<bool>,
    cursor: Mutex<cursor::Cursor>,
}
impl Scope {
    #[must_use]
    pub fn new(connect: Connect, hint: TransportHint, device: DeviceCert, continuity: u64) -> Self {
        Self::via(connect, hint.clone(), hint, device, continuity)
    }
    /// A link pinned to the workspace's governor `hint` that reached the host
    /// `via` instead.
    #[must_use]
    pub fn via(
        connect: Connect,
        hint: TransportHint,
        via: TransportHint,
        device: DeviceCert,
        continuity: u64,
    ) -> Self {
        Self {
            connect,
            hint,
            via,
            device,
            continuity: std::sync::atomic::AtomicU64::new(continuity),
            live: Mutex::new(true),
            cursor: Mutex::new(cursor::Cursor::default()),
        }
    }
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.connect.workspace
    }
    /// Whether this link is still pinned to this material: the same host
    /// and certificate. `continuity` is the owner's current one, which the
    /// caller read with the material that re-validated this device's standing
    /// ([`Connect::material`] refuses a device no longer admitted, permitted
    /// or behind an unrevoked host); the link adopts it.
    #[must_use]
    pub fn pinned_to(&self, hint: &TransportHint, device: &DeviceCert, continuity: u64) -> bool {
        let pinned = hint == &self.hint && device == &self.device;
        if pinned {
            self.continuity
                .store(continuity, std::sync::atomic::Ordering::SeqCst);
        }
        pinned
    }
    /// The host this link reached.
    #[must_use]
    pub fn host(&self) -> &TransportHint {
        &self.via
    }
    /// Whether this link reached the workspace's governor itself.
    #[must_use]
    pub fn reaches_governor(&self) -> bool {
        self.via == self.hint
    }
    /// The owner continuity this link last validated against.
    fn continuity(&self) -> u64 {
        self.continuity.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn close(&self) {
        if let Ok(mut live) = self.live.lock() {
            *live = false;
        }
    }
    /// Compact the sealed record histories of this workspace that have used most
    /// of their bound into one checkpoint each, through `open` and `seal`,
    /// this device's channel keys, so the compactions publish with the
    /// device's own custody (see
    /// [`NativeWorkspaceOwner::compact_outgoing`]). Answers how many
    /// histories were compacted.
    pub fn compact(
        &self,
        identity: &HostIdentity,
        open: &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealRecord<'_>,
    ) -> Result<usize, String> {
        let (hint, device, continuity) = self.connect.material(identity)?;
        if !self.pinned_to(&hint, &device, continuity) {
            return Err("native compaction scope changed".into());
        }
        self.connect.owner.compact_outgoing(
            self.connect.workspace,
            &identity.device_signing_key,
            open,
            seal,
            std::time::SystemTime::now(),
        )
    }
    /// Reconcile the owner's outgoing custody and send, once per link, every
    /// entry it may send now. Sealed writes that expired in custody are not
    /// renewed: see [`Self::publish_renewing`].
    pub fn publish(&self, link: &DirectSync, identity: &HostIdentity) -> Result<(), String> {
        self.send(link, identity, None)
    }
    /// [`Self::publish`], first renewing this device's sealed writes that
    /// expired in custody through `reseal`, the channels' key holder (see
    /// [`NativeWorkspaceOwner::renew_outgoing`]).
    ///
    /// # Errors
    /// As [`Self::publish`].
    pub fn publish_renewing(
        &self,
        link: &DirectSync,
        identity: &HostIdentity,
        reseal: &mut Reseal<'_>,
    ) -> Result<(), String> {
        self.send(link, identity, Some(reseal))
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the live flag and cursor stay locked through publication so a close cannot interleave"
    )]
    fn send(
        &self,
        link: &DirectSync,
        identity: &HostIdentity,
        reseal: Option<&mut Reseal<'_>>,
    ) -> Result<(), String> {
        let live = self
            .live
            .lock()
            .map_err(|_| "native publication session poisoned")?;
        if !*live {
            return Err("native publication session closed".into());
        }
        let (hint, device, continuity) = self.connect.material(identity)?;
        if !self.pinned_to(&hint, &device, continuity) {
            return Err("native publication session changed".into());
        }
        // Retiring what the workspace refused or receipted persists a new
        // record generation. Its failure must not prevent sending what is
        // still in custody.
        if let Err(error) = self.connect.owner.retire_outgoing(self.connect.workspace) {
            tracing::warn!(workspace = %self.connect.workspace, %error, "native publication retirement not committed");
        }
        if let Some(reseal) = reseal
            && let Err(error) = self.connect.owner.renew_outgoing(
                self.connect.workspace,
                &identity.device_signing_key,
                reseal,
                std::time::SystemTime::now(),
            )
        {
            tracing::warn!(workspace = %self.connect.workspace, %error, "native sealed write renewal blocked");
        }
        self.connect.owner.with_outgoing(
            self.connect.workspace,
            std::time::SystemTime::now(),
            |view, outgoing| {
                if view.device() != &self.device
                    || view.transport_hint()? != &self.hint
                    || view.continuity() != self.continuity()
                {
                    return Err("native publication authority changed".into());
                }
                let (selected, _) = identity.current_native_material(view.workspace_id())?;
                if &selected != view.device() {
                    return Err("native publication custody changed".into());
                }
                view.peer()
                    .permissions(selected.device_public_key, std::time::SystemTime::now())
                    .map_err(|error| error.to_string())?;
                let mut cursor = self
                    .cursor
                    .lock()
                    .map_err(|_| "native publication cursor poisoned")?;
                cursor.drive(
                    outgoing.queued(),
                    outgoing.entries().map(|entry| {
                        (
                            entry.id(),
                            entry.bytes(),
                            entry.channel(),
                            entry.retention(),
                        )
                    }),
                    |bytes, _channel, _retention| {
                        let message: NeighbourMsg =
                            postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
                        link.send(message)
                    },
                )
            },
        )
    }
}

/// Another device's app write the owner took in for this device's apps.
#[derive(Clone)]
pub struct AppWrite {
    /// The writer's peer handle, as the write names it.
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// The author's standing in the workspace.
    pub author: tilo_action_policy::Standing,
    /// Whether the host replayed it from what it retains.
    pub replayed: bool,
    pub envelope: collab_types::SignedOp,
}

/// Whether the workspace took a frame in, as a device's apps read it: a write
/// it accepted or holds already, this device's own included.
#[must_use]
pub fn took_in(report: &crate::AdmissionReport, retained: bool) -> bool {
    match report.primary() {
        crate::InputDisposition::Accepted(_) | crate::InputDisposition::Duplicate(_) => true,
        // A host's replay of a frame this device took in before: its exact
        // evidence is gone (a legacy state, or one folded by a compaction),
        // so admission reports it as history it already holds. This device's
        // apps may be new — a restarted session keeps the workspace and
        // starts its apps empty — and the frame is part of their record: it
        // goes to them as any other replayed frame does, and their CRDT drops
        // what they already hold.
        crate::InputDisposition::RetryLater(
            crate::MissingPrerequisite::ExactHistoricalEvidence,
        ) => retained && report.duplicate,
        _ => false,
    }
}

/// The app write a frame carries, for this device's apps: `None` for anything
/// else, a write the owner neither accepted nor already held, this device's own
/// write, a write naming another workspace, or an author without standing.
#[must_use]
pub fn app_write(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
    device: [u8; 32],
    author: [u8; 32],
    msg: &NeighbourMsg,
    replayed: bool,
    taken_in: bool,
) -> Option<AppWrite> {
    if !taken_in || author == device {
        return None;
    }
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        record_id,
        envelope,
    } = msg
    else {
        return None;
    };
    if cluster_id != &workspace.to_string() {
        return None;
    }
    let standing = owner
        .with_current(workspace, |view| {
            view.peer()
                .permissions(author, std::time::SystemTime::now())
                .map(crate::MemberAuthority::standing)
                .map_err(|error| error.to_string())
        })
        .ok()?;
    Some(AppWrite {
        peer: *peer,
        app_id: app_id.clone(),
        cluster_id: cluster_id.clone(),
        record_id: record_id.clone(),
        author: standing,
        replayed,
        envelope: (**envelope).clone(),
    })
}

pub use crate::owner::hosted_by;

/// A frame a workspace's link brought in, as the session hands it on: the
/// link already admitted it into the workspace, so what is left is to see
/// what it did and give this device's apps what it carries.
#[derive(Clone)]
pub struct Input {
    scope: Arc<Scope>,
    heard: crate::direct::Heard,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeInput")
            .field("workspace", &self.scope.connect.workspace)
            .field("retained", &self.heard.retained)
            .finish_non_exhaustive()
    }
}

impl Input {
    /// The input a link's [`Heard`](crate::direct::Heard) frame is, for the
    /// workspace `scope` pins.
    #[must_use]
    pub fn heard(scope: Arc<Scope>, heard: crate::direct::Heard) -> Self {
        Self { scope, heard }
    }
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.scope.connect.workspace
    }
    #[must_use]
    pub fn author(&self) -> [u8; 32] {
        self.heard.author
    }
    #[must_use]
    pub fn message(&self) -> &NeighbourMsg {
        &self.heard.message
    }
    /// Whether it came as part of what the other device retains, rather than
    /// as live traffic.
    #[must_use]
    pub fn retained(&self) -> bool {
        self.heard.retained
    }
    /// Whether the workspace's state moved with it.
    #[must_use]
    pub fn landed(&self) -> bool {
        self.heard.landed
    }
    /// Observe the committed owner generation.
    pub fn generation(&self) -> Result<u64, String> {
        self.scope
            .connect
            .owner
            .with_current(self.workspace(), |view| Ok(view.generation()))
    }
    /// What the workspace's admission decided for this frame, as the link
    /// found it.
    pub fn admit(&self) -> Result<crate::AdmissionReport, String> {
        self.heard.report.clone()
    }
    /// Whether `other` came over the same link as this one: what
    /// [`Self::admit_batch`] keeps together.
    #[must_use]
    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.scope, &other.scope)
    }
    /// What taking `inputs` in, all of one link, decided: each with the
    /// report its admission gave, and whether the workspace's state moved.
    #[must_use]
    pub fn admit_batch(inputs: Vec<Self>) -> Admitted {
        let changed = inputs
            .first()
            .is_some_and(|first| inputs.iter().all(|input| first.same_session(input)))
            && inputs.iter().any(|input| input.heard.landed);
        Admitted {
            inputs: inputs
                .into_iter()
                .map(|input| {
                    let report = input.admit();
                    (input, report)
                })
                .collect(),
            changed,
        }
    }
    /// The app write `report` took in from another device, for this device's
    /// apps: `None` for anything else, a write the owner neither accepted nor
    /// already held, this device's own write, a write naming another
    /// workspace, or an author without standing.
    #[must_use]
    pub fn app_write(&self, report: &crate::AdmissionReport) -> Option<AppWrite> {
        app_write(
            &self.scope.connect.owner,
            self.workspace(),
            self.scope.device.device_public_key,
            self.heard.author,
            &self.heard.message,
            self.heard.retained,
            took_in(report, self.heard.retained),
        )
    }
    /// The author's current standing in the workspace: the workspace's policy, and
    /// the bound on a device not theirs.
    pub fn standing(&self) -> Result<tilo_action_policy::Standing, String> {
        self.scope
            .connect
            .owner
            .with_current(self.workspace(), |view| {
                view.peer()
                    .permissions(self.heard.author, std::time::SystemTime::now())
                    .map(crate::MemberAuthority::standing)
                    .map_err(|error| error.to_string())
            })
    }
}

/// What [`Input::admit_batch`] decided.
pub struct Admitted {
    /// Each input, in the order it came, with what its admission decided.
    pub inputs: Vec<(Input, Result<crate::AdmissionReport, String>)>,
    /// Whether the admission moved the workspace's state.
    pub changed: bool,
}

/// Whether one of `entries` is the revocation `cert`'s own root signed of the
/// device it names. A root speaks only for the keys it made: another person's
/// entry naming that device key refuses nothing of this certificate's.
fn its_root_revoked(entries: impl IntoIterator<Item = RevocationEntry>, cert: &DeviceCert) -> bool {
    entries.into_iter().any(|entry| {
        (entry.user_public_key, entry.revoked_device_public_key)
            == (cert.user_public_key, cert.device_public_key)
    })
}
