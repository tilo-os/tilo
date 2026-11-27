//! Opaque ownership observations. These never enter historical presentation inputs.
use super::{HistoryRegistrationError, HistoryResource};
use crate::history_source::verbs::HistoryEdit;
use crate::{
    bound_dispatch::CallbackBinding,
    resource::{DocumentCheckout, ResourceStore},
};
use automerge::{AutoCommit, transaction::Transactable};
use std::rc::{Rc, Weak};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    pub static OWNED_DOCUMENT_CLONES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

pub enum OwnedDocumentPublication {
    Unchanged,
    Unfinished,
    Blocked,
    Changed {
        mutation: collab_types::DocumentMutation,
        heads: Vec<automerge::ChangeHash>,
        /// The document the mutation was read from: what a change that continues an earlier one names
        /// its action through.
        document: AutoCommit,
    },
}

pub use tilo_ui_base::control_domain::ResourceOwnerLifetime;

/// Captured by the actual mounted owner; callers cannot manufacture ownership.
#[derive(Clone)]
pub struct HistoryOwner(pub(crate) OwnerKind);
#[derive(Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "avoid a heap allocation for each captured component ownership observation"
)]
pub(crate) enum OwnerKind {
    Component(CallbackBinding),
    Record {
        store: ResourceStore,
        checkout: DocumentCheckout,
        lifetime: Weak<ResourceOwnerLifetime>,
        /// Whether this app publishes the record's changes: the identity service signs a change on a
        /// route only for the app of that route, or for one of the workspace's shared records
        /// ([`StoredResource::publishes`](crate::resource::StoredResource::publishes)). A record this app
        /// only reads is merged from its route, never published on it.
        publishes: bool,
    },
}
impl HistoryOwner {
    pub fn component(binding: CallbackBinding) -> Self {
        Self(OwnerKind::Component(binding))
    }
    /// The mounted component this owner is, if it is one.
    pub(crate) fn component_binding(&self) -> Option<&CallbackBinding> {
        match &self.0 {
            OwnerKind::Component(binding) => Some(binding),
            OwnerKind::Record { .. } => None,
        }
    }
    /// `record`, held by this app, which `publishes` its changes when it is the app that may
    /// ([`Self::publishes_resource`]): a record it only reads is joined and merged from its route, never
    /// published on it.
    pub fn record(
        store: ResourceStore,
        checkout: DocumentCheckout,
        lifetime: &Rc<ResourceOwnerLifetime>,
        publishes: bool,
    ) -> Self {
        Self(OwnerKind::Record {
            store,
            checkout,
            lifetime: Rc::downgrade(lifetime),
            publishes,
        })
    }

    /// Whether this app publishes what changes in the record behind this owner ([`Self::record`]).
    pub(crate) fn publishes_resource(&self) -> bool {
        match &self.0 {
            OwnerKind::Component(_) => true,
            OwnerKind::Record { publishes, .. } => *publishes,
        }
    }
    pub(crate) fn lifetime_id(&self) -> Option<uuid::Uuid> {
        match &self.0 {
            OwnerKind::Component(binding) => Some(binding.lifetime_id),
            OwnerKind::Record { lifetime, .. } => lifetime
                .upgrade()
                .filter(|life| life.active.get())
                .map(|life| life.id),
        }
    }
    pub fn same_owner(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (OwnerKind::Component(a), OwnerKind::Component(b)) => a.same_component(b),
            (
                OwnerKind::Record {
                    store: a,
                    checkout: ac,
                    lifetime: al,
                    ..
                },
                OwnerKind::Record {
                    store: b,
                    checkout: bc,
                    lifetime: bl,
                    ..
                },
            ) => a.same_owner(b) && ac == bc && Weak::ptr_eq(al, bl),
            _ => false,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), HistoryRegistrationError> {
        let (store, checkout) = match &self.0 {
            OwnerKind::Component(binding) => {
                binding
                    .enter()
                    .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
                (&binding.records, &binding.checkout)
            }
            OwnerKind::Record {
                store,
                checkout,
                lifetime,
                ..
            } => {
                if !lifetime.upgrade().is_some_and(|life| life.active.get()) {
                    return Err(HistoryRegistrationError::UnsupportedOwner);
                }
                (store, checkout)
            }
        };
        store
            .open_existing_resource_checked(checkout)
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)
    }
    pub(crate) fn resource(
        &self,
        app_id: &str,
    ) -> Result<HistoryResource, HistoryRegistrationError> {
        self.validate()?;
        let (store, checkout, lifetime) = match &self.0 {
            OwnerKind::Component(binding) => {
                if binding.app_id != app_id {
                    return Err(HistoryRegistrationError::ForeignSource);
                }
                (&binding.records, &binding.checkout, binding.lifetime_id)
            }
            OwnerKind::Record {
                store,
                checkout,
                lifetime,
                ..
            } => (
                store,
                checkout,
                lifetime
                    .upgrade()
                    .ok_or(HistoryRegistrationError::UnsupportedOwner)?
                    .id,
            ),
        };
        let (storage, package, workspace) = store
            .history_namespace(&checkout.id)
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        if !package.is_empty() && package != app_id {
            return Err(HistoryRegistrationError::ForeignSource);
        }
        Ok(HistoryResource::new(
            storage.unwrap_or_else(|| format!("memory:{lifetime}")),
            if workspace.is_empty() {
                format!("lifetime:{lifetime}")
            } else {
                workspace
            },
            app_id,
            checkout.id.clone(),
        ))
    }
    /// Read the committed frontier through the same ownership and record loan as `document`.
    pub(crate) fn committed_heads(
        &self,
        outer: &AutoCommit,
    ) -> Result<Vec<automerge::ChangeHash>, HistoryRegistrationError> {
        self.validate()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .read_document(outer, |doc, _| doc.base_heads())
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Record {
                store, checkout, ..
            } => store
                .read_checked(&checkout.id, |doc| doc.base_heads())
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
        }
    }

    pub(crate) fn document(
        &self,
        outer: &AutoCommit,
    ) -> Result<AutoCommit, HistoryRegistrationError> {
        self.validate()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .read_document(outer, |doc, _| {
                    #[cfg(any(test, feature = "test-support"))]
                    OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
                    doc.clone()
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Record {
                store, checkout, ..
            } => store
                .read_checked(&checkout.id, |doc| {
                    #[cfg(any(test, feature = "test-support"))]
                    OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
                    doc.clone()
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
        }
    }

    pub(crate) fn prepare_publication(
        &self,
        outer: &AutoCommit,
        sent: &mut crate::state::DocumentFrontier,
        needs_snapshot: bool,
        blocked: bool,
    ) -> Result<OwnedDocumentPublication, HistoryRegistrationError> {
        fn inspect(
            document: &AutoCommit,
            sent: &mut crate::state::DocumentFrontier,
            needs_snapshot: bool,
            blocked: bool,
            own: &crate::state::WritingActors,
        ) -> OwnedDocumentPublication {
            if document.pending_ops() != 0 {
                return OwnedDocumentPublication::Unfinished;
            }
            let heads = super::sorted_heads(document.base_heads());
            if !needs_snapshot && sent.unchanged(&heads) {
                return OwnedDocumentPublication::Unchanged;
            }
            if blocked {
                return OwnedDocumentPublication::Blocked;
            }
            #[cfg(any(test, feature = "test-support"))]
            OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
            let mut document = document.clone();
            let mutation = if needs_snapshot {
                collab_types::DocumentMutation::Snapshot(document.save())
            } else {
                let delta = sent.delta(&mut document, own);
                if delta.is_empty() {
                    // What arrived is all the document gained, and the
                    // destination delivered it: it is missing nothing, and
                    // the frontier says so without another clone.
                    sent.advance(heads);
                    return OwnedDocumentPublication::Unchanged;
                }
                collab_types::DocumentMutation::Incremental(delta)
            };
            OwnedDocumentPublication::Changed {
                mutation,
                heads,
                document,
            }
        }

        let (store, checkout) = self.store_checkout();
        let own = store
            .writing_actors(&checkout.id)
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .read_document(outer, |document, _| {
                    inspect(document, sent, needs_snapshot, blocked, &own)
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Record {
                store,
                checkout,
                lifetime,
                ..
            } => {
                if !lifetime.upgrade().is_some_and(|life| life.active.get()) {
                    return Err(HistoryRegistrationError::UnsupportedOwner);
                }
                store
                    .open_existing_resource_checked(checkout)
                    .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
                store
                    .read_checked(&checkout.id, |document| {
                        inspect(document, sent, needs_snapshot, blocked, &own)
                    })
                    .map_err(|_| HistoryRegistrationError::UnsupportedOwner)
            }
        }
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn legacy_publication_rejects_same_id_with_stale_checkout_metadata() {
        let store = ResourceStore::in_memory();
        let live = DocumentCheckout::new(
            crate::DocumentId::for_tab(919),
            crate::resource::ResourceType::new("note"),
        );
        store.open_resource(&live);
        let stale = DocumentCheckout::new(
            live.id.clone(),
            crate::resource::ResourceType::new("different-kind"),
        );
        let lifetime = ResourceOwnerLifetime::new();
        let owner = HistoryOwner::record(store, stale, &lifetime, true);

        assert!(matches!(
            owner.prepare_publication(
                &AutoCommit::new(),
                &mut crate::state::DocumentFrontier::default(),
                true,
                false
            ),
            Err(HistoryRegistrationError::UnsupportedOwner)
        ));
    }
}

impl HistoryOwner {
    pub fn same_document(&self, other: &Self) -> bool {
        let (a, ac) = self.store_checkout();
        let (b, bc) = other.store_checkout();
        a.same_owner(b) && ac == bc
    }
    pub fn store_checkout(&self) -> (&ResourceStore, &DocumentCheckout) {
        match &self.0 {
            OwnerKind::Component(binding) => (&binding.records, &binding.checkout),
            OwnerKind::Record {
                store, checkout, ..
            } => (store, checkout),
        }
    }
    /// What the owner's record holds, as the checkout that opened it says. A host that must create the
    /// record on this device's first write needs the kind named here; a route joined without one can
    /// only be written where the record already exists.
    pub(crate) fn resource_type(&self) -> crate::resource::ResourceType {
        self.store_checkout().1.kind.clone()
    }

    pub fn broker_route(&self) -> Option<tilo_ui_base::collaboration::RouteSpec> {
        self.validate().ok()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding.broker_route(),
            OwnerKind::Record {
                store, checkout, ..
            } => store.route(checkout).ok(),
        }
    }
    /// Merge a peer's `mutation` into the record.
    pub fn merge_remote(
        &self,
        mut incoming: tilo_ui_base::collaboration::Incoming<'_>,
        outer: &mut AutoCommit,
    ) -> Result<super::routes::MergedRemote, super::routes::RemoteApplyError> {
        self.validate()
            .map_err(|error| super::routes::RemoteApplyError::Unavailable(error.to_string()))?;
        let (store, checkout) = self.store_checkout();
        let merged = crate::bound_dispatch::merge_remote_owned_document(
            store,
            checkout,
            incoming.reborrow(),
        )?;
        if merged.changed.is_none() && self.close_named_step(outer) {
            return crate::bound_dispatch::merge_remote_owned_document(store, checkout, incoming);
        }
        Ok(merged)
    }

    pub(crate) fn with_restore_authority<T>(
        &self,
        fallback_authority: &tilo_ui_base::permissions::PermissionContext,
        fallback_author: &tilo_ui_base::permissions::MemberId,
        operation: impl FnOnce(
            &tilo_ui_base::permissions::PermissionContext,
            &tilo_ui_base::permissions::MemberId,
        ) -> T,
    ) -> T {
        match &self.0 {
            OwnerKind::Component(binding) => {
                operation(&binding.permissions.borrow(), &binding.author)
            }
            OwnerKind::Record { .. } => operation(fallback_authority, fallback_author),
        }
    }

    /// Run the history verb `run` names on this owner's record, as one step of its history: a
    /// component's record decoded by the component's own registry, a record of its own taken as it is
    /// stored. Whether the record changed; why not, in words a person reads.
    pub(crate) fn run_history_edit(
        &self,
        root: &mut dyn tilo_ui_base::widget::Widget,
        outer: &mut AutoCommit,
        run: &HistoryRun<'_>,
    ) -> Result<bool, String> {
        self.validate().map_err(|_| NOT_RESTORED.to_owned())?;
        match &self.0 {
            OwnerKind::Component(binding) => binding.run_history_edit(root, outer, run),
            OwnerKind::Record {
                store, checkout, ..
            } => crate::bound_dispatch::run_record_history_edit(store, checkout, run),
        }
    }

    /// See [`crate::bound_dispatch::CallbackBinding::close_open_step`].
    pub(crate) fn close_open_step(&self, outer: &mut AutoCommit) {
        if let OwnerKind::Component(binding) = &self.0
            && let Err(error) = binding.close_open_step(outer)
        {
            tracing::debug!(?error, "history review could not close the open step");
        }
    }

    /// Whether this document cannot take a peer's changes yet, an unfinished edit or a restore in
    /// flight ([`crate::bound_dispatch::defers_remote`]).
    pub(crate) fn defers_remote(&self) -> bool {
        let (store, checkout) = self.store_checkout();
        crate::bound_dispatch::defers_remote(store, checkout)
    }

    /// See [`crate::bound_dispatch::CallbackBinding::close_named_step`]: whether a step an open action
    /// names closed.
    pub(crate) fn close_named_step(&self, outer: &mut AutoCommit) -> bool {
        match &self.0 {
            OwnerKind::Component(binding) => {
                binding.close_named_step(outer).unwrap_or_else(|error| {
                    tracing::debug!(?error, "a committed merge could not close the open step");
                    false
                })
            }
            OwnerKind::Record { .. } => false,
        }
    }

    pub(crate) fn schedule_committed_history(&self) -> Result<(), String> {
        let (store, checkout) = self.store_checkout();
        store.schedule_committed_history(&checkout.id)
    }
}

/// Why a history verb did not run: the record holds an edit not finished yet.
pub(crate) const FINISH_EDIT: &str = "Finish the current edit before restoring.";
/// Why a history verb did not run: the record changed since the review.
pub(crate) const STALE_REVIEW: &str = "Live content changed. Refresh review before restoring.";
/// Why a history verb did not run, when nothing more particular can be said.
pub(crate) const NOT_RESTORED: &str = "The document could not be restored safely.";

/// One of the store's history verbs ([`HistoryEdit`]) as History runs it on a record, reviewed
/// against the record's frontier `current`, by `author` under `context`.
pub(crate) struct HistoryRun<'a> {
    pub(crate) edit: HistoryEdit,
    pub(crate) context: tilo_actions::ActionContext,
    pub(crate) current: &'a [automerge::ChangeHash],
    pub(crate) author: &'a tilo_ui_base::permissions::MemberId,
}

impl HistoryRun<'_> {
    /// The verb as its invocation carries it.
    pub(crate) fn encoded(&self) -> tilo_actions::EncodedAction {
        crate::history_source::DocumentHistoryEdit::new(self.edit.clone()).encoded()
    }

    /// Why a record whose app does not decode the history verbs keeps its versions.
    pub(crate) fn unsupported() -> String {
        tilo_ui_base::i18n::I18nManager::lookup_plain("history-restore-unsupported")
    }

    /// Run `action`, the verb as the record's own types decode it, on `doc` through the executor
    /// as one step of `history`, written as `doc`'s actor. Whether it changed the document.
    pub(crate) fn step(
        &self,
        doc: &mut AutoCommit,
        history: &mut crate::state::UndoLog,
        action: &dyn tilo_actions::Action,
        instance: Option<collab_types::history::InstanceId>,
    ) -> Result<bool, String> {
        use collab_types::history::same_frontier;
        if doc.pending_ops() != 0 {
            return Err(FINISH_EDIT.to_owned());
        }
        if !same_frontier(&doc.base_heads(), self.current) {
            return Err(STALE_REVIEW.to_owned());
        }
        if !same_frontier(history.last_heads(), self.current) {
            return Err(FINISH_EDIT.to_owned());
        }
        if !history.can_bump() {
            return Err(NOT_RESTORED.to_owned());
        }
        let before = doc.get_heads();
        let writer = doc.get_actor().to_bytes().to_vec();
        tilo_actions::apply_local_with_context(
            doc,
            action,
            self.encoded(),
            Some(&writer),
            self.context.clone(),
        )
        .map_err(|error| match error {
            tilo_actions::ActionError::Refused(reason) => reason,
            error => {
                tracing::debug!(%error, "a history verb could not run");
                NOT_RESTORED.to_owned()
            }
        })?;
        let after = doc.get_heads();
        Ok(history.record_committed_span(
            doc,
            before,
            after,
            None,
            None,
            Some(self.author.clone()),
            instance,
        ))
    }
}

/// A stored record in a temporary profile.
#[cfg(test)]
pub(crate) struct ResourceOwnerFixture {
    _owners: crate::resource::ResourceOwners,
    pub(crate) store: ResourceStore,
    pub(crate) checkout: DocumentCheckout,
}

#[cfg(test)]
impl ResourceOwnerFixture {
    /// Record `id` of `so.tilo.controlled` in `workspace`, holding `value: 1` and `owner: id`.
    pub(crate) fn in_workspace(id: u64, workspace: &str) -> Self {
        let mut owners = crate::resource::ResourceOwners::writing(Rc::new(
            crate::resource::MemoryWorkspace::default(),
        ));
        let store = owners.open("so.tilo.controlled", workspace).unwrap();
        let checkout = DocumentCheckout::new(
            crate::DocumentId::for_tab(id),
            crate::resource::ResourceType::new("controlled"),
        );
        store
            .reserve_resource(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(automerge::ROOT, "value", 1)
                    .map_err(|e| e.to_string())?;
                doc.put(automerge::ROOT, "owner", id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        Self {
            _owners: owners,
            store,
            checkout,
        }
    }
}
