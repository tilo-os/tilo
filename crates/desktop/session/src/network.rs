//! What this session serves the network actor beside its own peer networks.
//!
//! The actor is the transport, and what some of its APIs tell is the session's business: a
//! member's device at another peer protocol version. That is served here by the part of the
//! session that owns it, installed once before the actor starts ([`install`]).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::platform::fabric::SharedLink;
use tilo_network_actor::services::PeerVersion;
use tilo_unlock_protocol::published::{
    DecisionOutcome, MAX_UNLOCK_DECISIONS, UnlockDecisionEntry, UnlockFabricState,
};
use tilo_unlock_protocol::{BootId, SlotId};
use tilo_workspace_service::workspaces::person_workspace::devices::{Device, Devices};

/// A member's device heard at another peer protocol version: one of the two devices must
/// update before the two work together again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionMismatch {
    /// Where the other device is reached: the only name the transport knows it by. The session
    /// names the device from its fabric ([`device_at`]).
    pub endpoint: String,
    /// The version it speaks.
    pub theirs: u32,
    /// The version this device speaks.
    pub ours: u32,
}

impl VersionMismatch {
    /// Whether the other device is the newer one, so this device is the one that updates.
    pub(crate) fn theirs_is_newer(&self) -> bool {
        self.theirs > self.ours
    }
}

/// Install the session's services for the actor started from now on: the word that a member's
/// device is at another protocol version.
pub(crate) fn install() -> std::sync::mpsc::Receiver<VersionMismatch> {
    let (updates, requests) = std::sync::mpsc::channel();
    let installed = tilo_network_actor::services::install(tilo_network_actor::services::Services {
        peer_version: peer_versions(updates),
    });
    if !installed {
        // The actor keeps the first session's services for the life of this process, so a second
        // session started in it is answered by that one's: this session's peer version notices
        // never reach the transport.
        tracing::warn!(
            "the network actor already serves an earlier session's services: this session's peer \
             version notices are not served"
        );
    }
    requests
}

/// Tell the session that a member's device is at another protocol version, once per device
/// and version: one of the two must update before they work together again. The session answers the
/// person, and a newer one on their side has it fetch the newest system ([`crate::update_finish`]).
fn peer_versions(updates: std::sync::mpsc::Sender<VersionMismatch>) -> PeerVersion {
    let told = Arc::new(Mutex::new(HashSet::new()));
    Arc::new(move |endpoint: String, theirs: u32| {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        if theirs == ours
            || !told
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((endpoint.clone(), theirs))
        {
            return;
        }
        tracing::info!(
            %endpoint,
            theirs,
            ours,
            "a member's device speaks another peer protocol version"
        );
        let mismatch = VersionMismatch {
            endpoint,
            theirs,
            ours,
        };
        if updates.send(mismatch).is_err() {
            // The session that would answer the person is gone.
            tracing::debug!("no session left to answer the person");
        }
    })
}

/// What the person calls the device reached at `endpoint` , when their fabric holds it: the label
/// that device says of itself. A workspace host this device never met on the fabric has none.
pub(crate) fn device_at(fabric: Option<&SharedLink>, endpoint: &str) -> Option<String> {
    let endpoint: iroh::EndpointId = endpoint.parse().ok()?;
    let fabric = fabric?;
    let device = fabric.device_behind(endpoint)?;
    let label = fabric.devices().get(&device)?.about.label.trim().to_owned();
    (!label.is_empty()).then_some(label)
}

/// What the device `key`, whose entry of the person's devices `kept` is, keeps about disk unlock:
/// its enrolments and auto-approvals, and the newest answers the person gave on it that the record
/// keeps. Nothing where that is not true of the device. The fabric lists it beside each device,
/// though the unlock exchange itself is the device daemon's.
pub(crate) fn kept_unlock(record: &Devices, kept: &Device, key: &[u8; 32]) -> UnlockFabricState {
    /// The fact under which a device keeps what it says of disk unlock.
    const UNLOCK: &str = "unlock";
    let device = hex::encode(key);
    let mut state = kept
        .facts
        .get(UNLOCK)
        .and_then(|value| crate::platform::fabric::decoded::<UnlockFabricState>(value))
        .unwrap_or_default();
    let mut answers = record
        .unlocks
        .values()
        .filter(|answer| answer.device == device)
        .filter_map(decision_of)
        .collect::<Vec<_>>();
    answers.sort_by_key(|answer| answer.decided_at_ns);
    let over = answers.len().saturating_sub(MAX_UNLOCK_DECISIONS);
    answers.drain(..over);
    state.decisions = answers;
    if state.validate(key).is_err() {
        return UnlockFabricState::default();
    }
    state
}

/// The answer the record keeps as `answer`; `None` for one no device of this build wrote.
fn decision_of(
    answer: &tilo_workspace_service::workspaces::person_workspace::devices::UnlockAnswer,
) -> Option<UnlockDecisionEntry> {
    let (volume, keyslot) = answer.slot.split_once(':')?;
    Some(UnlockDecisionEntry {
        boot_cert_digest: bytes(&answer.enrolment)?,
        boot_id: BootId(bytes(&answer.boot)?),
        slot: SlotId {
            volume_uuid: bytes(volume)?,
            keyslot: keyslot.parse().ok()?,
        },
        outcome: if answer.allowed {
            DecisionOutcome::Approve
        } else {
            DecisionOutcome::Deny
        },
        decided_by: bytes(&answer.device)?,
        decided_at_ns: u64::try_from(answer.at_ns).ok()?,
        valid_until_ns: u64::try_from(answer.until_ns).ok()?,
    })
}

/// The `N` bytes `text` holds in hex.
fn bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
    let mut out = [0; N];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    /// A peer at another version is reported once per endpoint and version: the person is told, a
    /// repeat says nothing new, and a peer moving to yet another version is news again.
    #[test]
    fn another_versions_peer_is_reported_once() {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        let ahead = SecretKey::generate().public().to_string();
        let (updates, heard) = std::sync::mpsc::channel();
        let report = peer_versions(updates);

        report(ahead.clone(), ours.wrapping_add(1));
        let mismatch = heard.try_recv().expect("the person is told");
        assert_eq!(mismatch.endpoint, ahead, "the transport names where it is");
        assert_eq!(mismatch.theirs, ours.wrapping_add(1));
        assert_eq!(mismatch.ours, ours);
        assert!(
            mismatch.theirs_is_newer(),
            "this device is the one to update"
        );

        report(ahead.clone(), ours.wrapping_add(1));
        report(ahead.clone(), ours);
        assert!(
            heard.try_recv().is_err(),
            "the same peer and version says nothing new, and this version is no mismatch"
        );

        report(ahead, ours.wrapping_add(2));
        assert!(heard.try_recv().is_ok(), "a further move is news again");
    }

    /// A device the fabric holds nothing of has no name here, so nothing is guessed from its
    /// endpoint.
    #[test]
    fn a_device_the_fabric_does_not_hold_has_no_name() {
        assert!(device_at(None, "not-an-endpoint").is_none());
        let endpoint = SecretKey::generate().public().to_string();
        assert!(device_at(None, &endpoint).is_none());
    }
}
