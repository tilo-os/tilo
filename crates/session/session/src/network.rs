//! What this session serves the network actor beside its own planes.
//!
//! The actor is the transport, and what some of its doors ask is the
//! session's business: another of the person's devices asking for its
//! disk-unlock half, an agent's question for its person on a workspace host,
//! a collaborator's device at another peer protocol version. Each is served
//! here by the part of the session that owns it, installed once before the
//! actor starts ([`install`]).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::platform::fabric::DeviceId;
use crate::platform::fabric::SharedLink;
use iroh::endpoint::Connection;
use tilo_network_actor::services::{self, ApproversOf, DeviceUnlock, PeerVersion, Services};
use tilo_service::Questions;

mod approvers;
mod unlock;

pub(crate) use approvers::KIND as AGENT_QUESTION;
pub(crate) use unlock::{KIND as DEVICE_UNLOCK, UnlockQuestion, slot_name};

/// A collaborator's device heard at another peer protocol version: one of the two devices must
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

/// Install the session's services for the actor started from now on: the person's other devices'
/// disk unlock ([`unlock`]), the questions their agents ask ([`approvers`]), and the word that a
/// collaborator's device is at another protocol version.
/// A session that is no certified device of the person's offers no door: it has no key to answer
/// as.
pub(crate) fn install(
    link: Option<&Arc<crate::platform::fabric::SessionFabric>>,
    questions: Option<Questions>,
) -> std::sync::mpsc::Receiver<VersionMismatch> {
    let (updates, requests) = std::sync::mpsc::channel();
    let Some(link) = link.map(|link| Arc::clone(link) as SharedLink) else {
        return requests;
    };
    let Some(local) = link
        .local_device_fabric_identity()
        .and_then(|(device, _)| key_of(&device))
    else {
        return requests;
    };
    let unlocking = Arc::new(unlock::Unlock::new(link, local, questions.clone()));
    let installed = services::install(Services {
        peer_version: peer_versions(updates),
        device_unlock: device_unlock(unlocking.clone()),
        approvers: approvers(questions),
    });
    if installed {
        unlocking.spawn_enrol_socket();
    }
    requests
}

/// Another of the person's devices asks for its unlock half at early boot.
fn device_unlock(unlocking: Arc<unlock::Unlock>) -> DeviceUnlock {
    Arc::new(move |connection: Connection| {
        let unlocking = Arc::clone(&unlocking);
        tokio::spawn(unlocking.serve(connection));
    })
}

/// A fresh actor's approver sessions ask the person on `questions` .
fn approvers(questions: Option<Questions>) -> ApproversOf {
    Arc::new(move || {
        Box::new(approvers::Answering::new(questions.clone())) as Box<dyn services::Approvers>
    })
}

/// Tell the session that a collaborator's device is at another protocol version, once per device
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
            "a collaborator's device speaks another peer protocol version"
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
    let label = fabric
        .device_fabric()
        .snapshots()
        .get(&device)?
        .presence
        .label
        .trim()
        .to_owned();
    (!label.is_empty()).then_some(label)
}

/// The key `device` names, as the bytes the fleet signs and lists.
pub(crate) fn key_of(device: &DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str(), &mut key).ok()?;
    Some(key)
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
