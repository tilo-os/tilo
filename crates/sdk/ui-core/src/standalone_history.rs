//! A private History session pinned to the document that opened it.
//!
//! The host owns presentation and semantic actions. This model owns the captured
//! destination and read authorization; it never redirects a stale destination
//! to the outer app document and never commits a live edit while browsing.

use automerge::{AutoCommit, ChangeHash, transaction::Transactable};

use crate::bound_dispatch::{BoundDispatchError, CallbackBinding};
use crate::history_source::{
    AuthorDirectory, CompareError, HistoryComparison, HistorySource, committed_heads,
    compare_entries,
};
use crate::permissions::{PermissionContext, PermissionScope, Verb};
use crate::record::{RecordId, RecordOpenError};
use crate::widget::{HistoryEntry, HistoryEntryId};

#[derive(Clone)]
pub(crate) enum HistoryTarget {
    Bound(Box<CallbackBinding>),
    Legacy {
        app_id: String,
        record: Option<RecordId>,
        /// Supplied by the host and replaced whenever it replaces the document,
        /// including reopening the same record. A record name alone is not a
        /// document lifetime.
        document_identity: uuid::Uuid,
    },
}

pub(crate) struct HistoryReadContext<'a> {
    pub document: &'a AutoCommit,
    pub active_record: Option<&'a RecordId>,
    pub document_identity: uuid::Uuid,
    pub permissions: &'a PermissionContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HistoryError {
    Denied,
    Stale,
    Unavailable,
    InvalidSelection,
    Comparison(CompareError),
}

impl HistoryTarget {
    fn read<R>(
        &self,
        context: &HistoryReadContext<'_>,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, HistoryError> {
        match self {
            Self::Bound(binding) => {
                if binding.registry_owner.upgrade().is_none() {
                    return Err(HistoryError::Stale);
                }
                let allowed = binding
                    .permissions
                    .try_borrow()
                    .map_err(|_| HistoryError::Unavailable)?
                    .check(&PermissionScope::app(&binding.app_id, Verb::Read));
                if !allowed {
                    return Err(HistoryError::Denied);
                }
                binding
                    .read_document(context.document, |doc, _| read(doc))
                    .map_err(|error| match error {
                        BoundDispatchError::ClosedLifetime
                        | BoundDispatchError::MissingNode
                        | BoundDispatchError::Record(
                            RecordOpenError::MissingRecord | RecordOpenError::IdentityMismatch,
                        ) => HistoryError::Stale,
                        _ => HistoryError::Unavailable,
                    })
            }
            Self::Legacy {
                app_id,
                record,
                document_identity,
            } => {
                if *document_identity != context.document_identity
                    || record.as_ref() != context.active_record
                {
                    return Err(HistoryError::Stale);
                }
                if !context
                    .permissions
                    .check(&PermissionScope::app(app_id, Verb::Read))
                {
                    return Err(HistoryError::Denied);
                }
                Ok(read(context.document))
            }
        }
    }
}

pub(crate) struct StandaloneHistory {
    target: HistoryTarget,
    source: HistorySource,
    heads: Option<Vec<ChangeHash>>,
    rows: Vec<HistoryEntry>,
    selection: Vec<HistoryEntryId>,
    comparison: Option<HistoryComparison>,
    invalidated: Option<HistoryError>,
}

impl StandaloneHistory {
    pub(crate) fn open(
        target: HistoryTarget,
        context: &HistoryReadContext<'_>,
        authors: AuthorDirectory,
    ) -> Result<Self, HistoryError> {
        let mut session = Self {
            target,
            source: HistorySource::new().with_authors(authors),
            heads: None,
            rows: Vec::new(),
            selection: Vec::new(),
            comparison: None,
            invalidated: None,
        };
        session.refresh(context)?;
        Ok(session)
    }

    /// Validate authority for an immutable observation of the last rendered
    /// frame. This does not refresh rows or close a pending edit transaction.
    pub(crate) fn validate(&self, context: &HistoryReadContext<'_>) -> Result<(), HistoryError> {
        if let Some(error) = &self.invalidated {
            return Err(error.clone());
        }
        self.target.read(context, |_| ())
    }

    /// Revalidate even on a cache hit. The host must refresh before exposing
    /// cached rows or comparison snapshots to a frame or semantic reader.
    pub(crate) fn refresh(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<bool, HistoryError> {
        if let Some(error) = &self.invalidated {
            return Err(error.clone());
        }
        let projected = self.target.read(context, |doc| {
            let heads = committed_heads(doc);
            if self.heads.as_ref() == Some(&heads) {
                return None;
            }
            let rows = self.source.entries(&mut committed_snapshot(doc));
            Some((heads, rows))
        });
        let Some((heads, rows)) = self.checked(projected)? else {
            return Ok(false);
        };
        self.heads = Some(heads);
        self.rows = rows;
        self.comparison = None;
        if self
            .selection
            .iter()
            .any(|id| !self.rows.iter().any(|row| row.id == *id))
        {
            self.selection.clear();
        }
        Ok(true)
    }

    pub(crate) fn rows(&self) -> &[HistoryEntry] {
        &self.rows
    }

    pub(crate) fn selection(&self) -> &[HistoryEntryId] {
        &self.selection
    }

    pub(crate) fn comparison(&self) -> Option<&HistoryComparison> {
        self.comparison.as_ref()
    }

    /// Only currently exposed entries in this captured document are selectable.
    /// Empty selection explicitly clears the comparison; duplicate ids do not
    /// widen a comparison or its allocation bound.
    pub(crate) fn select(
        &mut self,
        context: &HistoryReadContext<'_>,
        entries: &[HistoryEntryId],
    ) -> Result<(), HistoryError> {
        self.refresh(context)?;
        self.selection.clear();
        self.comparison = None;
        if entries.len() > self.rows.len()
            || entries
                .iter()
                .any(|id| !self.rows.iter().any(|row| row.id == *id))
        {
            return Err(HistoryError::InvalidSelection);
        }
        for id in entries {
            if !self.selection.contains(id) {
                self.selection.push(*id);
            }
        }
        Ok(())
    }

    pub(crate) fn compare_selection(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<&HistoryComparison, HistoryError> {
        self.refresh(context)?;
        self.comparison = None;
        if self.selection.is_empty() {
            return Err(HistoryError::InvalidSelection);
        }
        let comparison = self
            .target
            .read(context, |doc| {
                compare_entries(&mut committed_snapshot(doc), &self.selection)
                    .map_err(HistoryError::Comparison)
            })
            .and_then(std::convert::identity);
        self.comparison = Some(self.checked(comparison)?);
        self.comparison.as_ref().ok_or(HistoryError::Unavailable)
    }

    fn checked<R>(&mut self, result: Result<R, HistoryError>) -> Result<R, HistoryError> {
        if let Err(error) = &result {
            self.heads = None;
            self.rows.clear();
            self.selection.clear();
            self.comparison = None;
            // Regranting access or reopening a record requires a fresh user
            // session, so an old queued action cannot revive a closed panel.
            if matches!(error, HistoryError::Denied | HistoryError::Stale) {
                self.invalidated = Some(error.clone());
            }
        }
        result
    }
}

fn committed_snapshot(doc: &AutoCommit) -> AutoCommit {
    let mut snapshot = doc.clone();
    if snapshot.pending_ops() > 0 {
        snapshot.rollback();
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use automerge::{ROOT, ReadDoc, transaction::CommitOptions};

    use super::*;
    use crate::permissions::{Actor, CapOverride, Scope, Subject};

    fn document(value: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", value).unwrap();
        doc.commit_with(CommitOptions::default().with_time(1));
        doc
    }

    fn read_only(app_id: &str) -> PermissionContext {
        PermissionContext::from_capabilities(
            Actor::agent("reader"),
            vec![CapOverride::grant(
                Subject::agent("reader"),
                Scope::App(app_id.into()),
                Verb::Read,
            )],
        )
    }

    fn target(record: Option<RecordId>, identity: uuid::Uuid) -> HistoryTarget {
        HistoryTarget::Legacy {
            app_id: "notes".into(),
            record,
            document_identity: identity,
        }
    }

    #[test]
    fn browsing_and_comparison_preserve_pending_edit_and_attribution() {
        let mut doc = document("saved");
        let record = RecordId::for_file("a.md");
        let identity = uuid::Uuid::new_v4();
        let permissions = read_only("notes");
        let before = doc.base_heads();
        doc.put(ROOT, "body", "still typing").unwrap();
        let pending = doc.pending_ops();
        let context = HistoryReadContext {
            document: &doc,
            active_record: Some(&record),
            document_identity: identity,
            permissions: &permissions,
        };
        let mut session = StandaloneHistory::open(
            target(Some(record.clone()), identity),
            &context,
            AuthorDirectory::new(),
        )
        .unwrap();
        assert_eq!(session.rows().len(), 1);
        assert!(!session.refresh(&context).unwrap());
        session.select(&context, &[session.rows()[0].id]).unwrap();
        let comparison = session.compare_selection(&context).unwrap();
        let after = AutoCommit::load(&comparison.after_document).unwrap();
        assert_eq!(
            after.get(ROOT, "body").unwrap().unwrap().0.as_str(),
            Some("saved")
        );
        assert_eq!(doc.pending_ops(), pending);
        assert_eq!(doc.base_heads(), before);
        doc.put(ROOT, "body", "finished").unwrap();
        let action = tilo_actions::ActionContext::for_resource("Finish edit", "a", "note", "A");
        doc.commit_with(CommitOptions::default().with_message(action.change_message().unwrap()));
        let changes = doc.get_changes(&before);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0]
                .message()
                .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
                .unwrap()
                .action_label,
            "Finish edit",
        );
    }

    #[test]
    fn record_switch_clears_old_rows_and_does_not_fall_back() {
        let first = document("first");
        let second = document("second");
        let a = RecordId::for_file("a.md");
        let b = RecordId::for_file("b.md");
        let identity = uuid::Uuid::new_v4();
        let permissions = PermissionContext::permissive();
        let context = HistoryReadContext {
            document: &first,
            active_record: Some(&a),
            document_identity: identity,
            permissions: &permissions,
        };
        let mut session = StandaloneHistory::open(
            target(Some(a.clone()), identity),
            &context,
            AuthorDirectory::new(),
        )
        .unwrap();
        session.select(&context, &[session.rows()[0].id]).unwrap();
        session.compare_selection(&context).unwrap();
        let switched = HistoryReadContext {
            document: &second,
            active_record: Some(&b),
            ..context
        };
        assert_eq!(session.refresh(&switched), Err(HistoryError::Stale));
        assert!(session.rows().is_empty());
        assert!(session.selection().is_empty());
        assert!(session.comparison().is_none());
        assert_eq!(session.refresh(&context), Err(HistoryError::Stale));
    }

    #[test]
    fn replacing_same_record_or_app_document_invalidates_old_session() {
        let first = document("first");
        let replacement = document("replacement");
        let permissions = PermissionContext::permissive();
        for record in [None, Some(RecordId::for_tab(3))] {
            let identity = uuid::Uuid::new_v4();
            let context = HistoryReadContext {
                document: &first,
                active_record: record.as_ref(),
                document_identity: identity,
                permissions: &permissions,
            };
            let mut session = StandaloneHistory::open(
                target(record.clone(), identity),
                &context,
                AuthorDirectory::new(),
            )
            .unwrap();
            let replaced = HistoryReadContext {
                document: &replacement,
                document_identity: uuid::Uuid::new_v4(),
                ..context
            };
            assert_eq!(session.refresh(&replaced), Err(HistoryError::Stale));
            assert!(session.rows().is_empty());
        }
    }

    #[test]
    fn revocation_is_checked_on_cached_refresh_select_and_compare() {
        let doc = document("private");
        let identity = uuid::Uuid::new_v4();
        let permissions = read_only("notes");
        let denied = read_only("other-app");
        let context = HistoryReadContext {
            document: &doc,
            active_record: None,
            document_identity: identity,
            permissions: &permissions,
        };
        let revoked = HistoryReadContext {
            permissions: &denied,
            ..context
        };
        for operation in 0..3 {
            let mut session =
                StandaloneHistory::open(target(None, identity), &context, AuthorDirectory::new())
                    .unwrap();
            let id = session.rows()[0].id;
            session.select(&context, &[id]).unwrap();
            session.compare_selection(&context).unwrap();
            let result = match operation {
                0 => session.refresh(&revoked).map(|_| ()),
                1 => session.select(&revoked, &[id]),
                _ => session.compare_selection(&revoked).map(|_| ()),
            };
            assert_eq!(result, Err(HistoryError::Denied));
            assert!(session.rows().is_empty());
            assert!(session.selection().is_empty());
            assert!(session.comparison().is_none());
            assert_eq!(session.refresh(&context), Err(HistoryError::Denied));
        }
    }

    #[test]
    fn unknown_selection_cannot_materialize_another_documents_change() {
        let doc = document("first");
        let mut foreign = document("foreign");
        let foreign_id = HistorySource::new().entries(&mut foreign)[0].id;
        let identity = uuid::Uuid::new_v4();
        let permissions = PermissionContext::permissive();
        let context = HistoryReadContext {
            document: &doc,
            active_record: None,
            document_identity: identity,
            permissions: &permissions,
        };
        let mut session =
            StandaloneHistory::open(target(None, identity), &context, AuthorDirectory::new())
                .unwrap();
        assert_eq!(
            session.select(&context, &[foreign_id]),
            Err(HistoryError::InvalidSelection)
        );
        assert_eq!(
            session.compare_selection(&context).unwrap_err(),
            HistoryError::InvalidSelection
        );
        assert!(session.comparison().is_none());
    }
}
