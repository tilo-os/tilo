//! What a commit publishes: the governor's copy prepared for it, and the
//! messages it sends to the workspace's members.

use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use tilo_peer_state::StatePeer;
use web_time::SystemTime;

use super::Publication;

/// Keep exact admission evidence in the governor's copy of the workspace. A
/// member of a workspace whose channels are sealed writes its records sealed,
/// and the governor follows those writes into its copy: with exact evidence
/// it keeps each one as the record's ciphertext history and acknowledges
/// it in its next governance change. Without it a sealed write only marks
/// the record encrypted, and every later change of the workspace (letting the
/// next person in among them) is refused as incomplete encryption state.
/// A workspace committed before this selection takes it on its next commit.
///
/// The copy also writes under the host's own actor
/// ([`tilo_peer_state::workspace_host_actor`]): a device hosting its own workspaces
/// never writes one workspace from two documents under one actor.
pub(super) fn select_exact_admission(peer: &mut StatePeer) -> Result<(), String> {
    peer.write_as_workspace_host();
    peer.select_exact_admissions()
        .map_err(|error| format!("workspace exceeds exact admission bounds: {error}"))
}

pub(super) fn prepare_governance(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    peer.governance_mut()
        .reconcile_administrator_grants(key, now)?;
    peer.governance_mut().drop_names_of_nobody(key, now)?;
    peer.media_mut().reconcile_media_governance(key, now)?;
    peer.media().validate_media_governance_for_publication()
}

/// What `candidate` publishes for the workspace's records, as its changes
/// since the governor's copy in `peer`: the messages a commit sends on, and
/// the records one that starts the workspace over must still publish.
pub(super) fn durable_messages(
    candidate: &mut StatePeer,
    peer: &StatePeer,
    (key, now): (&SigningKey, SystemTime),
    generation: u64,
) -> Result<Vec<neighbour_wire::NeighbourMsg>, String> {
    candidate
        .publication_mut()
        .durable_mutation_messages(peer, key, now, generation)
}

/// Whether the workspace publishes `message` of a commit to its members. The
/// governor's compaction of a record's sealed history bounds its own copy:
/// every member already holds each write it covers, and the server retains
/// those writes for whoever joins later, so sending it on would push the
/// whole record to every member each time the history fills, for nothing.
/// It stays in the governor's durable copy alone.
pub(super) fn for_members(message: &neighbour_wire::NeighbourMsg) -> bool {
    !matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
            if envelope.kind == collab_types::OpKind::Compaction
    )
}

pub(super) fn publication(
    message: &neighbour_wire::NeighbourMsg,
    checkpoint: bool,
) -> Result<Publication, String> {
    let (_, kind) = neighbour_wire::retention(message, checkpoint)
        .ok_or("workspace governance message has no retention channel")?;
    Ok(Publication {
        bytes: postcard::to_stdvec(message)
            .map_err(|error| error.to_string())?
            .into(),
        checkpoint: kind == Retention::Checkpoint,
    })
}
