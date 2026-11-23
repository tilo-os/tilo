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
    let roster = roster_devices
        .iter()
        .filter_map(|device| {
            tilo_failure::ignored!(
                "a key no endpoint can have is no device the roster names",
                iroh::EndpointId::from_bytes(device).ok()
            )
        })
        .map(|device| device.to_string())
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
        roster,
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
            admitted: admitted.contains(&reader.device),
        }),
        holders,
        holder,
        device_labels: labels,
        write_refusals: peer.workspace().write_refusals(),
        open_links,
        document: peer.workspace().document(),
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
