//! A document's content keys as a holder opens, seals and reads it: the keys of
//! the group a record is sealed for, scoped to the record, and the device's
//! answer they come from.
//!
//! What a holder needs to open a sealed record is here, over the device's
//! [`GroupKeys`](crate::documents::device_keys::GroupKeys) and the record's
//! route; the host's own folding of a history (its compaction) is the holder's,
//! beside the workspaces it serves.

use collab_types::{ArtifactId, SignedOp};
use key_group::ContentKeys;

use crate::documents::device_keys::DeviceKeys;

/// The content keys of one document, as the node holds them: its readers'
/// group's keys, scoped to the document.
pub(crate) struct DocumentKeys {
    pub(crate) keys: ContentKeys,
    pub(crate) workspace_keys: Option<ContentKeys>,
    pub(crate) prior_reader_keys: Option<ContentKeys>,
    #[cfg_attr(
        target_arch = "wasm32",
        allow(
            dead_code,
            reason = "the reader set names the group a history folds into, which is the host's"
        )
    )]
    pub(crate) readers: Option<String>,
    pub(crate) artifact: ArtifactId,
}

impl DocumentKeys {
    /// The workspace's keys for a document every member can read.
    pub(crate) fn app_document(
        keys: ContentKeys,
        workspace: &str,
        (app, document): (&str, &str),
    ) -> Self {
        Self {
            keys,
            workspace_keys: None,
            prior_reader_keys: None,
            readers: None,
            artifact: collab_types::document_artifact_id(app, workspace, document),
        }
    }

    /// The epoch of the keys: a rotation moves it.
    pub(crate) fn epoch(&self) -> u64 {
        self.keys.epoch()
    }

    pub(crate) fn covers(&self, metadata: collab_types::EncryptionMetadata) -> bool {
        self.keys.holds_epoch(metadata.epoch)
            || (!matches!(
                metadata.scheme,
                collab_types::EncryptionScheme::DocumentContentKey { .. }
            ) && [&self.prior_reader_keys, &self.workspace_keys]
                .into_iter()
                .any(|keys| {
                    keys.as_ref()
                        .is_some_and(|keys| keys.holds_epoch(metadata.epoch))
                }))
    }

    /// Open any write of the record's history, under any epoch the keys
    /// hold, a compaction included.
    pub(crate) fn open(&self) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> + '_ {
        |op| {
            let metadata = op.encryption.ok_or("the write is not sealed")?;
            let ciphertext =
                collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
            let keys = match metadata.scheme {
                collab_types::EncryptionScheme::DocumentContentKey { .. } => &self.keys,
                _ if op.artifact != self.artifact => {
                    self.workspace_keys.as_ref().unwrap_or(&self.keys)
                }
                _ => self.prior_reader_keys.as_ref().unwrap_or(&self.keys),
            };
            keys.open_content(op.artifact, &metadata, &ciphertext)
                .map(|plaintext| plaintext.to_vec())
                .map_err(|error| error.to_string())
        }
    }
}

pub(crate) async fn read_document_keys(
    device: &DeviceKeys,
    workspace: &str,
    route: (&str, &str),
    scoped: bool,
    create: bool,
) -> Result<DocumentKeys, String> {
    read_document_keys_for(device, workspace, route, scoped, create, None).await
}

pub(crate) async fn read_document_keys_for(
    device: &DeviceKeys,
    workspace: &str,
    route: (&str, &str),
    scoped: bool,
    create: bool,
    named: Option<String>,
) -> Result<DocumentKeys, String> {
    let ask = DocumentKeyAsk::of(device, workspace, route, scoped, named)?;
    let keys = device
        .group_keys(&ask.group, ask.readers(), create)
        .await
        .ok_or("no keys are available for this document")??;
    Ok(ask.with_keys(device, keys).await)
}

/// The group a document's keys are asked of, and what else opens it: what a holder that must
/// not wait for the keys looks up, and asks for again elsewhere when they are not here.
pub(crate) struct DocumentKeyAsk {
    /// The group the document is sealed for: the workspace's own, or the document's.
    pub(crate) group: String,
    named: Option<String>,
    scoped: bool,
    workspace: String,
    route: tilo_peer_state::document_frame::DocumentRoute,
}

impl DocumentKeyAsk {
    /// The ask for the keys of `document` of `app` in `workspace`, scoped to the document's own
    /// group when `scoped`; `named` its reader set, else the one `device` knows of.
    pub(crate) fn of(
        device: &DeviceKeys,
        workspace: &str,
        (app, document): (&str, &str),
        scoped: bool,
        named: Option<String>,
    ) -> Result<Self, String> {
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document: document.to_owned(),
        };
        let named = if neighbour_wire::KeyScope::has_named_readers(document) {
            let workspace = route.workspace_id().map_err(|error| error.to_string())?;
            Some(
                named
                    .or_else(|| device.named_readers(workspace, document))
                    .ok_or("the reader set of the document is not known here")?,
            )
        } else {
            None
        };
        let group = if scoped {
            route
                .key_scope_for(named.as_deref())
                .map_err(|error| error.to_string())?
                .to_string()
        } else {
            workspace.to_owned()
        };
        Ok(Self {
            group,
            named,
            scoped,
            workspace: workspace.to_owned(),
            route,
        })
    }

    /// The reader set the group is asked for.
    pub(crate) fn readers(&self) -> &str {
        self.named.as_deref().unwrap_or(collab_types::EVERY_MEMBER)
    }

    /// The document's keys, `keys` its group's: with the workspace's and the reader set's
    /// earlier keys where `device` holds them, looked up and never waited for.
    pub(crate) async fn with_keys(self, device: &DeviceKeys, keys: ContentKeys) -> DocumentKeys {
        let workspace = self.workspace.as_str();
        let workspace_keys = if self.scoped || self.named.is_some() {
            device
                .group_keys(workspace, collab_types::EVERY_MEMBER, false)
                .await
                .and_then(Result::ok)
        } else {
            None
        };
        let prior_reader_keys = if self.scoped && self.named.is_some() {
            device
                .group_keys(workspace, self.readers(), false)
                .await
                .and_then(Result::ok)
        } else if self.scoped {
            workspace_keys.clone()
        } else {
            None
        };
        DocumentKeys {
            keys,
            workspace_keys,
            prior_reader_keys,
            artifact: self.route.artifact(),
            readers: self.named,
        }
    }
}
