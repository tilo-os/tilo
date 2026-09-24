//! Opaque custody witnesses. These never enter historical presentation inputs.
use super::{HistoryRegistrationError, HistoryResource};
use crate::{
    bound_dispatch::CallbackBinding,
    record::{RecordCheckout, RecordStore},
};
use automerge::AutoCommit;
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

pub(crate) struct LegacyHistoryLifetime {
    pub(crate) active: Cell<bool>,
    id: uuid::Uuid,
}
impl LegacyHistoryLifetime {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            active: Cell::new(true),
            id: uuid::Uuid::new_v4(),
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
    Legacy {
        store: RecordStore,
        checkout: RecordCheckout,
        lifetime: Weak<LegacyHistoryLifetime>,
    },
}
impl HistoryOwner {
    pub(crate) fn component(binding: CallbackBinding) -> Self {
        Self(OwnerKind::Component(binding))
    }
    pub(crate) fn legacy(
        store: RecordStore,
        checkout: RecordCheckout,
        lifetime: &Rc<LegacyHistoryLifetime>,
    ) -> Self {
        Self(OwnerKind::Legacy {
            store,
            checkout,
            lifetime: Rc::downgrade(lifetime),
        })
    }
    pub(crate) fn lifetime_id(&self) -> Option<uuid::Uuid> {
        match &self.0 {
            OwnerKind::Component(binding) => Some(binding.lifetime_id),
            OwnerKind::Legacy { lifetime, .. } => lifetime
                .upgrade()
                .filter(|life| life.active.get())
                .map(|life| life.id),
        }
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (OwnerKind::Component(a), OwnerKind::Component(b)) => a.same_component(b),
            (
                OwnerKind::Legacy {
                    store: a,
                    checkout: ac,
                    lifetime: al,
                },
                OwnerKind::Legacy {
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
            OwnerKind::Legacy {
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
            OwnerKind::Legacy {
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
    pub(crate) fn document(
        &self,
        outer: &AutoCommit,
    ) -> Result<AutoCommit, HistoryRegistrationError> {
        self.validate()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding
                .read_document(outer, |doc, _| doc.clone())
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
            OwnerKind::Legacy {
                store, checkout, ..
            } => store
                .read_checked(&checkout.id, Clone::clone)
                .map_err(|_| HistoryRegistrationError::UnsupportedOwner),
        }
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
            OwnerKind::Legacy {
                store, checkout, ..
            } => (store, checkout),
        }
    }
    pub(crate) fn broker_route(&self) -> Option<crate::collaboration::RouteSpec> {
        self.validate().ok()?;
        match &self.0 {
            OwnerKind::Component(binding) => binding.broker_route(),
            OwnerKind::Legacy {
                store, checkout, ..
            } => {
                let (_, package, workspace) = store.history_namespace(&checkout.id).ok()?;
                if package.is_empty() {
                    return None;
                }
                Some(crate::collaboration::RouteSpec::record(
                    package,
                    uuid::Uuid::parse_str(&workspace).ok()?,
                    checkout.id.as_str(),
                ))
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
            OwnerKind::Legacy { .. } => operation(fallback_authority, fallback_author),
        }
    }

    pub(crate) fn restore_prepared(
        &self,
        outer: &mut AutoCommit,
        admitted: &crate::state::owned_restore::ConfirmedRestore<'_>,
        prepared: &crate::history_mode::restore::PreparedContentRestore,
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
            OwnerKind::Component(binding) => binding.restore_prepared(outer, admitted, prepared),
            OwnerKind::Legacy {
                store, checkout, ..
            } => crate::bound_dispatch::restore_legacy_history(store, checkout, admitted, prepared),
        }
    }

    pub(crate) fn schedule_committed_history(&self) {
        let (store, checkout) = self.store_checkout();
        store.schedule_committed_history(&checkout.id);
    }
}
