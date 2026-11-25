//! How a workspace this device holds stands for its person
//! ([`tilo_peer_state::workspace_view::WorkspaceReading`]), read off the replica it keeps: what the
//! device's daemon publishes to its person's shell.

use std::collections::{BTreeMap, BTreeSet};

use collab_types::signer::DeviceSigner;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::StatePeer;
use tilo_peer_state::join::Role;
use tilo_peer_state::workspace_view::{Holder, Hosting, NotDeletable, WorkspaceReading};

/// Who a reading is for: the device's person, the device, and what signs as it.
pub struct Reader<'a> {
    pub person: [u8; 32],
    pub device: [u8; 32],
    pub signer: &'a dyn DeviceSigner,
}

/// `workspace` as `peer` holds it, read for `reader`. `holder` is the device keeping it, as this
/// device asks it, and `open_links` when its links out lapse.
#[must_use]
pub fn reading(
    workspace: WorkspaceId,
    peer: &StatePeer,
    reader: &Reader<'_>,
    holder: Option<[u8; 32]>,
    open_links: Vec<u64>,
) -> WorkspaceReading {
    let now = tilo_clock::wall();
    let membership = peer.governance();
    let person = hex::encode(reader.person);
    let members = membership.governed_members(now).unwrap_or_default();
    let owner = Some(membership.owner_user_public_key())
        .filter(|owner| *owner != [0; 32])
        .map(hex::encode);
    let labels: BTreeMap<[u8; 32], String> = peer
        .certificates()
        .into_iter()
        .map(|certificate| (certificate.device_public_key, certificate.device_label))
        .collect();
    let admitted: BTreeSet<[u8; 32]> = membership
        .governed_access(now)
        .map(|access| access.member_devices.into_iter().collect())
        .unwrap_or_default();
    let roster_devices: Vec<[u8; 32]> = labels
        .keys()
        .filter(|device| admitted.contains(*device))
        .copied()
        .collect();
    let holders = holders(peer);
    let people = members.members.clone();
    let alone = people
        .iter()
        .all(|member| member.removed || member.id.person_id() == Some(person.as_str()));
    let is_owner = owner.as_deref() == Some(person.as_str());
    let not_deletable = if !alone {
        Some(NotDeletable::OthersInIt)
    } else if roster_devices != [reader.device] {
        Some(NotDeletable::OtherDevice)
    } else if !is_owner {
        Some(NotDeletable::NotOwner)
    } else {
        None
    };
    let reads_whole = is_owner
        || people
            .iter()
            .any(|member| !member.removed && member.id.person_id() == Some(person.as_str()));
    let may_invite = membership.may_invite(reader.signer, now);
    WorkspaceReading {
        id: workspace.to_string(),
        name: membership
            .workspace_name()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        owner,
        roster_devices,
        people,
        members,
        policy: membership.governed_policy().ok(),
        administers: peer
            .workspace()
            .check_workspace_administration(reader.device, now)
            .is_ok(),
        may_invite,
        invite_refused: !may_invite,
        may_invite_administrator: membership.may_invite_administrator(reader.signer, now),
        invitable_roles: Role::ASSIGNABLE
            .into_iter()
            .filter(|role| membership.may_invite_role(reader.signer, *role, now))
            .collect(),
        not_deletable,
        reads_whole,
        hosting: Some(Hosting {
            host: holder
                .and_then(|holder| labels.get(&holder).cloned())
                .unwrap_or_default(),
            here: holder == Some(reader.device),
            // A device keeping the workspace for its own person is let in by that naming, not as
            // a member's device, which the member list leaves it out of: it waits for no host.
            admitted: admitted.contains(&reader.device)
                || membership.keeping_devices().iter().any(|keeping| {
                    keeping.host.device_public_key == reader.device
                        && keeping.host.user_public_key == reader.person
                }),
        }),
        holders,
        holder,
        device_labels: labels,
        write_refusals: peer.workspace().write_refusals(),
        quarantined: peer.quarantined(),
        open_links,
        document: peer.workspace().document(),
        shortcuts: peer.workspace().workspace_shortcuts(),
        sandbox: membership.governed_sandbox().ok(),
    }
}

/// The devices keeping the workspace `peer` holds, by the label each signed, as its membership
/// names them.
fn holders(peer: &StatePeer) -> Vec<Holder> {
    let mut holders: Vec<Holder> = peer
        .governance()
        .keeping_devices()
        .into_iter()
        .filter_map(|hint| {
            let payload =
                tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
                    &hint,
                )
                .ok()?;
            let label = payload.label.trim();
            Some(Holder {
                device: hex::encode(hint.host.device_public_key),
                label: if label.is_empty() {
                    hint.host.device_label
                } else {
                    label.to_owned()
                },
                reads: payload.holds_keys,
                cloud: payload.cloud,
            })
        })
        .collect();
    holders.sort_by(|a, b| a.label.cmp(&b.label));
    holders
}

/// A device keeping the workspace `peer` holds other than `own`, and where it is reached: the
/// address that device signed into the workspace's membership. What a device asks the workspace's
/// key access and membership of when it keeps none of it itself.
#[must_use]
pub fn keeping_device_elsewhere(
    peer: &StatePeer,
    own: &[u8; 32],
) -> Option<([u8; 32], iroh::EndpointAddr)> {
    peer.governance()
        .keeping_devices()
        .into_iter()
        .filter(|hint| !hint.host.is_of_device(own))
        .find_map(|hint| {
            let (payload, _) =
                tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
                    &hint,
                )
                .ok()?;
            Some((hint.host.writes_for(), payload.endpoint_addr))
        })
}

/// The device keeping the workspace `peer` holds that runs its work, when that is another device
/// than `own`: a home server or a cloud host, which decides its records' actions with the
/// workspace's policy and runs them in its own workers. A workspace only desktops keep runs its
/// records' actions where its windows are.
#[must_use]
pub fn host_elsewhere(peer: &StatePeer, own: &[u8; 32]) -> Option<HostElsewhere> {
    peer.governance()
        .keeping_devices()
        .into_iter()
        .find_map(|hint| HostElsewhere::of(hint, own))
}

/// A workspace's host other than this device, as its signed address names it
/// ([`host_elsewhere`]).
#[derive(Clone, Debug)]
pub struct HostElsewhere {
    /// The address the host signed into the workspace's membership.
    pub hint: tilo_peer_state::join::TransportHint,
    /// The host's device key.
    pub device: [u8; 32],
    /// Where the host is reached.
    pub address: iroh::EndpointAddr,
}

impl HostElsewhere {
    /// The host `hint` names, when it is signed by the device it names, runs the workspace's
    /// work, and is another device than `own`.
    #[must_use]
    pub fn of(hint: tilo_peer_state::join::TransportHint, own: &[u8; 32]) -> Option<Self> {
        let (payload, _) =
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
                &hint,
            )
            .ok()?;
        let device = payload.device_certificate.device_public_key;
        (payload.runs_work && device != *own).then_some(Self {
            hint,
            device,
            address: payload.endpoint_addr,
        })
    }

    /// [`Self::of`] the signed address `written` names (postcard, in hex), as the person's
    /// workspace list carries it.
    #[must_use]
    pub fn read(written: &str, own: &[u8; 32]) -> Option<Self> {
        let bytes = hex::decode(written).ok()?;
        Self::of(postcard::from_bytes(&bytes).ok()?, own)
    }

    /// The signed address, as the person's workspace list carries it: postcard, in hex.
    #[must_use]
    pub fn written(&self) -> Option<String> {
        postcard::to_allocvec(&self.hint).ok().map(hex::encode)
    }
}

/// `part` of the workspace `peer` holds, read for the device `device` and encoded as the shell
/// reads it ([`tilo_peer_state::workspace_view::Part`]).
///
/// # Errors
/// The part does not encode.
pub fn part(
    peer: &StatePeer,
    device: [u8; 32],
    part: tilo_peer_state::workspace_view::Part,
) -> Result<Vec<u8>, serde_json::Error> {
    use tilo_peer_state::workspace_view::{CallGateReading, Part};
    let now = tilo_clock::wall();
    match part {
        Part::Revocations => serde_json::to_vec(&peer.revocations()),
        Part::Readable => {
            let mut readable = crate::documents::listing(peer);
            readable.retain(|(app, document)| {
                peer.check_document_read(device, app, document, now).is_ok()
            });
            serde_json::to_vec(&readable)
        }
        Part::CallGate => serde_json::to_vec(&CallGateReading::from(
            peer.governance().call_gate(now).unwrap_or_default(),
        )),
        Part::Keeping => serde_json::to_vec(&peer.governance().keeping_devices()),
    }
}
