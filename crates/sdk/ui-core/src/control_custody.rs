//! Emission-time custody for registered local controls.
//!
//! Registration is deliberately separate from authority. Only the ordinary
//! action doors can authorize or execute a descriptor. Unregistered private
//! producers remain legacy-compatible and explicitly unqualified for History.
//!
//! Registered queues are sealed in installation preorder. Each producer keeps
//! its own FIFO or replacement discipline; there is no global temporal order.
//! Stable subtree slots preserve this order across ModelView/Tile generations.
//! Ordinary dispatch still uses the existing owner-specific canonical doors.
use crate::history_mode::pending_controls::PendingControlOrigin;
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    rc::{Rc, Weak},
};
use tilo_actions::ControlInvocation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlRequestId(uuid::Uuid, u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlProducerDiscipline {
    Fifo { limit: usize },
    ReplaceOne,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalEmission {
    Queued(ControlRequestId),
    Suppressed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitError {
    Closed,
    Exhausted,
}
/// Result of emitting through a retained raw replace-one slot.
///
/// `LegacyPending` is transitional pre-registration compatibility. Mounted
/// roots receive either an opaque queued identity or suppression and can never
/// recover a registered invocation through the compatibility take path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawControlEmission {
    LegacyPending,
    Queued(ControlRequestId),
    Suppressed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustodyAttachment {
    Initial,
    Dynamic,
    Renewal(ControlRenewal),
}
/// Issued only by an owner host after closing the exact previous incarnation.
/// It grants fresh producer installation, never transfer of pending descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlRenewal {
    previous: uuid::Uuid,
    replacement: uuid::Uuid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustodyError {
    ForeignOwner,
    UnsafeLateAttachment,
    Closed,
}
/// This is an inventory, never an assertion that arbitrary private storage was
/// observed. Legacy compatibility is retained until downstream migration ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlCoverage {
    pub registered: usize,
    pub legacy_compatibility_unqualified: bool,
    pub unsafe_late_attachments: usize,
}
#[derive(Clone)]
pub struct ControlDomain(Rc<RefCell<DomainState>>);
#[derive(Clone, Default)]
pub(crate) struct WeakControlDomain(Weak<RefCell<DomainState>>);
struct DomainState {
    id: uuid::Uuid,
    incarnation: uuid::Uuid,
    closed: bool,
    initial_installation: bool,
    next_request: u64,
    owner: Option<ControlOwner>,
    parent: WeakControlDomain,
    children: Vec<WeakControlDomain>,
    leases: BTreeSet<uuid::Uuid>,
    producers: Vec<Rc<RefCell<Producer>>>,
    order: Vec<DomainEntry>,
    coverage: ControlCoverage,
}
#[derive(Clone)]
pub(crate) enum ControlOwner {
    Outer(uuid::Uuid),
    Component(crate::bound_dispatch::CallbackBinding),
    Legacy(crate::history_mode::HistoryOwner),
}
impl ControlOwner {
    fn origin(&self, invocation: &ControlInvocation) -> PendingControlOrigin {
        match self {
            Self::Outer(root_lifetime) => PendingControlOrigin::Outer {
                root_lifetime: *root_lifetime,
            },
            Self::Component(binding) => {
                PendingControlOrigin::Component(crate::bound_dispatch::BoundActionTarget {
                    physical_text: false,
                    binding: binding.clone(),
                    address: crate::bound_dispatch::BoundActionAddress {
                        instance_id: binding.instance_id,
                        address: invocation.address.clone(),
                    },
                })
            }
            Self::Legacy(owner) => PendingControlOrigin::Legacy(
                crate::bound_dispatch::legacy_document::LegacyResourceTarget {
                    owner: owner.clone(),
                    address: invocation.address.clone(),
                },
            ),
        }
    }
}
#[derive(Clone)]
enum DomainEntry {
    Producer(Rc<RefCell<Producer>>),
    Child(WeakControlDomain),
}
struct Producer {
    discipline: ControlProducerDiscipline,
    pending: VecDeque<(ControlRequestId, ControlInvocation, bool)>,
}
/// A retained producer does not keep its owner, registry or widget alive.
#[derive(Clone)]
pub struct ControlOutbox {
    owner_incarnation: uuid::Uuid,
    domain: WeakControlDomain,
    producer: Rc<RefCell<Producer>>,
}
pub(crate) struct OwnedControl {
    pub(crate) origin: PendingControlOrigin,
    pub(crate) invocation: ControlInvocation,
    pub(crate) domain: WeakControlDomain,
}
/// Exact surface incarnation/generation. Removing one lease cannot remove any
/// overlapping surface's lease. New descendants observe ancestors immediately.
pub(crate) struct SuspensionLease {
    domain: WeakControlDomain,
    key: uuid::Uuid,
}
impl Drop for SuspensionLease {
    fn drop(&mut self) {
        if let Some(domain) = self.domain.upgrade() {
            domain.0.borrow_mut().leases.remove(&self.key);
        }
    }
}
impl WeakControlDomain {
    pub(crate) fn upgrade(&self) -> Option<ControlDomain> {
        self.0.upgrade().map(ControlDomain)
    }
    pub(crate) fn live(&self) -> bool {
        self.upgrade()
            .is_some_and(|d| !d.closed() && !d.suspended())
    }
}
impl ControlDomain {
    pub(crate) fn new() -> Self {
        let id = uuid::Uuid::new_v4();
        Self(Rc::new(RefCell::new(DomainState {
            id,
            incarnation: id,
            closed: false,
            initial_installation: true,
            next_request: 1,
            owner: None,
            parent: WeakControlDomain::default(),
            children: Vec::new(),
            leases: BTreeSet::new(),
            producers: Vec::new(),
            order: Vec::new(),
            coverage: ControlCoverage {
                legacy_compatibility_unqualified: true,
                ..ControlCoverage::default()
            },
        })))
    }
    pub(crate) fn finish_initial_installation(&self) {
        self.0.borrow_mut().initial_installation = false;
        for child in self.children() {
            child.finish_initial_installation();
        }
    }
    pub(crate) fn renewal_from(&self, previous: &Self) -> CustodyAttachment {
        assert!(previous.closed(), "renewal requires closed owner custody");
        CustodyAttachment::Renewal(ControlRenewal {
            previous: previous.0.borrow().incarnation,
            replacement: self.0.borrow().incarnation,
        })
    }
    pub(crate) fn permits_renewal(&self, previous: &Self, attachment: CustodyAttachment) -> bool {
        matches!(attachment, CustodyAttachment::Renewal(token) if previous.closed() && token.previous==previous.0.borrow().incarnation && token.replacement==self.0.borrow().incarnation)
    }
    pub(crate) fn outer() -> Self {
        let domain = Self::new();
        let id = domain.0.borrow().id;
        domain.set_owner(ControlOwner::Outer(id));
        domain
    }
    pub(crate) fn downgrade(&self) -> WeakControlDomain {
        WeakControlDomain(Rc::downgrade(&self.0))
    }
    pub(crate) fn same_owner_incarnation(&self, other: &Self) -> bool {
        self.0.borrow().incarnation == other.0.borrow().incarnation
    }
    pub(crate) fn inherit_owner(&self, parent: &Self) -> Result<(), CustodyError> {
        let parent = parent.0.borrow();
        let mut state = self.0.borrow_mut();
        if state.closed || parent.closed {
            return Err(CustodyError::Closed);
        }
        if state.incarnation == parent.incarnation {
            return Ok(());
        }
        // Ownership can be initialized before issuing any producer. Once
        // assigned, presentation reparenting cannot retarget that incarnation.
        if state.owner.is_some() || !state.producers.is_empty() {
            return Err(CustodyError::ForeignOwner);
        }
        state.owner.clone_from(&parent.owner);
        state.incarnation = parent.incarnation;
        Ok(())
    }
    pub(crate) fn set_owner(&self, owner: ControlOwner) {
        self.0.borrow_mut().owner = Some(owner);
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn closed(&self) -> bool {
        self.0.borrow().closed
    }
    pub(crate) fn suspended(&self) -> bool {
        let state = self.0.borrow();
        !state.leases.is_empty()
            || state
                .parent
                .upgrade()
                .is_some_and(|parent| parent.suspended())
    }
    pub(crate) fn attach_to(&self, parent: &Self) -> Result<(), CustodyError> {
        if self.same(parent) {
            return Ok(());
        }
        if self.closed() || parent.closed() {
            return Err(CustodyError::Closed);
        }
        let mut ancestor = Some(parent.clone());
        while let Some(current) = ancestor {
            if self.same(&current) {
                return Err(CustodyError::ForeignOwner);
            }
            ancestor = current.0.borrow().parent.upgrade();
        }
        if let Some(old) = self.0.borrow().parent.upgrade() {
            if old.same(parent) {
                return Ok(());
            }
            // Reparenting is a host transition, never inferred from an address.
            old.0
                .borrow_mut()
                .children
                .retain(|child| child.upgrade().is_some_and(|child| !child.same(self)));
            old.0.borrow_mut().order.retain(|entry| match entry {
                DomainEntry::Child(child) => child.upgrade().is_some_and(|child| !child.same(self)),
                _ => true,
            });
        }
        self.0.borrow_mut().parent = parent.downgrade();
        parent.0.borrow_mut().children.push(self.downgrade());
        parent
            .0
            .borrow_mut()
            .order
            .push(DomainEntry::Child(self.downgrade()));
        if self.suspended() {
            self.clear_live();
        }
        Ok(())
    }
    pub(crate) fn child(&self) -> Self {
        let child = Self::new();
        if child
            .inherit_owner(self)
            .and_then(|()| child.attach_to(self))
            .is_err()
        {
            child.close();
        }
        child
    }
    pub fn producer(&self, discipline: ControlProducerDiscipline) -> ControlOutbox {
        let mut state = self.0.borrow_mut();
        let producer = Rc::new(RefCell::new(Producer {
            discipline,
            pending: VecDeque::new(),
        }));
        state.producers.push(producer.clone());
        state.order.push(DomainEntry::Producer(producer.clone()));
        state.coverage.registered += 1;
        ControlOutbox {
            owner_incarnation: state.incarnation,
            domain: self.downgrade(),
            producer,
        }
    }
    pub fn coverage(&self) -> ControlCoverage {
        let mut result = self.0.borrow().coverage.clone();
        for child in self.children() {
            let coverage = child.coverage();
            result.registered += coverage.registered;
            result.unsafe_late_attachments += coverage.unsafe_late_attachments;
            result.legacy_compatibility_unqualified |= coverage.legacy_compatibility_unqualified;
        }
        result
    }
    pub(crate) fn unsafe_attachment(&self) {
        self.0.borrow_mut().coverage.unsafe_late_attachments += 1;
    }
    fn children(&self) -> Vec<Self> {
        self.0
            .borrow()
            .children
            .iter()
            .filter_map(WeakControlDomain::upgrade)
            .collect()
    }
    pub(crate) fn close(&self) {
        self.0.borrow_mut().closed = true;
        self.clear_live();
        for child in self.children() {
            child.close();
        }
        let parent = {
            let mut state = self.0.borrow_mut();
            state.leases.clear();
            std::mem::take(&mut state.parent).upgrade()
        };
        if let Some(parent) = parent {
            let mut state = parent.0.borrow_mut();
            state
                .children
                .retain(|child| child.upgrade().is_some_and(|child| !child.same(self)));
            state.order.retain(|entry| match entry {
                DomainEntry::Child(child) => child.upgrade().is_some_and(|child| !child.same(self)),
                DomainEntry::Producer(_) => true,
            });
        }
    }
    fn clear_live(&self) {
        for producer in &self.0.borrow().producers {
            producer.borrow_mut().pending.clear();
        }
        for child in self.children() {
            child.clear_live();
        }
    }
    fn detach_pending(&self, output: &mut Vec<OwnedControl>) {
        if self.closed() || self.suspended() {
            return;
        }
        let (owner, order) = {
            let state = self.0.borrow();
            (state.owner.clone(), state.order.clone())
        };
        for entry in order {
            match entry {
                DomainEntry::Producer(producer) => {
                    if let Some(owner) = &owner {
                        for (_, invocation, physical_text) in
                            producer.borrow_mut().pending.drain(..)
                        {
                            let mut origin = owner.origin(&invocation);
                            if let PendingControlOrigin::Component(target) = &mut origin {
                                target.physical_text = physical_text;
                            }
                            output.push(OwnedControl {
                                origin,
                                invocation,
                                domain: self.downgrade(),
                            });
                        }
                    }
                }
                DomainEntry::Child(child) => {
                    if let Some(child) = child.upgrade() {
                        child.detach_pending(output);
                    }
                }
            }
        }
    }

    pub(crate) fn drain_live(&self) -> Vec<OwnedControl> {
        let mut output = Vec::new();
        self.detach_pending(&mut output);
        output
    }
    pub(crate) fn suspend(&self) -> (SuspensionLease, Vec<OwnedControl>) {
        let pending = self.drain_live();
        let key = uuid::Uuid::new_v4();
        self.0.borrow_mut().leases.insert(key);
        (
            SuspensionLease {
                domain: self.downgrade(),
                key,
            },
            pending,
        )
    }
}
impl ControlOutbox {
    pub fn emit(&self, invocation: ControlInvocation) -> Result<LocalEmission, EmitError> {
        let domain = self.domain.upgrade().ok_or(EmitError::Closed)?;
        if domain.closed() || domain.0.borrow().incarnation != self.owner_incarnation {
            return Err(EmitError::Closed);
        }
        if domain.suspended() {
            return Ok(LocalEmission::Suppressed);
        }
        let mut state = domain.0.borrow_mut();
        let id = ControlRequestId(state.id, state.next_request);
        state.next_request = state
            .next_request
            .checked_add(1)
            .ok_or(EmitError::Exhausted)?;
        let mut producer = self.producer.borrow_mut();
        match producer.discipline {
            ControlProducerDiscipline::ReplaceOne => producer.pending.clear(),
            ControlProducerDiscipline::Fifo { limit } => {
                if limit == 0 {
                    return Ok(LocalEmission::Suppressed);
                }
                if producer.pending.len() >= limit {
                    producer.pending.pop_front();
                }
            }
        }
        producer
            .pending
            .push_back((id, invocation, crate::bound_dispatch::physical_text()));
        Ok(LocalEmission::Queued(id))
    }
    pub fn replace(&self, invocation: ControlInvocation) -> Result<LocalEmission, EmitError> {
        self.clear();
        self.emit(invocation)
    }
    pub fn clear(&self) {
        self.producer.borrow_mut().pending.clear();
    }
    fn belongs_to(&self, domain: &ControlDomain) -> bool {
        self.domain.upgrade().is_some_and(|old| old.same(domain))
    }
}
/// Storage shared by the nine emitters and the palette's replace-one slot.
/// Automatic private taking survives *only* in this unqualified migration mode.
pub(crate) struct ControlHolder {
    discipline: ControlProducerDiscipline,
    storage: HolderStorage,
}
enum HolderStorage {
    Legacy {
        pending: VecDeque<(ControlInvocation, bool)>,
        emitted: bool,
    },
    Registered(ControlOutbox),
}
impl ControlHolder {
    pub(crate) fn registered(
        domain: &ControlDomain,
        discipline: ControlProducerDiscipline,
    ) -> Self {
        Self {
            discipline,
            storage: HolderStorage::Registered(domain.producer(discipline)),
        }
    }
    pub(crate) fn new(discipline: ControlProducerDiscipline) -> Self {
        Self {
            discipline,
            storage: HolderStorage::Legacy {
                pending: VecDeque::new(),
                emitted: false,
            },
        }
    }
    pub(crate) fn install(
        &mut self,
        domain: &ControlDomain,
        attachment: CustodyAttachment,
    ) -> Result<(), CustodyError> {
        if domain.closed() {
            return Err(CustodyError::Closed);
        }
        if let HolderStorage::Registered(outbox) = &self.storage {
            if outbox.belongs_to(domain) {
                return Ok(());
            }
            let permitted = if let CustodyAttachment::Renewal(token) = attachment {
                token.replacement == domain.0.borrow().incarnation
                    && outbox.owner_incarnation == token.previous
                    && outbox
                        .domain
                        .upgrade()
                        .is_none_or(|previous| previous.closed())
            } else {
                false
            };
            if !permitted {
                domain.unsafe_attachment();
                return Err(CustodyError::ForeignOwner);
            }
            if domain.closed() {
                return Err(CustodyError::Closed);
            }
            self.storage = HolderStorage::Registered(domain.producer(self.discipline));
            return Ok(());
        }
        if domain.closed() {
            return Err(CustodyError::Closed);
        }
        if let HolderStorage::Legacy { emitted: true, .. } = &self.storage {
            if attachment != CustodyAttachment::Initial || !domain.0.borrow().initial_installation {
                domain.unsafe_attachment();
                return Err(CustodyError::UnsafeLateAttachment);
            }
        }
        let outbox = domain.producer(self.discipline);
        let old = std::mem::replace(&mut self.storage, HolderStorage::Registered(outbox.clone()));
        if let HolderStorage::Legacy { pending, .. } = old {
            for (invocation, physical_text) in pending {
                let _ = crate::bound_dispatch::with_physical_text(physical_text, || {
                    outbox.emit(invocation)
                });
            }
        }
        Ok(())
    }
    pub(crate) fn emit(&mut self, invocation: ControlInvocation) {
        let _ = self.emit_detailed(invocation);
    }
    fn emit_detailed(
        &mut self,
        invocation: ControlInvocation,
    ) -> Result<RawControlEmission, EmitError> {
        match &mut self.storage {
            HolderStorage::Registered(outbox) => {
                outbox.emit(invocation).map(|emission| match emission {
                    LocalEmission::Queued(id) => RawControlEmission::Queued(id),
                    LocalEmission::Suppressed => RawControlEmission::Suppressed,
                })
            }
            HolderStorage::Legacy { pending, emitted } => {
                *emitted = true;
                match self.discipline {
                    ControlProducerDiscipline::ReplaceOne => pending.clear(),
                    ControlProducerDiscipline::Fifo { limit } => {
                        if limit == 0 {
                            return Ok(RawControlEmission::Suppressed);
                        }
                        if pending.len() >= limit {
                            pending.pop_front();
                        }
                    }
                }
                pending.push_back((invocation, crate::bound_dispatch::physical_text()));
                Ok(RawControlEmission::LegacyPending)
            }
        }
    }
    pub(crate) fn take(&mut self) -> Option<ControlInvocation> {
        match &mut self.storage {
            HolderStorage::Legacy { pending, .. } => {
                pending.pop_front().map(|(invocation, physical_text)| {
                    crate::bound_dispatch::note_drained_physical_text(physical_text);
                    invocation
                })
            }
            HolderStorage::Registered(_) => None,
        }
    }
    pub(crate) fn clear(&mut self) {
        match &mut self.storage {
            HolderStorage::Legacy { pending, .. } => pending.clear(),
            HolderStorage::Registered(outbox) => outbox.clear(),
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    fn controlled_pending_for_test(&self) -> Option<ControlInvocation> {
        match &self.storage {
            HolderStorage::Legacy { pending, .. } => {
                pending.front().map(|(invocation, _)| invocation.clone())
            }
            HolderStorage::Registered(outbox) => outbox
                .producer
                .borrow()
                .pending
                .front()
                .map(|(_, invocation, _)| invocation.clone()),
        }
    }
}

/// Retained raw descriptor custody for a single last-write-wins producer.
///
/// The legacy buffer exists only until a mounted owner installs custody. Once
/// registered, this slot delegates directly to the owner's private producer
/// and never exposes registered descriptors through compatibility taking.
pub struct RawControlSlot {
    holder: ControlHolder,
}

impl RawControlSlot {
    pub fn replace_one() -> Self {
        Self {
            holder: ControlHolder::new(ControlProducerDiscipline::ReplaceOne),
        }
    }

    pub fn replace_one_in(domain: &ControlDomain) -> Self {
        Self {
            holder: ControlHolder::registered(domain, ControlProducerDiscipline::ReplaceOne),
        }
    }

    /// Bind this retained producer to a mounted owner.
    ///
    /// # Errors
    /// Refuses closed, foreign, or unsafe late owner transitions.
    pub fn install_custody(
        &mut self,
        domain: &ControlDomain,
        attachment: CustodyAttachment,
    ) -> Result<(), CustodyError> {
        self.holder.install(domain, attachment)
    }

    /// Own a complete invocation at emission time.
    ///
    /// # Errors
    /// Returns the registered producer's closed or identity-exhaustion error.
    pub fn emit(&mut self, invocation: ControlInvocation) -> Result<RawControlEmission, EmitError> {
        self.holder.emit_detailed(invocation)
    }

    pub fn clear(&mut self) {
        self.holder.clear();
    }

    /// Transitional direct-fixture compatibility before registration.
    pub fn take_legacy_compatibility(&mut self) -> Option<ControlInvocation> {
        self.holder.take()
    }

    /// Inspect the retained descriptor in deterministic controlled fixtures.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn controlled_pending_for_test(&self) -> Option<ControlInvocation> {
        self.holder.controlled_pending_for_test()
    }
}
#[derive(Default)]
pub(crate) struct RegisteredControls {
    pub(crate) outer: VecDeque<ControlInvocation>,
    pub(crate) components: Vec<(crate::bound_dispatch::BoundActionTarget, ControlInvocation)>,
    pub(crate) legacy: Vec<(
        crate::bound_dispatch::legacy_document::LegacyResourceTarget,
        ControlInvocation,
    )>,
}
impl ControlDomain {
    pub(crate) fn drain_partitioned(&self) -> RegisteredControls {
        let mut result = RegisteredControls::default();
        for control in self.drain_live() {
            match control.origin {
                PendingControlOrigin::Outer { .. } => result.outer.push_back(control.invocation),
                PendingControlOrigin::Component(target) => {
                    result.components.push((target, control.invocation))
                }
                PendingControlOrigin::Legacy(target) => {
                    result.legacy.push((target, control.invocation))
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{ActionEmitter, Widget};
    fn invocation(value: bool) -> ControlInvocation {
        crate::widget::test_bool_action_binding()
            .invoke(value)
            .unwrap()
    }
    fn unit_binding() -> tilo_actions::ActionBinding<()> {
        let spec = tilo_actions::ActionSpec::typed_operation(
            "run",
            "Run",
            "Run",
            tilo_actions::OperationKind::Effect,
            "fixture.run",
            vec![],
            |_| tilo_actions::EncodedAction::encode_operation("fixture.run", &()).unwrap(),
        );
        let resource =
            tilo_actions::Resource::new("fixture", "fixture", "Fixture").with_action(spec.clone());
        tilo_actions::ActionBinding::new(&resource, &spec, |()| serde_json::Map::new()).unwrap()
    }
    fn raw_request(resource: &str, action: &str, marker: &str) -> ControlInvocation {
        let mut arguments = serde_json::Map::new();
        arguments.insert("marker".into(), serde_json::Value::String(marker.into()));
        ControlInvocation {
            address: tilo_actions::ActionAddress::new(resource, action),
            arguments,
        }
    }
    #[test]
    fn control_custody_raw_holder_legacy_replaces_copies_and_clears() {
        let mut slot = RawControlSlot::replace_one();
        let expected_b = raw_request("resource-b", "apply", "copied-b");

        assert_eq!(
            slot.emit(raw_request("resource-a", "apply", "copied-a")),
            Ok(RawControlEmission::LegacyPending)
        );
        assert_eq!(
            slot.emit(expected_b.clone()),
            Ok(RawControlEmission::LegacyPending)
        );
        assert_eq!(slot.take_legacy_compatibility(), Some(expected_b));
        assert_eq!(slot.take_legacy_compatibility(), None);

        slot.emit(raw_request("resource-c", "clear", "copied-c"))
            .unwrap();
        slot.clear();
        assert_eq!(slot.take_legacy_compatibility(), None);
    }
    #[test]
    fn control_custody_raw_holder_initial_transfer_is_once_and_registered_never_takes() {
        let domain = ControlDomain::outer();
        let expected = raw_request("initial", "apply", "before-install");
        let mut slot = RawControlSlot::replace_one();
        slot.emit(expected.clone()).unwrap();

        slot.install_custody(&domain, CustodyAttachment::Initial)
            .unwrap();
        slot.install_custody(&domain, CustodyAttachment::Dynamic)
            .unwrap();
        assert_eq!(slot.take_legacy_compatibility(), None);
        let drained = domain.drain_live();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].invocation, expected);
        assert!(
            domain.drain_live().is_empty(),
            "initial value transfers once"
        );
    }
    #[test]
    fn control_custody_raw_holder_suspension_closed_foreign_and_late_refuse() {
        let domain = ControlDomain::outer();
        let mut slot = RawControlSlot::replace_one_in(&domain);
        let (lease, saved) = domain.suspend();
        assert!(saved.is_empty());
        assert_eq!(
            slot.emit(raw_request("hidden", "apply", "suppressed")),
            Ok(RawControlEmission::Suppressed)
        );
        drop(lease);
        assert!(domain.drain_live().is_empty());

        let foreign = ControlDomain::outer();
        assert_eq!(
            slot.install_custody(&foreign, CustodyAttachment::Initial),
            Err(CustodyError::ForeignOwner)
        );
        assert_eq!(foreign.coverage().unsafe_late_attachments, 1);

        foreign.finish_initial_installation();
        let mut late = RawControlSlot::replace_one();
        late.emit(raw_request("late", "apply", "retained")).unwrap();
        assert_eq!(
            late.install_custody(&foreign, CustodyAttachment::Dynamic),
            Err(CustodyError::UnsafeLateAttachment)
        );
        assert_eq!(foreign.coverage().unsafe_late_attachments, 2);

        domain.close();
        assert_eq!(
            slot.emit(raw_request("closed", "apply", "refused")),
            Err(EmitError::Closed)
        );
        assert_eq!(slot.take_legacy_compatibility(), None);
        assert_eq!(
            slot.install_custody(&domain, CustodyAttachment::Initial),
            Err(CustodyError::Closed)
        );
    }
    #[test]
    fn control_custody_raw_holder_exact_renewal_transfers_no_old_output_and_accepts_fresh() {
        let previous = ControlDomain::outer();
        let mut slot = RawControlSlot::replace_one_in(&previous);
        slot.emit(raw_request("old", "apply", "must-not-transfer"))
            .unwrap();
        previous.close();

        let replacement = ControlDomain::outer();
        let renewal = replacement.renewal_from(&previous);
        slot.install_custody(&replacement, renewal).unwrap();
        assert!(replacement.drain_live().is_empty());
        assert_eq!(slot.take_legacy_compatibility(), None);
        assert!(matches!(
            slot.emit(raw_request("fresh", "apply", "fresh-only")),
            Ok(RawControlEmission::Queued(_))
        ));
        let drained = replacement.drain_live();
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].invocation,
            raw_request("fresh", "apply", "fresh-only")
        );

        let wrong_replacement = ControlDomain::outer();
        assert_eq!(
            slot.install_custody(&wrong_replacement, renewal),
            Err(CustodyError::ForeignOwner)
        );
        assert_eq!(wrong_replacement.coverage().unsafe_late_attachments, 1);
    }
    #[test]
    fn control_custody_core_registration_is_single_custody_and_late_attach_is_unqualified() {
        let domain = ControlDomain::outer();
        let mut initial = ActionEmitter::new(crate::widget::test_bool_action_binding());
        initial.emit(true).unwrap();
        initial
            .install_custody(&domain, CustodyAttachment::Initial)
            .unwrap();
        assert!(initial.take().is_none());
        let (lease, saved) = domain.suspend();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].invocation, invocation(true));
        initial.emit(false).unwrap();
        initial.refresh_availability(&Default::default());
        assert_eq!(saved[0].invocation, invocation(true));
        drop(lease);
        assert!(domain.drain_live().is_empty());
        let mut late = ActionEmitter::new(crate::widget::test_bool_action_binding());
        late.emit(false).unwrap();
        assert!(late.take().is_some());
        assert_eq!(
            late.install_custody(&domain, CustodyAttachment::Dynamic),
            Err(CustodyError::UnsafeLateAttachment)
        );
        assert_eq!(domain.coverage().unsafe_late_attachments, 1);
        assert!(domain.coverage().legacy_compatibility_unqualified);
        domain.finish_initial_installation();
        let mut falsely_initial = ActionEmitter::new(crate::widget::test_bool_action_binding());
        falsely_initial.emit(true).unwrap();
        assert_eq!(
            falsely_initial.install_custody(&domain, CustodyAttachment::Initial),
            Err(CustodyError::UnsafeLateAttachment)
        );
        let foreign = ControlDomain::outer();
        assert_eq!(
            initial.install_custody(&foreign, CustodyAttachment::Initial),
            Err(CustodyError::ForeignOwner)
        );
        domain.close();
        assert_eq!(
            initial.install_custody(&domain, CustodyAttachment::Initial),
            Err(CustodyError::Closed)
        );
        initial.emit(true).unwrap();
        assert!(initial.take().is_none());
        let mut closed = ActionEmitter::new_in(crate::widget::test_bool_action_binding(), &domain);
        closed.emit(true).unwrap();
        assert!(closed.take().is_none());
    }
    #[test]
    fn control_custody_core_nested_leases_close_and_replacement() {
        for outer_first in [false, true] {
            let outer = ControlDomain::outer();
            let child = outer.child();
            let outbox = child.producer(ControlProducerDiscipline::Fifo { limit: 64 });
            outbox.emit(invocation(true)).unwrap();
            let (a, saved) = outer.suspend();
            let (b, duplicate) = child.suspend();
            assert_eq!(saved.len(), 1);
            assert!(duplicate.is_empty());
            let remaining = if outer_first {
                drop(a);
                b
            } else {
                drop(b);
                a
            };
            assert_eq!(
                outbox.emit(invocation(false)),
                Ok(LocalEmission::Suppressed)
            );
            drop(remaining);
            assert!(matches!(
                outbox.emit(invocation(false)),
                Ok(LocalEmission::Queued(_))
            ));
            assert_eq!(outer.drain_live().len(), 1);
            child.close();
            assert_eq!(outbox.emit(invocation(true)), Err(EmitError::Closed));
            let replacement = outer.child();
            let fresh = replacement.producer(ControlProducerDiscipline::ReplaceOne);
            fresh.emit(invocation(false)).unwrap();
            assert_eq!(outer.drain_live().len(), 1);
            outer.close();
            let illegal_child = outer.child();
            assert_eq!(
                illegal_child
                    .producer(ControlProducerDiscipline::ReplaceOne)
                    .emit(invocation(true)),
                Err(EmitError::Closed)
            );
        }
    }
    #[test]
    fn control_custody_core_legacy_clients_preserve_live_behavior_without_acceptance() {
        let mut legacy = ActionEmitter::new(crate::widget::test_bool_action_binding());
        legacy.emit(true).unwrap();
        assert_eq!(legacy.take(), Some(invocation(true)));
        assert!(legacy.take().is_none());
        assert!(
            ControlDomain::outer()
                .coverage()
                .legacy_compatibility_unqualified
        );
    }
    #[test]
    fn control_custody_core_order_keeps_interleaved_subtrees_and_replace_one() {
        let outer = ControlDomain::outer();
        let a = outer.producer(ControlProducerDiscipline::Fifo { limit: 2 });
        let inner = outer.child();
        let b = inner.producer(ControlProducerDiscipline::ReplaceOne);
        let c = outer.producer(ControlProducerDiscipline::Fifo { limit: 2 });
        for (box_, label) in [(&a, "A"), (&b, "old B"), (&b, "B"), (&c, "C")] {
            let mut request = invocation(true);
            request.address.resource = label.into();
            box_.emit(request).unwrap();
        }
        let (_lease, saved) = outer.suspend();
        assert_eq!(
            saved
                .iter()
                .map(|request| request.invocation.address.resource.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        let mut items = crate::history_mode::pending_controls::SuspendedLocalControls::default();
        items.begin_history_entry();
        for item in saved {
            items.capture_registered(item, Err("ordering-only witness".into()));
        }
        items.return_to_live();
        let first = items.review();
        items.begin_history_entry();
        items.return_to_live();
        assert_eq!(
            items.review(),
            first,
            "sealed descriptors keep their order across reentry/return"
        );
    }
    #[test]
    fn control_custody_core_dynamic_modelview_inherits_suspension_before_emit() {
        let domain = ControlDomain::outer();
        let retained = Rc::new(RefCell::new(None));
        let output = retained.clone();
        let mut view = crate::widget::ModelView::from_document_in(
            &domain,
            |_, _| 1,
            move |_, domain| {
                let producer = domain.producer(ControlProducerDiscipline::ReplaceOne);
                assert_eq!(
                    producer.emit(invocation(true)),
                    Ok(LocalEmission::Suppressed)
                );
                *output.borrow_mut() = Some(producer);
                Box::new(crate::widget::Button::new("passive", "Passive").on_action(unit_binding()))
            },
        );
        crate::widget::controls::install_control_custody(
            &mut view,
            &domain,
            CustodyAttachment::Initial,
        );
        let (lease, saved) = domain.suspend();
        assert!(saved.is_empty());
        let mut doc = automerge::AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        drop(lease);
        assert!(domain.drain_live().is_empty());
        assert!(view.children_mut()[0].perform_action(
            &crate::a11y::Action::Activate,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        let requests = domain.drain_live();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].invocation.arguments.is_empty());
        assert!(matches!(
            retained.borrow().as_ref().unwrap().emit(invocation(false)),
            Ok(LocalEmission::Queued(_))
        ));
        assert_eq!(domain.drain_live().len(), 1);
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::widget::Widget;
    fn request(label: &str) -> ControlInvocation {
        ControlInvocation {
            address: tilo_actions::ActionAddress::new(label, "run"),
            arguments: serde_json::Map::new(),
        }
    }
    #[test]
    fn control_custody_core_actual_modelview_replacement_preserves_installed_order() {
        let domain = ControlDomain::outer();
        let key = Rc::new(RefCell::new(0));
        let projection = key.clone();
        let a = domain.producer(ControlProducerDiscipline::ReplaceOne);
        let mut model = crate::widget::ModelView::from_document_in(
            &domain,
            move |_, _| *projection.borrow(),
            |key, domain| {
                let outbox = domain.producer(ControlProducerDiscipline::ReplaceOne);
                outbox
                    .emit(request(if *key == 0 { "B0" } else { "B1" }))
                    .unwrap();
                Box::new(crate::widget::Text::new("passive"))
            },
        );
        crate::widget::controls::install_control_custody(
            &mut model,
            &domain,
            CustodyAttachment::Initial,
        );
        let c = domain.producer(ControlProducerDiscipline::ReplaceOne);
        let doc = automerge::AutoCommit::new();
        for revision in 0..128 {
            let want = if revision == 0 { "B0" } else { "B1" };
            *key.borrow_mut() = revision;
            model.sync(&doc, &automerge::ROOT);
            let scope = domain.children().into_iter().next().unwrap();
            assert_eq!(domain.0.borrow().order.len(), 3, "stable A/subtree/C slots");
            assert_eq!(
                scope.0.borrow().order.len(),
                1,
                "closed generations unlink from installed scope"
            );
            assert_eq!(scope.children().len(), 1);
            a.emit(request("A")).unwrap();
            c.emit(request("C")).unwrap();
            let (lease, saved) = domain.suspend();
            assert_eq!(
                saved
                    .iter()
                    .map(|item| item.invocation.address.resource.as_str())
                    .collect::<Vec<_>>(),
                ["A", want, "C"]
            );
            drop(lease);
        }
    }
    #[test]
    fn control_custody_core_continuation_waits_for_every_covering_surface() {
        let root = ControlDomain::outer();
        let child = root.child();
        let producer = child.producer(ControlProducerDiscipline::ReplaceOne);
        producer.emit(request("saved")).unwrap();
        let (inner, saved) = child.suspend();
        let (outer, duplicate) = root.suspend();
        assert!(duplicate.is_empty());
        let mut pending = crate::history_mode::pending_controls::SuspendedLocalControls::default();
        pending.begin_history_entry();
        for item in saved {
            pending.capture_registered(
                item,
                Err("fixture witness is intentionally not authority".into()),
            );
        }
        drop(inner);
        pending.return_to_live();
        let id = pending.review()[0].id;
        let token = pending.token(id).unwrap();
        assert!(
            pending.prepare(&token, false).is_none(),
            "outer covering lease blocks saved Continue too"
        );
        drop(outer);
        assert!(
            pending.prepare(&token, false).is_some(),
            "blocked token was not consumed"
        );
    }
}
