//! Records the workspace's governing host writes itself, outside any app's factory: the
//! members' shared desktop, a person's servers in their admin workspace. Every host writes them,
//! whether or not it runs agents, so this is not part of `applications`.

use super::authority::WorkspaceOp;
use super::{CollaborationError, Workspaces, system_time};
use crate::host_authority::HostOperation;
use tilo_user_identity::DeviceCert;

impl Workspaces {
    /// Author a workspace record no app factory makes, such as the shared
    /// desktop members' pages keep, as its exact current governor: an
    /// existing record keeps its graph and gains only the change, and an
    /// absent one starts from it. Members' pages admit it like any member's
    /// change to that record.
    ///
    /// # Errors
    /// Refuses other certificates, stale custody, incomplete records, sealed
    /// ones whose channel this relay does not take part in, failed changes or
    /// oversized publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor record stays locked through durable signed publication"
    )]
    pub async fn author_workspace_document<R>(
        &self,
        workspace: &str,
        governor: &DeviceCert,
        (app, record): (&str, &str),
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<R, String>,
    ) -> Result<R, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            governor,
            None,
            WorkspaceOp::Host(HostOperation::AuthorApplications),
            now,
        )?;
        if governor != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "record authoring requires exact governor custody",
            ));
        }
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        if &governed.record.authority != governor {
            return Err(CollaborationError::new(
                "record governor differs from current workspace authority",
            ));
        }
        governed
            .peer()
            .workspace()
            .check_workspace_administration(governor.device_public_key, system_time(now))?;
        let keys = self
            .sealing_keys(&governed, app, record)
            .await
            .map_err(CollaborationError::new)?;
        let existing = governed
            .readable_document(app, record)
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
                        .plaintext_complete_heads(app, record)
                        .ok_or_else(|| {
                            CollaborationError::new("record is not complete plaintext")
                        })?,
                };
                (document, Some(heads))
            }
            None => (automerge::AutoCommit::new(), None),
        };
        let result = change(&mut document)?;
        if automerge::transaction::Transactable::pending_ops(&document) != 0 {
            document.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
        }
        let mut candidate = governed.peer().clone();
        let published = publish_authored(
            &mut candidate,
            keys.as_ref(),
            (app, record),
            (&mut document, heads),
            (&self.inner.host.key, system_time(now)),
        )?;
        if !published {
            return Ok(result);
        }
        let collaboration = governed.collaboration().clone();
        governed.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(collaboration)
        })?;
        governed.absorb_desktop((app, record), &mut document);
        self.settle(&mut governed).await?;
        Ok(result)
    }
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
