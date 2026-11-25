//! One link per enrolled workspace: dialling the host its enrollment pins,
//! holding the link ([`hold`]) over the direct connection, admitting what arrives
//! into this device's owner, and publishing what the owner holds.
mod hold;
mod reach;

pub use hold::{CannotDial, Holder, Link, hold};
pub use reach::{Linked, OwnHost, Reach};

use crate::direct::DirectSync;
use crate::join::TransportHint;
use crate::owner::{AppWrite, DeviceWorkspaces, NativeCommittedView, WorkspaceId, took_in};
use neighbour_wire::NeighbourMsg;
use std::sync::Arc;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// An enrolled workspace to hold a link for.
#[derive(Clone)]
pub struct Connect {
    owner: DeviceWorkspaces,
    workspace: WorkspaceId,
}
impl std::fmt::Debug for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NativeConnect")
            .field(&self.workspace)
            .finish()
    }
}

impl DeviceWorkspaces {
    /// Where `workspace`'s host is reached, with the device its enrollment
    /// pins: the address a live session of the workspace dials that host at,
    /// decoded from the hint the host signed. `None` while the workspace is
    /// not held here or its enrollment names no iroh address.
    #[must_use]
    pub fn host_addr(&self, workspace: WorkspaceId) -> Option<([u8; 32], iroh::EndpointAddr)> {
        let hint = self
            .with_current(workspace, |view| Ok(view.transport_hint()?.clone()))
            .ok()?;
        let (payload, _) =
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
                &hint,
            )
            .ok()?;
        Some((hint.host.writes_for(), payload.endpoint_addr))
    }
}

impl Connect {
    /// # Errors
    /// `workspace` has no committed enrollment.
    pub fn new(owner: DeviceWorkspaces, workspace: WorkspaceId) -> Result<Self, String> {
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
        identity: &DeviceIdentity,
    ) -> Result<(TransportHint, DeviceCert, u64), String> {
        let (cert, continuity) = self.ownership(identity)?;
        let hint = self
            .owner
            .with_current(self.workspace, |view| Ok(view.transport_hint()?.clone()))?;
        Ok((hint, cert, continuity))
    }
    /// Whether this device may act as `workspace`'s own member now, and the
    /// continuity its record is at ([`Self::material`]).
    pub fn ownership(&self, identity: &DeviceIdentity) -> Result<(DeviceCert, u64), String> {
        self.owner.with_current(self.workspace, |view| {
            let (cert, entries) = identity.current_material(view.workspace_id())?;
            if &cert != view.device() {
                return Err("native ownership differs from enrollment".into());
            }
            view.peer()
                .permissions(cert.device_public_key, tilo_clock::wall())
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
    /// Every holder of the workspace a link may reach, each alike: the host
    /// this device's enrollment names, then each holder the workspace's
    /// governance names ([`crate::keeping_devices_of`]) in the order it holds
    /// them. A holder the workspace revoked, or another naming this device,
    /// is left out.
    pub fn holders(&self, own: &[u8; 32]) -> Result<Vec<TransportHint>, String> {
        self.owner.with_current(self.workspace, |view| {
            let enrolled = view.transport_hint()?.clone();
            let revocations = view.peer().revocations();
            let named = view
                .peer()
                .governance()
                .keeping_devices()
                .into_iter()
                .filter(|hint| {
                    hint.host.device_public_key != enrolled.host.device_public_key
                        && !crate::owner::hosted_by(hint, own)
                        && !its_root_revoked(revocations.iter().cloned(), &hint.host)
                });
            Ok(std::iter::once(enrolled.clone()).chain(named).collect())
        })
    }

    /// Whether this device is the workspace's host itself: the host its
    /// enrollment pins is this very device. Such a workspace is never dialled:
    /// its link pairs with the host in this process.
    #[must_use]
    pub fn hosted_here(&self, identity: &DeviceIdentity) -> bool {
        self.owner
            .with_current(self.workspace, |view| {
                let (cert, _) = identity.current_material(view.workspace_id())?;
                Ok(view
                    .transport_hint()?
                    .host
                    .is_of_device(&cert.device_public_key))
            })
            .unwrap_or(false)
    }
    /// The renewal of the certificate the workspace's record holds, when this
    /// device's ownership holds one: the device proves its membership with it
    /// ([`crate::owner::enroll::renew_membership`]) before its links dial again,
    /// as [`Self::material`] refuses ownership that differs from the record.
    pub fn renewal(&self, identity: &DeviceIdentity) -> Result<Option<DeviceCert>, String> {
        self.owner.with_current(self.workspace, |view| {
            let (cert, _) = identity.current_material(view.workspace_id())?;
            Ok(crate::owner::renewal::renews(&cert, view.device()).then_some(cert))
        })
    }
    /// This device's end of the workspace's link, syncing the replica its
    /// owner decides on: what a link brings is durable as it lands.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn link(
        &self,
        signer: impl collab_types::signer::DeviceSigner + 'static,
    ) -> Result<DirectSync, String> {
        DirectSync::over(self.owner.replica(self.workspace)?, signer)
            .owning(self.owner.clone(), self.workspace)
    }
    /// The owner of the workspace's record.
    #[must_use]
    pub fn owner(&self) -> &DeviceWorkspaces {
        &self.owner
    }
}

/// A live link's pin; closed when the link ends.
///
/// The pin is the host it reached and the certificate it signs as. The owner's
/// continuity moves with any security change of the workspace, another member
/// joining included; the link follows it in place once this device's own
/// member access was checked again ([`Connect::material`]), rather than ending and
/// redialling every member for one person's join.
pub struct Scope {
    connect: Connect,
    /// The host this device's enrollment names.
    hint: TransportHint,
    /// The holder the link reached: any of the workspace's holders
    /// ([`Connect::hosts`]).
    via: TransportHint,
    device: DeviceCert,
    /// The owner continuity the link last validated against.
    continuity: std::sync::atomic::AtomicU64,
}
impl Scope {
    /// A link of the enrollment `hint` names that reached the holder `via`.
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
        }
    }
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.connect.workspace
    }
    /// Whether this link is still pinned to this material: the same host
    /// and certificate. `continuity` is the owner's current one, which the
    /// caller read with the material that re-validated this device's access
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
    /// The owner continuity this link last validated against.
    fn continuity(&self) -> u64 {
        self.continuity.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Whether this device may still publish over this link: the workspace's current material
    /// is the one the link is pinned to ([`Connect::material`]).
    ///
    /// # Errors
    /// This device's access in the workspace, or the link's pin, changed.
    pub fn current(&self, identity: &DeviceIdentity) -> Result<(), String> {
        let (hint, device, continuity) = self.connect.material(identity)?;
        if self.pinned_to(&hint, &device, continuity) {
            Ok(())
        } else {
            Err("native publication session changed".into())
        }
    }
    /// Whether the ownership `view` holds still publishes over this link, as it stands in the
    /// owner's turn: the same certificate, host and continuity, the certificate this device
    /// selects for the workspace, and its permission to write.
    ///
    /// # Errors
    /// What changed since the link was pinned.
    pub fn authorize(
        &self,
        view: &NativeCommittedView,
        identity: &DeviceIdentity,
    ) -> Result<(), String> {
        if view.device() != &self.device
            || view.transport_hint()? != &self.hint
            || view.continuity() != self.continuity()
        {
            return Err("native publication authority changed".into());
        }
        let (selected, _) = identity.current_material(view.workspace_id())?;
        if &selected != view.device() {
            return Err("native publication ownership changed".into());
        }
        view.peer()
            .permissions(selected.device_public_key, tilo_clock::wall())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// The app write a frame carries, for this device's apps: `None` for anything
/// else, a write the owner neither accepted nor already held, this device's own
/// write, a write naming another workspace, or an author without member access.
#[must_use]
pub fn app_write(
    owner: &DeviceWorkspaces,
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
        document_id,
        envelope,
        readers: _,
    } = msg
    else {
        return None;
    };
    if cluster_id != &workspace.to_string() {
        return None;
    }
    // What its author may do, as the write was taken: what the device may
    // now, or, for a device its person removed since, what its person may
    // when the cut of the removal keeps the write as history.
    let access = owner
        .with_current(workspace, |view| {
            view.peer()
                .permissions_as_written(app_id, document_id, envelope, tilo_clock::wall())
                .map_err(|error| error.to_string())
        })
        .ok()?;
    Some(AppWrite {
        peer: *peer,
        app_id: app_id.clone(),
        cluster_id: cluster_id.clone(),
        document_id: document_id.clone(),
        author: access,
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
    /// found it. An [`crate::AdmissionError::Device`] is this device failing to
    /// decide it, never a refusal of the frame.
    pub fn admit(&self) -> Result<crate::AdmissionReport, crate::AdmissionError> {
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
    /// workspace, or an author without member access.
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
    /// Every app write `report` took in from other devices, for this device's
    /// apps, in the order the workspace took them: the frame's own
    /// ([`Self::app_write`]), then each held write it brought what it waited
    /// for ([`crate::AdmissionReport::released`]). A held write reaches the
    /// apps only this way: when it came, nothing of it was taken in.
    #[must_use]
    pub fn app_writes(&self, report: &crate::AdmissionReport) -> Vec<AppWrite> {
        let own = self.scope.device.device_public_key;
        self.app_write(report)
            .into_iter()
            .chain(report.released().iter().filter_map(|released| {
                app_write(
                    &self.scope.connect.owner,
                    self.workspace(),
                    own,
                    released.author,
                    &released.message,
                    released.retained,
                    true,
                )
            }))
            .collect()
    }
    /// The author's current member access in the workspace: the workspace's policy, and
    /// the bound on a device not theirs.
    pub fn access(&self) -> Result<tilo_action_policy::MemberAccess, String> {
        self.scope
            .connect
            .owner
            .with_current(self.workspace(), |view| {
                view.peer()
                    .permissions(self.heard.author, tilo_clock::wall())
                    .map_err(|error| error.to_string())
            })
    }
}

/// What [`Input::admit_batch`] decided.
pub struct Admitted {
    /// Each input, in the order it came, with what its admission decided.
    pub inputs: Vec<(Input, Result<crate::AdmissionReport, crate::AdmissionError>)>,
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
