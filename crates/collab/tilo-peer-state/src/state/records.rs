//! The records this replica holds.

use super::{DocumentMutation, DocumentRow, NeighbourMsg, ReadDoc, Route, StatePeer};

use automerge::transaction::Transactable as _;

impl StatePeer {
    pub fn pending_count(&self) -> usize {
        self.core.pending.len()
    }
    pub fn covers(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return false;
        };
        if cluster_id != &self.core.workspace {
            return false;
        }
        let Some(document) = self.core.documents.get(&Route {
            app: app_id.clone(),
            record: record_id.clone(),
        }) else {
            return false;
        };
        if document
            .covered
            .get(&hex::encode(envelope.author.as_bytes()))
            .copied()
            .unwrap_or(0)
            < envelope.seq
        {
            return false;
        }
        let Some(snapshot) = &document.covered_snapshot else {
            return false;
        };
        let Ok(collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Incremental(bytes))) =
            collab_types::validate_whole_document_payload_route(
                app_id, cluster_id, record_id, envelope,
            )
        else {
            return false;
        };
        let Ok(checkpoint) = snapshot.decoded() else {
            return false;
        };
        // A sequence floor is not causal coverage: an out-of-order incremental
        // may have been validated while its dependencies were unavailable.
        // Contained: applied in the isolate to a copy of the checkpoint, the
        // peer's bytes leave its heads where they were and nothing waiting.
        let mut proof = checkpoint.doc.clone();
        matches!(
            tilo_automerge_isolate::apply(
                &mut proof,
                &bytes,
                tilo_automerge_isolate::Dependencies::RequireComplete
            ),
            Ok(tilo_automerge_isolate::Applied::Changed(false))
        )
    }
    pub fn rows(&self) -> Vec<DocumentRow> {
        self.core
            .documents
            .iter()
            .map(|(route, entry)| DocumentRow {
                app_id: route.app.clone(),
                record_id: route.record.clone(),
                heads: entry
                    .doc
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            })
            .collect()
    }

    /// Read the admitted document basis without serializing its contents.
    pub fn document_heads(&self, app: &str, record: &str) -> Option<Vec<automerge::ChangeHash>> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.base_heads())
    }

    /// Whether `record`'s governed checkpoint is still arriving: the workspace
    /// already carries its coverage, but the checkpoint body it names has
    /// not been admitted, so the document's accepted checkpoint is an older
    /// one. A read waits for the body; it is a cut in flight, not a damaged
    /// replica, and durable state taken now restores.
    #[must_use]
    pub fn checkpoint_pending(&self, app: &str, record: &str) -> bool {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return false;
        };
        let snapshot = document
            .covered_snapshot
            .as_ref()
            .map(|snapshot| snapshot.0.bytes.as_slice());
        let workspace = self
            .core
            .workspace_doc
            .as_deref()
            .map(crate::workspace::read::LegacyRead);
        matches!(
            crate::coverage::read::governed_snapshot_matches(
                workspace.as_ref(),
                (app, record),
                &document.covered,
                snapshot,
                &crate::coverage::read::LegacyCoverage,
            ),
            Ok(crate::coverage::read::Governed::Arriving)
        )
    }

    /// Heads of a plaintext, causally complete document. Missing, encrypted,
    /// and incomplete routes return `None` so a caller falls through to a
    /// restoring read and keeps that refusal.
    pub fn plaintext_complete_heads(
        &self,
        app: &str,
        record: &str,
    ) -> Option<Vec<automerge::ChangeHash>> {
        let entry = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        })?;
        if entry.encryption_required || !ReadDoc::get_missing_deps(&*entry.doc, &[]).is_empty() {
            return None;
        }
        Some(entry.doc.base_heads())
    }

    /// The `(app, record)` of every record this peer holds readable, not
    /// sealed for a channel's members.
    pub fn plaintext_routes(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .iter()
            .filter(|(_, entry)| !entry.encryption_required)
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect()
    }
    /// Whether this state holds `record` of `app`, without saving it as
    /// [`Self::document`] does.
    #[must_use]
    pub fn holds_document(&self, app: &str, record: &str) -> bool {
        self.core.documents.contains_key(&Route {
            app: app.into(),
            record: record.into(),
        })
    }
    /// The `(app, record)` of every encrypted record this state holds: only
    /// its channel's key holders read it.
    #[must_use]
    pub fn sealed_routes(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .iter()
            .filter(|(_, entry)| entry.encryption_required)
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect()
    }
    /// Encrypt `record` of `app`, held in the clear, from now on: its
    /// channel's key holder decided that its writes are sealed, so a write
    /// in the clear is refused as a downgrade and no checkpoint of it is
    /// published in the clear. Whether the record is held.
    pub fn seal_record(&mut self, app: &str, record: &str) -> bool {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return false;
        };
        if !document.encryption_required {
            let mut sealed = document.clone();
            sealed.encryption_required = true;
            self.core.documents.insert(route, sealed);
        }
        true
    }
    pub fn document(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .map(|entry| entry.doc.saved())
    }
    /// Every record this peer holds, by the app whose document it is and its
    /// id: what a host of the workspace reads without being told each route.
    #[must_use]
    pub fn records(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .keys()
            .map(|route| (route.app.clone(), route.record.clone()))
            .collect()
    }
}
