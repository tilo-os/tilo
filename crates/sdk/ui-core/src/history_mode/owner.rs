//! Opaque custody witnesses. These never enter historical presentation inputs.
use super::{HistoryRegistrationError, HistoryResource};
use crate::{
    bound_dispatch::CallbackBinding,
    record::{RecordCheckout, RecordStore},
};
use automerge::{AutoCommit, transaction::Transactable};
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

#[cfg(test)]
thread_local! {
    pub(crate) static OWNED_DOCUMENT_CLONES: Cell<usize> = const { Cell::new(0) };
}

pub(crate) enum OwnedDocumentPublication {
    Unchanged,
    Unfinished,
    Blocked,
    Changed {
        mutation: collab_types::DocumentMutation,
        authorization: Option<crate::invocation_scope::DocumentAuthorization>,
        heads: Vec<automerge::ChangeHash>,
    },
}

pub(crate) struct RecordOwnerLifetime {
    pub(crate) active: Cell<bool>,
    id: uuid::Uuid,
    pub(crate) control_domain: crate::ControlDomain,
}
impl RecordOwnerLifetime {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            active: Cell::new(true),
            id: uuid::Uuid::new_v4(),
            control_domain: crate::control_custody::ControlDomain::new(),
        })
    }
}

/// Captured by the actual mounted owner; callers cannot manufacture custody.
#[derive(Clone)]
pub struct HistoryOwner(pub(crate) OwnerKind);
#[derive(Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "avoid a heap allocation for each captured component custody witness"
)]
pub(crate) enum OwnerKind {
    Component(CallbackBinding),
    Record {
        store: RecordStore,
        checkout: RecordCheckout,
        lifetime: Weak<RecordOwnerLifetime>,
    },
}
impl HistoryOwner {
    pub(crate) fn check_invocation_lifetime(&self) -> Result<(), String> {
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .enter()
                .map(|_| ())
                .map_err(|error| format!("{error:?}")),
            OwnerKind::Record { lifetime, .. } => {
                if lifetime.upgrade().is_some_and(|life| life.active.get()) {
                    Ok(())
                } else {
                    Err("invocation record owner has retired".into())
                }
            }
        }
    }
    pub(crate) fn component(binding: CallbackBinding) -> Self {
        Self(OwnerKind::Component(binding))
    }
    /// The mounted component this owner is, if it is one.
    pub(crate) fn component_binding(&self) -> Option<&CallbackBinding> {
        match &self.0 {
            OwnerKind::Component(binding) => Some(binding),
            OwnerKind::Record { .. } => None,
        }
    }
    pub(crate) fn record(
        store: RecordStore,
        checkout: RecordCheckout,
        lifetime: &Rc<RecordOwnerLifetime>,
    ) -> Self {
        Self(OwnerKind::Record {
            store,
            checkout,
            lifetime: Rc::downgrade(lifetime),
        })
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
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (OwnerKind::Component(a), OwnerKind::Component(b)) => a.same_component(b),
            (
                OwnerKind::Record {
                    store: a,
                    checkout: ac,
                    lifetime: al,
                },
                OwnerKind::Record {
                    store: b,
                    checkout: bc,
                    lifetime: bl,
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
            } => {
                if !lifetime.upgrade().is_some_and(|life| life.active.get()) {
                    return Err(HistoryRegistrationError::UnsupportedOwner);
                }
                (store, checkout)
            }
        };
        store
            .open_existing_record_checked(checkout)
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
            } => (
                store,
                checkout,
                lifetime
                    .upgrade()
                    .ok_or(HistoryRegistrationError::UnsupportedOwner)?
                    .id,
            ),
        };
        let (storage, package, room) = store
            .history_namespace(&checkout.id)
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        if !package.is_empty() && package != app_id {
            return Err(HistoryRegistrationError::ForeignSource);
        }
        Ok(HistoryResource::new(
            storage.unwrap_or_else(|| format!("memory:{lifetime}")),
            if room.is_empty() {
                format!("lifetime:{lifetime}")
            } else {
                room
            },
            app_id,
            checkout.id.clone(),
        ))
    }
    /// Read the committed frontier through the same custody and record loan as `document`.
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
                    #[cfg(test)]
                    OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
                    doc.clone()
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Record {
                store, checkout, ..
            } => store
                .read_checked(&checkout.id, |doc| {
                    #[cfg(test)]
                    OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
                    doc.clone()
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
        }
    }

    pub(crate) fn prepare_publication(
        &self,
        outer: &AutoCommit,
        sent_heads: &[automerge::ChangeHash],
        needs_snapshot: bool,
        blocked: bool,
    ) -> Result<OwnedDocumentPublication, HistoryRegistrationError> {
        fn inspect(
            document: &AutoCommit,
            sent_heads: &[automerge::ChangeHash],
            needs_snapshot: bool,
            blocked: bool,
            provenance: &crate::invocation_scope::InvocationProvenance,
        ) -> OwnedDocumentPublication {
            if document.pending_ops() != 0 {
                return OwnedDocumentPublication::Unfinished;
            }
            let heads = super::sorted_heads(document.base_heads());
            if heads == sent_heads && !needs_snapshot {
                return OwnedDocumentPublication::Unchanged;
            }
            if blocked {
                return OwnedDocumentPublication::Blocked;
            }
            #[cfg(test)]
            OWNED_DOCUMENT_CLONES.set(OWNED_DOCUMENT_CLONES.get() + 1);
            let mut document = document.clone();
            let from = if needs_snapshot { &[][..] } else { sent_heads };
            let mutation = if needs_snapshot {
                collab_types::DocumentMutation::Snapshot(document.save())
            } else {
                collab_types::DocumentMutation::Incremental(document.save_after(from))
            };
            let Ok(authorization) = provenance.capture(&mut document, from, &mutation) else {
                return OwnedDocumentPublication::Blocked;
            };
            OwnedDocumentPublication::Changed {
                mutation,
                authorization,
                heads,
            }
        }

        let (store, checkout) = self.store_checkout();
        let provenance = store
            .invocation_provenance(&checkout.id)
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .read_document(outer, |document, _| {
                    inspect(
                        document,
                        sent_heads,
                        needs_snapshot,
                        blocked,
                        &provenance.borrow(),
                    )
                })
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Record {
                store,
                checkout,
                lifetime,
            } => {
                if !lifetime.upgrade().is_some_and(|life| life.active.get()) {
                    return Err(HistoryRegistrationError::UnsupportedOwner);
                }
                store
                    .open_existing_record_checked(checkout)
                    .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
                store
                    .read_checked(&checkout.id, |document| {
                        inspect(
                            document,
                            sent_heads,
                            needs_snapshot,
                            blocked,
                            &provenance.borrow(),
                        )
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
        let store = RecordStore::in_memory();
        let live = RecordCheckout::new(
            crate::RecordId::for_tab(919),
            crate::record::RecordKind::new("note"),
        );
        store.open_record(&live);
        let stale = RecordCheckout::new(
            live.id.clone(),
            crate::record::RecordKind::new("different-kind"),
        );
        let lifetime = RecordOwnerLifetime::new();
        let owner = HistoryOwner::record(store, stale, &lifetime);

        assert!(matches!(
            owner.prepare_publication(&AutoCommit::new(), &[], true, false),
            Err(HistoryRegistrationError::UnsupportedOwner)
        ));
    }
}

impl HistoryOwner {
    pub(crate) fn same_document(&self, other: &Self) -> bool {
        let (a, ac) = self.store_checkout();
        let (b, bc) = other.store_checkout();
        a.same_owner(b) && ac == bc
    }
    pub(crate) fn store_checkout(&self) -> (&RecordStore, &RecordCheckout) {
        match &self.0 {
            OwnerKind::Component(binding) => (&binding.records, &binding.checkout),
            OwnerKind::Record {
                store, checkout, ..
            } => (store, checkout),
        }
    }
    /// What the owner's record holds, as the checkout that opened it says.
    /// A host that must create the record on this device's first write needs
    /// the kind named here; a route joined without one can only be written
    /// where the record already exists.
    pub(crate) fn record_kind(&self) -> crate::record::RecordKind {
        self.store_checkout().1.kind.clone()
    }

    pub(crate) fn broker_route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.validate().ok()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding.broker_route(),
            OwnerKind::Record {
                store, checkout, ..
            } => {
                let (_, package, workspace) = store.history_namespace(&checkout.id).ok()?;
                if package.is_empty() {
                    return None;
                }
                if checkout.id.is_package_primary() {
                    Some(crate::collaboration::RouteSpec::app(
                        package,
                        uuid::Uuid::parse_str(&workspace).ok()?,
                    ))
                } else {
                    Some(crate::collaboration::RouteSpec::record(
                        package,
                        uuid::Uuid::parse_str(&workspace).ok()?,
                        checkout.id.as_str(),
                    ))
                }
            }
        }
    }
    pub(crate) fn merge_remote(
        &self,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<Option<bool>, super::routes::RemoteApplyError> {
        self.validate()
            .map_err(|error| super::routes::RemoteApplyError::Unavailable(error.to_string()))?;
        let (store, checkout) = self.store_checkout();
        crate::bound_dispatch::merge_remote_owned_document(store, checkout, mutation)
    }

    pub(crate) fn with_restore_authority<T>(
        &self,
        fallback_authority: &crate::permissions::PermissionContext,
        fallback_author: &crate::permissions::UserId,
        operation: impl FnOnce(&crate::permissions::PermissionContext, &crate::permissions::UserId) -> T,
    ) -> T {
        match &self.0 {
            OwnerKind::Component(binding) => {
                operation(&binding.permissions.borrow(), &binding.author)
            }
            OwnerKind::Record { .. } => operation(fallback_authority, fallback_author),
        }
    }

    pub(crate) fn restore_write(
        &self,
        outer: &mut AutoCommit,
        admitted: &crate::state::owned_restore::ConfirmedRestore<'_>,
        write: crate::state::owned_restore::RestoreWrite<'_>,
    ) -> Result<
        crate::state::owned_restore::OwnedActionReceipt,
        crate::state::owned_restore::OwnedRestoreError,
    > {
        self.validate().map_err(|_| {
            crate::state::owned_restore::OwnedRestoreError::Bound(
                crate::bound_dispatch::BoundDispatchError::ClosedLifetime,
            )
        })?;
        match &self.0 {
            OwnerKind::Component(binding) => binding.restore_write(outer, admitted, write),
            OwnerKind::Record {
                store, checkout, ..
            } => crate::bound_dispatch::restore_record_history(store, checkout, admitted, write),
        }
    }

    /// See [`crate::bound_dispatch::CallbackBinding::close_open_step`]. A
    /// record owner outside a component keeps no open step.
    pub(crate) fn close_open_step(&self, outer: &mut AutoCommit) {
        if let OwnerKind::Component(binding) = &self.0
            && let Err(error) = binding.close_open_step(outer)
        {
            tracing::debug!(?error, "history review could not close the open step");
        }
    }

    /// See [`crate::bound_dispatch::CallbackBinding::close_named_step`].
    /// Whether nothing unfinished is left open; a record owner outside a component keeps no step.
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

    /// # Errors
    /// The record's writer reports a storage failure.
    pub(crate) fn schedule_committed_history(&self) -> Result<(), String> {
        let (store, checkout) = self.store_checkout();
        store.schedule_committed_history(&checkout.id)
    }
}

impl Drop for RecordOwnerLifetime {
    fn drop(&mut self) {
        self.control_domain.close();
    }
}

/// A stored record in a temporary profile and the owner a test opens scopes
/// and History sources for.
#[cfg(test)]
pub(crate) struct RecordOwnerFixture {
    _temp: tempfile::TempDir,
    _owners: crate::record::RecordOwners,
    pub(crate) store: RecordStore,
    pub(crate) checkout: RecordCheckout,
    pub(crate) owner: HistoryOwner,
    _lifetime: Rc<RecordOwnerLifetime>,
}

#[cfg(test)]
impl RecordOwnerFixture {
    /// Record `id` of `so.tilo.controlled` in `room`, holding `value: 1` and
    /// `owner: id`, with invocation provenance required.
    pub(crate) fn in_room(id: u64, room: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().join("profile"));
        let store = owners.open("so.tilo.controlled", room).unwrap();
        let checkout = RecordCheckout::new(
            crate::RecordId::for_tab(id),
            crate::record::RecordKind::new("controlled"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(automerge::ROOT, "value", 1)
                    .map_err(|e| e.to_string())?;
                doc.put(automerge::ROOT, "owner", id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        store.require_invocation_provenance(&checkout).unwrap();
        let lifetime = RecordOwnerLifetime::new();
        let owner = HistoryOwner::record(store.clone(), checkout.clone(), &lifetime);
        Self {
            _temp: temp,
            _owners: owners,
            store,
            checkout,
            owner,
            _lifetime: lifetime,
        }
    }
}
