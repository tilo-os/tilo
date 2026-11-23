//! Records a holder of the workspace writes itself, outside any app's factory: the members'
//! shared desktop, a person's servers in their admin workspace. Every holder writes them,
//! whether or not it runs agents, so this is not part of `applications`.

use super::{CollaborationError, Workspaces, system_time};
use tilo_user_identity::DeviceCert;

impl Workspaces {
    /// Author a workspace record no app factory makes, such as the shared
    /// desktop members' pages keep, as the device the workspace was made on
    /// or any other holder its member list names: every holder keeps a
    /// writable copy, so what is set at one holder is written by it and
    /// reaches the rest. An existing record keeps its graph and gains only
    /// the change, and an absent one starts from it. Members' pages admit it
    /// like any member's change to that record.
    ///
    /// # Errors
    /// Refuses a device that holds no copy or has no administration of the
    /// workspace, incomplete records, sealed
    /// ones whose channel this server does not take part in, failed changes or
    /// oversized publication.
    pub async fn author_workspace_document<R>(
        &self,
        workspace: &str,
        holder: &DeviceCert,
        (app, name): (&str, &str),
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        self.author_workspace_document_from((workspace, holder), (app, name), None, change)
            .await
    }

    /// [`Self::author_workspace_document`] of a document this workspace holds
    /// none of yet, started from `genesis` instead of nothing: a document
    /// whose first change every device makes alike (a
    /// [`tilo_resource::genesis`]), so two devices that each write it before
    /// they see one another write one document and not two competing roots.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace record stays locked through durable signed publication"
    )]
    pub async fn author_workspace_document_from<R>(
        &self,
        (workspace, holder): (&str, &DeviceCert),
        (app, name): (&str, &str),
        genesis: Option<Box<automerge::AutoCommit>>,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if &governed.record.owner != holder {
            // Every other holder its member list names writes it too: each
            // keeps a writable copy.
            super::holding::require_holder(&governed.peer(), holder)?;
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(holder.device_public_key, system_time(now))?;
        let keys = self
            .sealing_keys(&governed, app, name)
            .await
            .map_err(CollaborationError::new)?;
        let existing = governed
            .readable_document(app, name)
            .await
            .map_err(CollaborationError::new)?;
        let (mut document, heads) = match existing {
            Some(bytes) => {
                let mut document = automerge::AutoCommit::load(&bytes)
                    .map_err(|error| CollaborationError::new(error.to_string()))?;
                let heads = match &keys {
                    Some(_) => document.get_heads(),
                    None => governed
                        .peer()
                        .plaintext_complete_heads(app, name)
                        .ok_or_else(|| {
                            CollaborationError::new("record is not complete plaintext")
                        })?,
                };
                (document, Some(heads))
            }
            None => (
                genesis.map_or_else(automerge::AutoCommit::new, |start| *start),
                None,
            ),
        };
        let result = change(&mut document)?;
        if automerge::transaction::Transactable::pending_ops(&document) != 0 {
            document.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
        }
        if heads.is_none() && !founds_as_read(&document, (app, name), workspace) {
            return Err(CollaborationError::new(format!(
                "{app} {name} would be founded without the identity every reader of the \
                 workspace's own records checks; it starts from its genesis"
            )));
        }
        // Build the change on the workspace as it stands, with its copy locked
        // from the build to the commit, the staging the workspace's other
        // changes use. The link takes members' writes into that same copy
        // without the workspace's other lock, so a candidate cloned before
        // this lock can be older than the copy by the time it commits, and the
        // commit is then refused for history it was not built on. Locked here,
        // a write the link takes in meanwhile waits for this commit, which
        // publishes it with this change.
        let (result, published) = governed
            .change_held(
                &self.inner.host.key,
                system_time(now),
                |_, candidate| {
                    let published = publish_authored(
                        candidate,
                        keys.as_ref(),
                        (app, name),
                        (&mut document, heads),
                        (&self.inner.host.key, system_time(now)),
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(((result, published), published))
                },
                |record, _| Ok(record.collaboration.clone()),
            )
            .map_err(CollaborationError::new)?;
        if !published {
            return Ok(result);
        }
        self.settle(&mut governed).await?;
        Ok(result)
    }
}

/// Whether `document`, the first this workspace holds of `name` of `app`, names itself as every
/// reader of it checks: true for anything but a record each of the workspace's apps makes from
/// its kind alone ([`collab_types::WORKSPACE_DOCUMENTS`]). Such a record founded without its
/// identity, or with another workspace's, is refused by every reader that merges it, and
/// whatever founds the same record properly afterwards cannot make it readable again.
fn founds_as_read(
    document: &automerge::AutoCommit,
    (app, name): (&str, &str),
    workspace: &str,
) -> bool {
    use collab_types::document::identity;
    let Some(kept) = collab_types::workspace_document(app, name) else {
        return true;
    };
    identity::identity(document)
        .is_some_and(|found| found.id.as_str() == name && found.kind.as_str() == kept.kind)
        && identity::home(document).is_some_and(|home| !home.is_empty())
        && identity::workspace_scope(document).as_deref() == Some(workspace)
}

/// Publish what `document` holds beyond `heads` (all of it for a record not
/// yet held), sealed under `keys` where the record's channel has them; false
/// when it holds nothing new.
fn publish_authored(
    candidate: &mut tilo_peer_state::StatePeer,
    keys: Option<&super::sealed_documents::DocumentKeys>,
    (app, record): (&str, &str),
    (document, heads): (
        &mut automerge::AutoCommit,
        Option<Vec<automerge::ChangeHash>>,
    ),
    (key, now): (&ed25519_dalek::SigningKey, std::time::SystemTime),
) -> Result<bool, CollaborationError> {
    if document.get_heads() == heads.clone().unwrap_or_default() {
        return Ok(false);
    }
    let bytes = match (&heads, keys) {
        (Some(heads), _) => document.save_after(heads),
        (None, Some(_)) => document.save_after(&[]),
        (None, None) => document.save(),
    };
    if bytes.len() > 64 * 1024 {
        return Err(CollaborationError::new(
            "record authoring exceeds publication limit",
        ));
    }
    match (heads, keys) {
        (Some(_), keys) => {
            super::sealed_documents::publish_changes(
                candidate,
                keys,
                (app, record),
                bytes,
                key,
                now,
            )?;
        }
        (None, Some(keys)) => {
            super::sealed_documents::start_sealed(candidate, keys, (app, record), bytes, key, now)?;
        }
        (None, None) => {
            candidate
                .publication_mut()
                .publish_document(app, record, bytes, key, now)?;
        }
    }
    Ok(true)
}
