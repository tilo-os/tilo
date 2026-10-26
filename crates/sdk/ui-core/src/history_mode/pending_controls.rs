//! Finite custody for live controls that were queued before History opened.
//!
//! Retained invocations are inert data.  A continuation token identifies one
//! presentation of one item; it never carries authority or confirmation.

use crate::bound_dispatch::{
    BoundActionTarget,
    legacy_document::{LegacyResourceTarget, LocalResourceTarget},
};
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
    NeedsConfirmation,
    Refused(String),
    Consumed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingControlReview {
    pub id: PendingControlId,
    pub invocation: ControlInvocation,
    pub status: PendingControlStatus,
    pub confirmation: Result<tilo_actions::ConfirmationPolicy, String>,
    pub owner: String,
    /// The action's name as its declaration gives it, when the control
    /// still resolves to one.
    pub label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContinuationStage {
    Review,
    Confirm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationToken {
    surface_lifetime: Uuid,
    owner_lifetime: Uuid,
    item_id: PendingControlId,
    presentation_generation: u64,
    nonce: Uuid,
    stage: ContinuationStage,
}

#[derive(Clone)]
pub(crate) enum PendingControlOrigin {
    Outer { root_lifetime: Uuid },
    Component(BoundActionTarget),
    Legacy(LegacyResourceTarget),
}

impl PendingControlOrigin {
    fn lifetime_id(&self) -> Option<Uuid> {
        match self {
            Self::Outer { root_lifetime } => Some(*root_lifetime),
            Self::Component(target) => Some(target.binding.lifetime_id),
            Self::Legacy(target) => target.owner.lifetime_id(),
        }
    }

    pub(crate) fn target(&self) -> LocalResourceTarget {
        match self {
            Self::Outer { .. } => LocalResourceTarget::Outer,
            Self::Component(target) => LocalResourceTarget::Component(target.clone()),
            Self::Legacy(target) => LocalResourceTarget::Legacy(target.clone()),
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Outer { root_lifetime } => format!("outer root {root_lifetime}"),
            Self::Component(target) => format!("component {}", target.binding.lifetime_id),
            Self::Legacy(target) => target.owner.lifetime_id().map_or_else(
                || "legacy owner unavailable".to_owned(),
                |id| format!("legacy owner {id}"),
            ),
        }
    }
}

/// Entry-time data for a binding queue whose mutable owner traversal failed.
/// No live producer is consulted when this sealed subset is recovered.
pub(crate) struct SealedComponentControl {
    pub(crate) target: BoundActionTarget,
    pub(crate) invocation: ControlInvocation,
    pub(crate) witness: Result<crate::bound_dispatch::ControlWitness, String>,
}

#[derive(Clone)]
struct PendingControl {
    id: PendingControlId,
    invocation: ControlInvocation,
    origin: PendingControlOrigin,
    witness: Result<crate::bound_dispatch::ControlWitness, String>,
    status: PendingControlStatus,
    outstanding_nonce: Option<Uuid>,
    control_domain: Option<crate::control_custody::WeakControlDomain>,
    /// The action's name as its declaration gives it, for people.
    label: Option<String>,
}

#[derive(Clone)]
pub(crate) enum ContinuationCommand {
    Review(ContinuationToken),
    Confirm(ContinuationToken, String),
    Cancel(ContinuationToken),
}

pub(crate) struct PreparedContinuation {
    pub(crate) id: PendingControlId,
    pub(crate) invocation: ControlInvocation,
    pub(crate) target: LocalResourceTarget,
    pub(crate) witness: Result<crate::bound_dispatch::ControlWitness, String>,
    pub(crate) confirmed: bool,
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
            if matches!(
                item.status,
                PendingControlStatus::Ready | PendingControlStatus::NeedsConfirmation
            ) {
                item.status = PendingControlStatus::Suspended;
            }
        }
        true
    }

    pub(crate) fn capture(
        &mut self,
        origin: PendingControlOrigin,
        invocation: ControlInvocation,
        witness: Result<crate::bound_dispatch::ControlWitness, String>,
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
            witness,
            status: PendingControlStatus::Suspended,
            outstanding_nonce: None,
            control_domain: None,
            label: None,
        });
    }

    pub(crate) fn capture_registered(
        &mut self,
        control: crate::control_custody::OwnedControl,
        witness: Result<crate::bound_dispatch::ControlWitness, String>,
    ) {
        self.capture(control.origin, control.invocation, witness);
        if let Some(item) = self.items.last_mut() {
            item.control_domain = Some(control.domain);
        }
    }
    /// Read each newly held control's declaration: name it as declared,
    /// and drop it when it is housekeeping. Only what a person asked for is
    /// held for their review; housekeeping the app does on its own, and it
    /// does it again once the person is back.
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
                    PendingControlStatus::Refused(crate::i18n::I18nManager::lookup_plain(
                        "history-mode-control-owner-closed",
                    ))
                } else {
                    item.status.clone()
                },
                confirmation: item
                    .witness
                    .as_ref()
                    .map(|witness| witness.confirmation().clone())
                    .map_err(Clone::clone),
                owner: item.origin.description(),
                label: item.label.clone().or_else(|| {
                    item.witness
                        .as_ref()
                        .ok()
                        .map(|witness| witness.label().to_owned())
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
        let stage = match item.status {
            PendingControlStatus::Ready => ContinuationStage::Review,
            PendingControlStatus::NeedsConfirmation => ContinuationStage::Confirm,
            PendingControlStatus::Suspended
            | PendingControlStatus::Refused(_)
            | PendingControlStatus::Consumed => return None,
        };
        let owner_lifetime = item.origin.lifetime_id()?;
        let nonce = Uuid::new_v4();
        item.outstanding_nonce = Some(nonce);
        Some(ContinuationToken {
            surface_lifetime: self.surface_lifetime,
            owner_lifetime,
            item_id: id,
            presentation_generation: self.presentation_generation,
            nonce,
            stage,
        })
    }

    fn consume(&mut self, token: &ContinuationToken, stage: ContinuationStage) -> Option<usize> {
        if self.suspended
            || token.surface_lifetime != self.surface_lifetime
            || token.presentation_generation != self.presentation_generation
            || token.stage != stage
        {
            return None;
        }
        let index = self
            .items
            .iter()
            .position(|item| item.id == token.item_id)?;
        let item = self.items.get_mut(index)?;
        if item.outstanding_nonce != Some(token.nonce)
            || item.origin.lifetime_id() != Some(token.owner_lifetime)
        {
            return None;
        }
        item.outstanding_nonce = None;
        Some(index)
    }

    pub(crate) fn prepare(
        &mut self,
        token: &ContinuationToken,
        confirmed: bool,
    ) -> Option<PreparedContinuation> {
        let stage = if confirmed {
            ContinuationStage::Confirm
        } else {
            ContinuationStage::Review
        };
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
        let index = self.consume(token, stage)?;
        let item = self.items.get(index)?;
        let expected = if confirmed {
            PendingControlStatus::NeedsConfirmation
        } else {
            PendingControlStatus::Ready
        };
        if item.status != expected {
            return None;
        }
        Some(PreparedContinuation {
            id: item.id,
            invocation: item.invocation.clone(),
            target: item.origin.target(),
            witness: item.witness.clone(),
            confirmed,
        })
    }

    pub(crate) fn prepare_typed_confirmation(
        &mut self,
        token: &ContinuationToken,
        value: &str,
    ) -> Option<PreparedContinuation> {
        let item = self.items.iter().find(|item| item.id == token.item_id)?;
        let policy = item.witness.as_ref().ok()?.confirmation();
        // The click on Confirm is the confirmation; only a phrase naming
        // what is destroyed must also be typed.
        match policy {
            tilo_actions::ConfirmationPolicy::None => return None,
            tilo_actions::ConfirmationPolicy::Confirm => {}
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                if value.trim() != phrase.trim() {
                    return None;
                }
            }
        }
        self.prepare(token, true)
    }

    pub(crate) fn needs_confirmation(&mut self, id: PendingControlId) {
        self.advance_generation();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.status = PendingControlStatus::NeedsConfirmation;
        }
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
        let Some(index) = self.consume(token, token.stage) else {
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
