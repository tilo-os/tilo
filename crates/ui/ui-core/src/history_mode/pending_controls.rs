//! Finite ownership for live controls that were queued before History opened. Retained invocations are
//! inert data. A continuation token identifies one presentation of one item; it never carries authority.
//! A kept-back control whose action waits for the person's yes goes, once continued, through the device's
//! dispatch like any press of theirs, which asks them.

use crate::bound_dispatch::{BoundActionTarget, local_target::LocalResourceTarget};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use tilo_actions::ControlInvocation;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PendingControlId(u64);

impl PendingControlId {
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingControlStatus {
    Suspended,
    Ready,
    Refused(String),
    Consumed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingControlReview {
    pub id: PendingControlId,
    pub invocation: ControlInvocation,
    pub status: PendingControlStatus,
    pub owner: String,
    /// The action's name as its declaration gives it, when the control still resolves to one.
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationToken {
    surface_lifetime: Uuid,
    owner_lifetime: Uuid,
    item_id: PendingControlId,
    presentation_generation: u64,
    nonce: Uuid,
}

pub(crate) use tilo_ui_base::control_domain::PendingControlOrigin;

fn origin_lifetime_id(origin: &PendingControlOrigin) -> Uuid {
    match origin {
        PendingControlOrigin::Outer { root_lifetime } => *root_lifetime,
        PendingControlOrigin::Component(control) => {
            crate::bound_dispatch::callback_binding_of(&*control.binding).lifetime_id
        }
    }
}

pub(crate) fn origin_target(origin: &PendingControlOrigin) -> LocalResourceTarget {
    match origin {
        PendingControlOrigin::Outer { .. } => LocalResourceTarget::Outer,
        PendingControlOrigin::Component(control) => {
            LocalResourceTarget::Component(crate::bound_dispatch::bound_target(control))
        }
    }
}

fn origin_description(origin: &PendingControlOrigin) -> String {
    match origin {
        PendingControlOrigin::Outer { root_lifetime } => format!("outer root {root_lifetime}"),
        PendingControlOrigin::Component(control) => format!(
            "component {}",
            crate::bound_dispatch::callback_binding_of(&*control.binding).lifetime_id
        ),
    }
}

/// Entry-time data for a binding queue whose mutable owner traversal failed.
pub(crate) struct SealedComponentControl {
    pub(crate) target: BoundActionTarget,
    pub(crate) invocation: ControlInvocation,
    pub(crate) observation: Result<crate::bound_dispatch::ControlObservation, String>,
}

#[derive(Clone)]
struct PendingControl {
    id: PendingControlId,
    invocation: ControlInvocation,
    origin: PendingControlOrigin,
    observation: Result<crate::bound_dispatch::ControlObservation, String>,
    status: PendingControlStatus,
    outstanding_nonce: Option<Uuid>,
    control_domain: Option<tilo_ui_base::control_domain::WeakControlDomain>,
    label: Option<String>,
}

#[derive(Clone)]
pub(crate) enum ContinuationCommand {
    Review(ContinuationToken),
    Cancel(ContinuationToken),
}

pub(crate) struct PreparedContinuation {
    pub(crate) id: PendingControlId,
    pub(crate) invocation: ControlInvocation,
    pub(crate) target: LocalResourceTarget,
    pub(crate) observation: Result<crate::bound_dispatch::ControlObservation, String>,
}

pub(crate) struct SuspendedLocalControls {
    surface_lifetime: Uuid,
    presentation_generation: u64,
    next_id: u64,
    suspended: bool,
    items: Vec<PendingControl>,
    commands: Rc<RefCell<VecDeque<ContinuationCommand>>>,
}

impl Default for SuspendedLocalControls {
    fn default() -> Self {
        Self {
            surface_lifetime: Uuid::new_v4(),
            presentation_generation: 0,
            next_id: 1,
            suspended: false,
            items: Vec::new(),
            commands: Rc::new(RefCell::new(VecDeque::new())),
        }
    }
}

impl SuspendedLocalControls {
    pub(crate) fn surface_lifetime(&self) -> Uuid {
        self.surface_lifetime
    }
    fn advance_generation(&mut self) {
        if let Some(next) = self.presentation_generation.checked_add(1) {
            self.presentation_generation = next;
        } else {
            self.invalidate_owner("continuation generation exhausted");
        }
        self.commands.borrow_mut().clear();
        for item in &mut self.items {
            item.outstanding_nonce = None;
        }
    }

    pub(crate) fn begin_history_entry(&mut self) -> bool {
        if self.suspended {
            return false;
        }
        self.advance_generation();
        self.suspended = true;
        for item in &mut self.items {
            if item.status == PendingControlStatus::Ready {
                item.status = PendingControlStatus::Suspended;
            }
        }
        true
    }

    pub(crate) fn capture(
        &mut self,
        origin: PendingControlOrigin,
        invocation: ControlInvocation,
        observation: Result<crate::bound_dispatch::ControlObservation, String>,
    ) {
        if !self.suspended {
            return;
        }
        let id = PendingControlId(self.next_id);
        let Some(next) = self.next_id.checked_add(1) else {
            self.invalidate_owner("pending control identifier exhausted");
            return;
        };
        self.next_id = next;
        self.items.push(PendingControl {
            id,
            invocation,
            origin,
            observation,
            status: PendingControlStatus::Suspended,
            outstanding_nonce: None,
            control_domain: None,
            label: None,
        });
    }

    pub(crate) fn capture_registered(
        &mut self,
        control: tilo_ui_base::control_domain::OwnedControl,
        observation: Result<crate::bound_dispatch::ControlObservation, String>,
    ) {
        self.capture(control.origin, control.invocation, observation);
        if let Some(item) = self.items.last_mut() {
            item.control_domain = Some(control.domain);
        }
    }
    /// Read each newly held control's declaration: name it as declared, and drop it when it is
    /// housekeeping.
    pub(crate) fn admit_declared(
        &mut self,
        mut declared: impl FnMut(
            &PendingControlOrigin,
            &ControlInvocation,
        ) -> Option<crate::bound_dispatch::ControlDeclaration>,
    ) {
        let before = self.items.len();
        self.items.retain_mut(|item| {
            if item.label.is_some() {
                return true;
            }
            let Some(declaration) = declared(&item.origin, &item.invocation) else {
                return true;
            };
            item.label = declaration.label;
            !declaration.housekeeping
        });
        if self.items.len() != before {
            self.advance_generation();
        }
    }

    pub(crate) fn return_to_live(&mut self) -> bool {
        if !self.suspended {
            return false;
        }
        self.suspended = false;
        self.advance_generation();
        for item in &mut self.items {
            if matches!(item.status, PendingControlStatus::Suspended) {
                item.status = PendingControlStatus::Ready;
            }
        }
        true
    }

    pub(crate) fn review(&self) -> Vec<PendingControlReview> {
        self.items
            .iter()
            .map(|item| PendingControlReview {
                id: item.id,
                invocation: item.invocation.clone(),
                status: if item.status != PendingControlStatus::Consumed
                    && item
                        .control_domain
                        .as_ref()
                        .is_some_and(|domain| domain.upgrade().is_none_or(|domain| domain.closed()))
                {
                    PendingControlStatus::Refused(tilo_ui_base::i18n::I18nManager::lookup_plain(
                        "history-mode-control-owner-closed",
                    ))
                } else {
                    item.status.clone()
                },
                owner: origin_description(&item.origin),
                label: item.label.clone().or_else(|| {
                    item.observation
                        .as_ref()
                        .ok()
                        .map(|observation| observation.label().to_owned())
                        .filter(|label| !label.trim().is_empty())
                }),
            })
            .collect()
    }

    pub(crate) fn token(&mut self, id: PendingControlId) -> Option<ContinuationToken> {
        if self.suspended {
            return None;
        }
        let item = self.items.iter_mut().find(|item| item.id == id)?;
        if item.status != PendingControlStatus::Ready {
            return None;
        }
        let owner_lifetime = origin_lifetime_id(&item.origin);
        let nonce = Uuid::new_v4();
        item.outstanding_nonce = Some(nonce);
        Some(ContinuationToken {
            surface_lifetime: self.surface_lifetime,
            owner_lifetime,
            item_id: id,
            presentation_generation: self.presentation_generation,
            nonce,
        })
    }

    fn consume(&mut self, token: &ContinuationToken) -> Option<usize> {
        if self.suspended
            || token.surface_lifetime != self.surface_lifetime
            || token.presentation_generation != self.presentation_generation
        {
            return None;
        }
        let index = self
            .items
            .iter()
            .position(|item| item.id == token.item_id)?;
        let item = self.items.get_mut(index)?;
        if item.outstanding_nonce != Some(token.nonce)
            || origin_lifetime_id(&item.origin) != token.owner_lifetime
        {
            return None;
        }
        item.outstanding_nonce = None;
        Some(index)
    }

    pub(crate) fn prepare(&mut self, token: &ContinuationToken) -> Option<PreparedContinuation> {
        if self
            .items
            .iter()
            .find(|item| item.id == token.item_id)?
            .control_domain
            .as_ref()
            .is_some_and(|domain| !domain.live())
        {
            return None;
        }
        let index = self.consume(token)?;
        let item = self.items.get(index)?;
        if item.status != PendingControlStatus::Ready {
            return None;
        }
        Some(PreparedContinuation {
            id: item.id,
            invocation: item.invocation.clone(),
            target: origin_target(&item.origin),
            observation: item.observation.clone(),
        })
    }

    pub(crate) fn refused(&mut self, id: PendingControlId, reason: impl Into<String>) {
        self.advance_generation();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.status = PendingControlStatus::Refused(reason.into());
        }
    }

    pub(crate) fn consumed(&mut self, id: PendingControlId) {
        self.advance_generation();
        self.items.retain(|item| item.id != id);
    }

    pub(crate) fn cancel(&mut self, token: &ContinuationToken) -> bool {
        let Some(index) = self.consume(token) else {
            return false;
        };
        self.advance_generation();
        self.items.remove(index);
        true
    }

    pub(crate) fn invalidate_owner(&mut self, reason: &str) {
        self.suspended = true;
        self.commands.borrow_mut().clear();
        for item in &mut self.items {
            item.outstanding_nonce = None;
            item.status = PendingControlStatus::Refused(reason.to_owned());
        }
    }

    pub(crate) fn commands(&self) -> Rc<RefCell<VecDeque<ContinuationCommand>>> {
        self.commands.clone()
    }

    pub(crate) fn drain_commands(&self) -> Vec<ContinuationCommand> {
        self.commands.borrow_mut().drain(..).collect()
    }
}
