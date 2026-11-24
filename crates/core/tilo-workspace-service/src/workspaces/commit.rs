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
    Ok(())
}

/// What `candidate` publishes for the workspace's records, as its changes
/// since the governor's copy in `peer`: the messages a commit sends on, and
/// the records one that starts the workspace over must still publish.
pub(super) fn durable_messages(
    candidate: &StatePeer,
    peer: &StatePeer,
    (_key, _now): (&SigningKey, SystemTime),
    _generation: u64,
) -> Vec<neighbour_wire::NeighbourMsg> {
    // What the commit changed is the workspace change every device admits, and
    // only that: the changes past what `peer` held. Sending the whole document
    // at each commit would publish its history again every time, and the
    // publication would grow with the square of the changes. A record's own
    // writes reach its readers as its device sends them.
    let Some(heads) = peer.workspace_heads() else {
        return vec![candidate.workspace_message()];
    };
    let changes = candidate.workspace_changes_since(&heads);
    if changes.is_empty() {
        return Vec::new();
    }
    vec![neighbour_wire::NeighbourMsg::SyncResp {
        changes,
        workspace: candidate.workspace_id().to_owned(),
    }]
}

/// The records `candidate`, a change built from `peer`, changes: what its
/// durable messages publish of a record. A base the workspace starts over
/// from publishes these again, and keeps every other record's checkpoint.
pub(super) fn changed_records(
    candidate: &StatePeer,
    peer: &StatePeer,
    signing: (&SigningKey, SystemTime),
    generation: u64,
) -> std::collections::BTreeSet<(String, String)> {
    durable_messages(candidate, peer, signing, generation)
        .into_iter()
        .filter_map(|message| match message {
            neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            } => Some((app_id, document_id)),
            _ => None,
        })
        .collect()
}

/// What a commit that starts the workspace over from a base publishes in
/// place of everything published before it. A device of a person who governs
/// the workspace's history founds its records: it publishes the base and its
/// checkpoint of each record in `changed`, the records the commit changed,
/// while every other record keeps its checkpoint on its own channel, since
/// replacing that baseline could discard members' writes. Any other holder
/// adopting the base its maker made founds no record, and its checkpoint of
/// one would be refused as a second start of it. It publishes the base as it
/// took it in, and after it, as its own writes, what the commit changed of
/// the records.
pub(super) fn base_publications(
    candidate: &mut StatePeer,
    replica: &StatePeer,
    changed: &std::collections::BTreeSet<(String, String)>,
    (key, now): (&SigningKey, SystemTime),
    generation: u64,
) -> Result<Vec<Publication>, String> {
    if !candidate.signed_by_governor(&key.verifying_key().to_bytes()) {
        let writes = durable_messages(candidate, replica, (key, now), generation);
        let mut outgoing = vec![publication(&candidate.workspace_message(), true)?];
        for write in writes
            .iter()
            .filter(|message| matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }))
        {
            outgoing.push(publication(write, false)?);
        }
        return Ok(outgoing);
    }
    candidate
        .publication_mut()
        .bootstrap_messages(key, now)?
        .iter()
        .filter(|message| match message {
            neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            } => changed.contains(&(app_id.clone(), document_id.clone())),
            _ => true,
        })
        .map(|message| publication(message, true))
        .collect()
}

/// Fold `record` of `app`, as a write in `candidate` left it, into the
/// governor's checkpoint of it.
pub(super) fn fold_into_checkpoint(
    candidate: &mut StatePeer,
    (app, record): (&str, &str),
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    let written = candidate
        .document(app, record)
        .ok_or("created record missing")?;
    candidate
        .publication_mut()
        .publish_document(app, record, written, key, now)
        .map(drop)
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
