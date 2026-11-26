//! Emission-time ownership for registered local controls. Registration is deliberately separate from
//! authority.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainAttachment {
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
pub enum AttachError {
    ForeignOwner,
    UnsafeLateAttachment,
    Closed,
}
/// This is an inventory, never an assertion that arbitrary private storage was
/// observed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlCoverage {
    pub registered: usize,
    pub unsafe_late_attachments: usize,
}
#[derive(Clone)]
pub struct ControlDomain(Rc<RefCell<DomainState>>);
#[derive(Clone, Default)]
pub struct WeakControlDomain(Weak<RefCell<DomainState>>);
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
/// Who registered a control a domain holds: the window's own root, or one of its components.
#[derive(Clone)]
pub enum PendingControlOrigin {
    Outer {
        root_lifetime: uuid::Uuid,
    },
    #[cfg(feature = "model")]
    Component(ComponentControl),
}

/// A control one of the window's components registered: the component, the action it addresses,
/// and the input that produced it.
#[cfg(feature = "model")]
#[derive(Clone)]
pub struct ComponentControl {
    pub input: crate::widget::input_provenance::Input,
    pub binding: crate::widget::component_walk::Binding,
    pub address: tilo_actions::ActionAddress,
}

#[derive(Clone)]
pub enum ControlOwner {
    Outer(uuid::Uuid),
    #[cfg(feature = "model")]
    Component(crate::widget::component_walk::Binding),
}
impl ControlOwner {
    fn origin(&self, invocation: &ControlInvocation) -> PendingControlOrigin {
        match self {
            Self::Outer(root_lifetime) => PendingControlOrigin::Outer {
                root_lifetime: *root_lifetime,
            },
            #[cfg(feature = "model")]
            Self::Component(binding) => PendingControlOrigin::Component(ComponentControl {
                input: crate::widget::input_provenance::Input::Discrete,
                binding: std::rc::Rc::clone(binding),
                address: invocation.address.clone(),
            }),
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
    pending: VecDeque<(
        ControlRequestId,
        ControlInvocation,
        crate::widget::input_provenance::Input,
    )>,
}
/// A retained producer does not keep its owner, registry or widget alive.
#[derive(Clone)]
pub struct ControlOutbox {
    owner_incarnation: uuid::Uuid,
    domain: WeakControlDomain,
    producer: Rc<RefCell<Producer>>,
}
pub struct OwnedControl {
    pub origin: PendingControlOrigin,
    pub invocation: ControlInvocation,
    pub domain: WeakControlDomain,
}
/// Exact surface incarnation/generation. Removing one lease cannot remove any
/// overlapping surface's lease. New descendants observe ancestors immediately.
pub struct SuspensionLease {
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
    pub fn upgrade(&self) -> Option<ControlDomain> {
        self.0.upgrade().map(ControlDomain)
    }
    pub fn live(&self) -> bool {
        self.upgrade()
            .is_some_and(|d| !d.closed() && !d.suspended())
    }
}
impl ControlDomain {
    pub fn new() -> Self {
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
            coverage: ControlCoverage::default(),
        })))
    }
    pub fn finish_initial_installation(&self) {
        self.0.borrow_mut().initial_installation = false;
        for child in self.children() {
            child.finish_initial_installation();
        }
    }
    pub fn renewal_from(&self, previous: &Self) -> DomainAttachment {
        assert!(previous.closed(), "renewal requires closed owner ownership");
        DomainAttachment::Renewal(ControlRenewal {
            previous: previous.0.borrow().incarnation,
            replacement: self.0.borrow().incarnation,
        })
    }
    pub fn permits_renewal(&self, previous: &Self, attachment: DomainAttachment) -> bool {
        matches!(attachment, DomainAttachment::Renewal(token) if previous.closed() && token.previous==previous.0.borrow().incarnation && token.replacement==self.0.borrow().incarnation)
    }
    pub fn outer() -> Self {
        let domain = Self::new();
        let id = domain.0.borrow().id;
        domain.set_owner(ControlOwner::Outer(id));
        domain
    }
    pub fn downgrade(&self) -> WeakControlDomain {
        WeakControlDomain(Rc::downgrade(&self.0))
    }
    pub fn same_owner_incarnation(&self, other: &Self) -> bool {
        self.0.borrow().incarnation == other.0.borrow().incarnation
    }
    pub fn inherit_owner(&self, parent: &Self) -> Result<(), AttachError> {
        let parent = parent.0.borrow();
        let mut state = self.0.borrow_mut();
        if state.closed || parent.closed {
            return Err(AttachError::Closed);
        }
        if state.incarnation == parent.incarnation {
            return Ok(());
        }
        // Ownership can be initialized before issuing any producer. Once
        // assigned, presentation reparenting cannot retarget that incarnation.
        if state.owner.is_some() || !state.producers.is_empty() {
            return Err(AttachError::ForeignOwner);
        }
        state.owner.clone_from(&parent.owner);
        state.incarnation = parent.incarnation;
        Ok(())
    }
    pub fn set_owner(&self, owner: ControlOwner) {
        self.0.borrow_mut().owner = Some(owner);
    }
    pub fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub fn closed(&self) -> bool {
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
    pub fn attach_to(&self, parent: &Self) -> Result<(), AttachError> {
        if self.same(parent) {
            return Ok(());
        }
        if self.closed() || parent.closed() {
            return Err(AttachError::Closed);
        }
        let mut ancestor = Some(parent.clone());
        while let Some(current) = ancestor {
            if self.same(&current) {
                return Err(AttachError::ForeignOwner);
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
    pub fn child(&self) -> Self {
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
    /// Take `outbox`'s queue into this domain: the control that made its
    /// domain before a host was there now reports here, keeping what it has
    /// already queued. Only a queue whose own domain has no owner can be
    /// adopted, so a report never changes hands between two hosts.
    fn adopt(&self, outbox: &mut ControlOutbox) {
        if let Some(own) = outbox.domain.upgrade() {
            own.release(&outbox.producer);
        }
        let mut state = self.0.borrow_mut();
        state.producers.push(outbox.producer.clone());
        state
            .order
            .push(DomainEntry::Producer(outbox.producer.clone()));
        state.coverage.registered += 1;
        outbox.owner_incarnation = state.incarnation;
        outbox.domain = self.downgrade();
    }

    /// Drop `producer` from this domain's inventory, keeping its queue.
    fn release(&self, producer: &Rc<RefCell<Producer>>) {
        let mut state = self.0.borrow_mut();
        state.producers.retain(|own| !Rc::ptr_eq(own, producer));
        state.order.retain(|entry| match entry {
            DomainEntry::Producer(own) => !Rc::ptr_eq(own, producer),
            DomainEntry::Child(_) => true,
        });
        state.coverage.registered = state.coverage.registered.saturating_sub(1);
    }

    pub fn coverage(&self) -> ControlCoverage {
        let mut result = self.0.borrow().coverage.clone();
        for child in self.children() {
            let coverage = child.coverage();
            result.registered += coverage.registered;
            result.unsafe_late_attachments += coverage.unsafe_late_attachments;
        }
        result
    }
    pub fn unsafe_attachment(&self) {
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
    pub fn close(&self) {
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
                            #[cfg_attr(not(feature = "model"), allow(unused_mut))]
                            let mut origin = owner.origin(&invocation);
                            #[cfg(feature = "model")]
                            if let PendingControlOrigin::Component(target) = &mut origin {
                                target.input = physical_text;
                            }
                            #[cfg(not(feature = "model"))]
                            let _ = physical_text;
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
    pub fn suspend(&self) -> (SuspensionLease, Vec<OwnedControl>) {
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
            .push_back((id, invocation, crate::widget::input_provenance::input()));
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
/// A control's queued intent: the producer it reports through, and — until a
/// host adopts it — the domain the control made for itself, so the queue is
/// the same one before and after mounting. There is no second place a report
/// can sit.
pub(crate) struct ControlHolder {
    discipline: ControlProducerDiscipline,
    /// The control domain, until a host adopts the queue.
    unowned: Option<ControlDomain>,
    /// The queue. Made on the first report while unowned, the host producer once adopted.
    queue: Option<ControlOutbox>,
    /// Whether this control reported while no host had adopted it: the
    /// reports are the control, so a host attaching later cannot take
    /// them as its.
    reported: bool,
}
impl ControlHolder {
    /// A control a host has not adopted yet.
    pub(crate) fn unowned(discipline: ControlProducerDiscipline) -> Self {
        Self {
            discipline,
            unowned: Some(ControlDomain::new()),
            queue: None,
            reported: false,
        }
    }

    /// A control built directly on its host's domain.
    pub(crate) fn in_domain(domain: &ControlDomain, discipline: ControlProducerDiscipline) -> Self {
        Self {
            discipline,
            unowned: None,
            queue: Some(domain.producer(discipline)),
            reported: false,
        }
    }

    fn queue(&mut self) -> Option<ControlOutbox> {
        if let Some(queue) = &self.queue {
            return Some(queue.clone());
        }
        let queue = self.unowned.as_ref()?.producer(self.discipline);
        self.queue = Some(queue.clone());
        Some(queue)
    }

    pub(crate) fn install(
        &mut self,
        domain: &ControlDomain,
        attachment: DomainAttachment,
    ) -> Result<(), AttachError> {
        if domain.closed() {
            return Err(AttachError::Closed);
        }
        if self.unowned.is_none() {
            let Some(outbox) = &self.queue else {
                return Ok(());
            };
            if outbox.belongs_to(domain) {
                return Ok(());
            }
            let permitted = if let DomainAttachment::Renewal(token) = attachment {
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
                return Err(AttachError::ForeignOwner);
            }
            if domain.closed() {
                return Err(AttachError::Closed);
            }
            self.queue = Some(domain.producer(self.discipline));
            return Ok(());
        }
        if self.reported
            && (attachment != DomainAttachment::Initial || !domain.0.borrow().initial_installation)
        {
            domain.unsafe_attachment();
            return Err(AttachError::UnsafeLateAttachment);
        }
        let queue = match &mut self.queue {
            Some(queue) => {
                domain.adopt(queue);
                queue.clone()
            }
            None => domain.producer(self.discipline),
        };
        self.queue = Some(queue);
        self.unowned = None;
        Ok(())
    }

    pub(crate) fn emit(
        &mut self,
        invocation: ControlInvocation,
    ) -> Result<LocalEmission, EmitError> {
        let queue = self.queue().ok_or(EmitError::Closed)?;
        let emission = queue.emit(invocation)?;
        if self.unowned.is_some() {
            self.reported = true;
        }
        Ok(emission)
    }

    /// One report of a control no host has adopted yet, oldest first.
    pub(crate) fn take(&self) -> Option<ControlInvocation> {
        self.unowned.as_ref()?;
        let queue = self.queue.as_ref()?;
        let mut producer = queue.producer.borrow_mut();
        producer
            .pending
            .pop_front()
            .map(|(_, invocation, physical_text)| {
                crate::widget::input_provenance::note_drained_input(physical_text);
                invocation
            })
    }

    pub(crate) fn clear(&self) {
        if let Some(queue) = &self.queue {
            queue.clear();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn controlled_pending_for_test(&self) -> Option<ControlInvocation> {
        self.queue
            .as_ref()?
            .producer
            .borrow()
            .pending
            .front()
            .map(|(_, invocation, _)| invocation.clone())
    }
}

/// Retained raw descriptor ownership for a single last-write-wins producer.
pub struct RawControlSlot {
    holder: ControlHolder,
}

impl RawControlSlot {
    pub fn replace_one() -> Self {
        Self {
            holder: ControlHolder::unowned(ControlProducerDiscipline::ReplaceOne),
        }
    }

    pub fn replace_one_in(domain: &ControlDomain) -> Self {
        Self {
            holder: ControlHolder::in_domain(domain, ControlProducerDiscipline::ReplaceOne),
        }
    }

    /// Bind this retained producer to a mounted owner.
    ///
    /// # Errors
    /// Refuses closed, foreign, or unsafe late owner transitions.
    pub fn attach(
        &mut self,
        domain: &ControlDomain,
        attachment: DomainAttachment,
    ) -> Result<(), AttachError> {
        self.holder.install(domain, attachment)
    }

    /// Own a complete invocation at emission time.
    pub fn emit(&mut self, invocation: ControlInvocation) -> Result<LocalEmission, EmitError> {
        self.holder.emit(invocation)
    }

    pub fn clear(&mut self) {
        self.holder.clear();
    }

    /// Inspect the retained descriptor in deterministic controlled fixtures.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn controlled_pending_for_test(&self) -> Option<ControlInvocation> {
        self.holder.controlled_pending_for_test()
    }
}
#[derive(Default)]
pub struct RegisteredControls {
    pub outer: VecDeque<ControlInvocation>,
    #[cfg(feature = "model")]
    pub components: Vec<(ComponentControl, ControlInvocation)>,
}
impl ControlDomain {
    pub fn drain_partitioned(&self) -> RegisteredControls {
        let mut result = RegisteredControls::default();
        for control in self.drain_live() {
            match control.origin {
                PendingControlOrigin::Outer { .. } => result.outer.push_back(control.invocation),
                #[cfg(feature = "model")]
                PendingControlOrigin::Component(target) => {
                    result.components.push((target, control.invocation))
                }
            }
        }
        result
    }
}

/// The life of what owns a control domain (a mounted component, an open record): while it is
/// active its controls run, and dropping it closes the domain.
pub struct ResourceOwnerLifetime {
    pub active: std::cell::Cell<bool>,
    pub id: uuid::Uuid,
    pub control_domain: ControlDomain,
}

impl ResourceOwnerLifetime {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            active: std::cell::Cell::new(true),
            id: uuid::Uuid::new_v4(),
            control_domain: ControlDomain::new(),
        })
    }
}

impl Drop for ResourceOwnerLifetime {
    fn drop(&mut self) {
        self.control_domain.close();
    }
}
