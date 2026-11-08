//! [`ActionEmitter`] — the reusable action-intent queue shared controls use, kept apart from the
//! widget toolkit itself so a control only pulls in the custody and binding it queues through.

use std::collections::HashSet;

use crate::widget::{ControlBindingKind, ControlBindingMetadata};

/// A reusable action-intent queue for controls, including custom widgets.
/// Widgets call [`emit`](Self::emit) from any interaction path. Mounted shared
/// controls install independent host custody; custom producers should use
/// [`new_in`](Self::new_in) with their actual owner domain. The private `take`
/// path remains only as explicitly unqualified migration compatibility.
pub struct ActionEmitter<I> {
    binding: tilo_actions::ActionBinding<I>,
    pending: crate::control_custody::ControlHolder,
    available: bool,
}

pub(crate) const ACTION_EMITTER_QUEUE_LIMIT: usize = 64;

impl<I> ActionEmitter<I> {
    pub fn new(binding: tilo_actions::ActionBinding<I>) -> Self {
        Self {
            binding,
            pending: crate::control_custody::ControlHolder::unowned(
                crate::control_custody::ControlProducerDiscipline::Fifo {
                    limit: ACTION_EMITTER_QUEUE_LIMIT,
                },
            ),
            available: true,
        }
    }

    /// A submit emitter: a press the host has not collected yet is replaced by
    /// the next, so one batch of gestures submits once rather than once per
    /// press (see [`ActionEmitter::new`] for the queueing form).
    pub fn new_replace_one(binding: tilo_actions::ActionBinding<I>) -> Self {
        Self {
            binding,
            pending: crate::control_custody::ControlHolder::unowned(
                crate::control_custody::ControlProducerDiscipline::ReplaceOne,
            ),
            available: true,
        }
    }

    /// Construct already registered, including for emitting inside a builder.
    pub fn new_in(binding: tilo_actions::ActionBinding<I>, domain: &crate::ControlDomain) -> Self {
        let mut emitter = Self::new(binding);
        emitter.pending = crate::control_custody::ControlHolder::in_domain(
            domain,
            crate::control_custody::ControlProducerDiscipline::Fifo {
                limit: ACTION_EMITTER_QUEUE_LIMIT,
            },
        );
        emitter
    }
    pub fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) -> Result<(), crate::control_custody::CustodyError> {
        self.pending.install(domain, attachment)
    }

    /// Queues the typed invocation when the action is currently available.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` cannot be encoded against the action's
    /// declared parameter contract.
    pub fn emit(&mut self, input: I) -> Result<(), tilo_actions::ActionBindingError> {
        if self.available {
            let invocation = self.binding.invoke(input)?;
            let _ = self.pending.emit(invocation);
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending.take()
    }

    pub fn binding(&self) -> &tilo_actions::ActionBinding<I> {
        &self.binding
    }

    /// Refresh whether the bound action is in the host's current,
    /// permission-filtered resource surface.
    pub fn refresh_availability(&mut self, allowed: &HashSet<tilo_actions::ActionAddress>) {
        self.available = allowed.contains(self.binding.address());
        if !self.available {
            self.pending.clear();
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Inventory entry for a custom control using this emitter.
    pub fn metadata(&self, control_id: impl Into<String>) -> ControlBindingMetadata {
        ControlBindingMetadata {
            control_id: control_id.into(),
            kind: ControlBindingKind::Action(self.binding.address().clone()),
        }
    }
}
