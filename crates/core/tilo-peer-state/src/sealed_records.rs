//! Records written and read the way a workspace's members do, sealed under
//! the keys every member holds: what tests write through the sync engine,
//! since a workspace refuses a write in the clear.
#![allow(
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a failed step is the test failing"
)]

use crate::StatePeer;
use crate::direct::DirectSync;
use automerge::AutoCommit;
use collab_types::{ContentRetention, SignedOp, document_artifact_id};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;

/// Fresh keys every member of `workspace` seals its records under.
pub fn members_keys(workspace: &str) -> key_group::ContentKeys {
    key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(workspace).unwrap(),
        key_group::MemberKey::generate("member").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

/// `record` of `app` as a holder of `keys` reads it: every sealed write
/// merged over its base. `None` while `peer` holds none of it.
pub fn opened(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    (app, record): (&str, &str),
) -> Option<Vec<u8>> {
    let mut open = |op: &SignedOp| {
        let metadata = op.encryption.ok_or("not sealed")?;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    };
    peer.publication()
        .opened_document(app, record, &mut open)
        .unwrap()
        .map(|opened| opened.document)
}

/// The heads of `record` of `app` as a holder of `keys` reads it.
pub fn opened_heads(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    route: (&str, &str),
) -> Option<Vec<automerge::ChangeHash>> {
    opened(peer, keys, route).map(|bytes| AutoCommit::load(&bytes).unwrap().get_heads())
}

/// Write `record` of `app` on `sync` as `key`'s device, sealed under `keys`
/// ([`sealed_write`]), and publish it.
pub fn write_sealed(
    sync: &DirectSync,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    route: (&str, &str),
    edit: impl FnOnce(&mut AutoCommit),
) -> NeighbourMsg {
    sync.publish(|peer| sealed_write(peer, keys, key, route, edit))
        .unwrap()
}

/// Write `record` of `app` on `peer` as `key`'s device, sealed under `keys`:
/// the whole record `edit` makes of nothing when `peer` holds none of it, the
/// change `edit` makes of it otherwise. What `edit` leaves uncommitted is
/// committed as a native edit.
///
/// # Errors
/// The write's refusal.
pub fn sealed_write(
    peer: &mut StatePeer,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    (app, record): (&str, &str),
    edit: impl FnOnce(&mut AutoCommit),
) -> Result<NeighbourMsg, String> {
    let held = opened(peer, keys, (app, record));
    let starts = held.is_none();
    let mut document = held
        .map_or_else(AutoCommit::new, |base| AutoCommit::load(&base).unwrap())
        .with_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
    let heads = document.get_heads();
    edit(&mut document);
    // An edit `edit` left uncommitted is a native one, named as such: every
    // change names who made it.
    document.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Native.change_message()),
    );
    if starts {
        return sealed_checkpoint(peer, keys, key, (app, record), document.save());
    }
    sealed_changes(peer, keys, key, (app, record), document.save_after(&heads))
}

/// Write `record` of `app` whole, as `document`, on `peer` as `key`'s device,
/// sealed under `keys`.
///
/// # Errors
/// The write's refusal.
pub fn sealed_checkpoint(
    peer: &mut StatePeer,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    (app, record): (&str, &str),
    document: Vec<u8>,
) -> Result<NeighbourMsg, String> {
    let artifact = document_artifact_id(app, peer.workspace_id(), record);
    peer.publication_mut().publish_sealed_checkpoint(
        app,
        record,
        document,
        &mut |plaintext| seal(keys, artifact, plaintext),
        key,
        tilo_clock::wall(),
    )
}

/// Write `changes` to `record` of `app` on `peer` as `key`'s device, sealed
/// under `keys`.
///
/// # Errors
/// The write's refusal.
pub fn sealed_changes(
    peer: &mut StatePeer,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    (app, record): (&str, &str),
    changes: Vec<u8>,
) -> Result<NeighbourMsg, String> {
    let artifact = document_artifact_id(app, peer.workspace_id(), record);
    peer.publication_mut().publish_sealed_incremental(
        app,
        record,
        changes,
        &mut |plaintext| seal(keys, artifact, plaintext),
        key,
        tilo_clock::wall(),
    )
}

fn seal(
    keys: &key_group::ContentKeys,
    artifact: collab_types::ArtifactId,
    plaintext: &[u8],
) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
    keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
        .map_err(|error| error.to_string())
}
