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

use iroh::endpoint::Connection;
use tilo_device_fabric::DeviceId;
use tilo_device_fabric::link::SharedLink;
use tilo_network_actor::services::{self, ApproversOf, DeviceUnlock, PeerVersion, Services};
use tilo_service::Questions;

mod approvers;
mod unlock;

pub(crate) use approvers::{AgentQuestion, KIND as AGENT_QUESTION};
pub(crate) use unlock::{KIND as DEVICE_UNLOCK, UnlockQuestion};

/// Install the session's services for the actor started from now on: the
/// person's other devices' disk unlock ([`unlock`]), the questions their
/// agents ask ([`approvers`]), and the word that a collaborator's device is
/// at another protocol version.
///
/// A session that is no certified device of the person's offers no door: it
/// has no key to answer as.
pub(crate) fn install(
    link: Option<&Arc<crate::fabric::SessionFabric>>,
    questions: Option<Questions>,
) -> std::sync::mpsc::Receiver<()> {
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

/// A fresh actor's approver sessions ask the person on `questions`.
fn approvers(questions: Option<Questions>) -> ApproversOf {
    Arc::new(move || {
        Box::new(approvers::Answering::new(questions.clone())) as Box<dyn services::Approvers>
    })
}

/// Tell the person that a collaborator's device is at another protocol
/// version, once per device and version: one of the two must update before
/// they work together again. A newer one on their side has this session fetch
/// the newest system ([`crate::update_finish`]).
fn peer_versions(updates: std::sync::mpsc::Sender<()>) -> PeerVersion {
    let told = Arc::new(Mutex::new(HashSet::new()));
    Arc::new(move |endpoint: String, device_label: String, theirs: u32| {
        let ours = neighbour_wire::PEER_PROTOCOL_VERSION;
        if theirs == ours
            || !told
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((endpoint.clone(), theirs))
        {
            return;
        }
        let device = if device_label.is_empty() {
            endpoint.as_str()
        } else {
            device_label.as_str()
        };
        tracing::info!(
            endpoint,
            device,
            theirs,
            ours,
            "a collaborator's device speaks another peer protocol version"
        );
        if theirs > ours && updates.send(()).is_err() {
            // The session that would update this device is gone.
            tracing::debug!(device, "no session left to update this device");
        }
    })
}

/// The key `device` names, as the bytes the fleet signs and lists.
pub(crate) fn key_of(device: &DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str(), &mut key).ok()?;
    Some(key)
}
