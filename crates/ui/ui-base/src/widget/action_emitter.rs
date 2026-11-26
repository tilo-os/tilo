//! [`ActionEmitter`] — the reusable action-intent queue shared controls use, kept apart from the widget
//! toolkit itself so a control only pulls in the ownership and binding it queues through.

use std::collections::HashSet;

use crate::widget::{ControlBindingKind, ControlBindingMetadata};

/// A reusable action-intent queue for controls, including custom widgets.
/// Widgets call [`emit`](Self::emit) from any interaction path. Mounted shared
/// controls install independent host ownership; custom producers should use
/// [`new_in`](Self::new_in) with their actual owner domain. The private `take`
/// path remains only as explicitly unqualified migration compatibility.
pub struct ActionEmitter<I> {
    binding: tilo_actions::ActionBinding<I>,
    pending: crate::control_domain::ControlHolder,
    available: bool,
}

pub(crate) const ACTION_EMITTER_QUEUE_LIMIT: usize = 64;

impl<I> ActionEmitter<I> {
    pub fn new(binding: tilo_actions::ActionBinding<I>) -> Self {
        Self {
            binding,
            pending: crate::control_domain::ControlHolder::unowned(
                crate::control_domain::ControlProducerDiscipline::Fifo {
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
            pending: crate::control_domain::ControlHolder::unowned(
                crate::control_domain::ControlProducerDiscipline::ReplaceOne,
            ),
            available: true,
        }
    }

    pub fn new_in(binding: tilo_actions::ActionBinding<I>, domain: &crate::ControlDomain) -> Self {
        let mut emitter = Self::new(binding);
        emitter.pending = crate::control_domain::ControlHolder::in_domain(
            domain,
            crate::control_domain::ControlProducerDiscipline::Fifo {
                limit: ACTION_EMITTER_QUEUE_LIMIT,
            },
        );
        emitter
    }
    pub fn attach(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) -> Result<(), crate::control_domain::AttachError> {
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
            self.press(input)?;
        }
        Ok(())
    }

    /// Queues the person's own pointer press, whether or not the action is available: the
    /// dispatch runs it, or refuses one they may not run here and says why, where a dropped press
    /// would leave them guessing. A control that shows its action unavailable takes the press for
    /// this without taking focus or changing what it shows. Only a press of the person at the
    /// window comes here: a call made for someone else, by an agent or by an automation, is
    /// [`Self::emit`]ted and refused where it is made.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` cannot be encoded against the action's
    /// declared parameter contract.
    pub fn press(&mut self, input: I) -> Result<(), tilo_actions::ActionBindingError> {
        let invocation = self.binding.invoke(input)?;
        let _ = self.pending.emit(invocation);
        Ok(())
    }

    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending.take()
    }

    pub fn binding(&self) -> &tilo_actions::ActionBinding<I> {
        &self.binding
    }

    pub fn refresh_availability(&mut self, allowed: &HashSet<tilo_actions::ActionAddress>) {
        let was = std::mem::replace(
            &mut self.available,
            allowed.contains(self.binding.address()),
        );
        // What was asked while it could run is withdrawn with it; a refused press stays for the
        // dispatch to say why.
        if was && !self.available {
            self.pending.clear();
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn metadata(&self, control_id: impl Into<String>) -> ControlBindingMetadata {
        ControlBindingMetadata {
            control_id: control_id.into(),
            kind: ControlBindingKind::Action(self.binding.address().clone()),
        }
    }
}

/// A person's press on a slider whose action they may not run here: the slider takes no focus and
/// does not move, and the value the press points at is sent, to be refused with why
/// ([`ActionEmitter::emit`]), never dropped unseen.
#[derive(Clone, Copy, Debug, Default)]
pub struct RefusedPress {
    over: Option<vello::kurbo::Point>,
}

impl RefusedPress {
    /// Follow `event` over `rect`: where a press of the left button inside it landed, which it
    /// takes.
    pub fn pressed(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        rect: vello::kurbo::Rect,
    ) -> Option<vello::kurbo::Point> {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let at = ctx.logical(position);
                self.over = rect.contains(at).then_some(at);
                None
            }
            WindowEvent::CursorLeft { .. } => {
                self.over = None;
                None
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let at = self.over?;
                *ctx.handled = true;
                Some(at)
            }
            _ => None,
        }
    }
}
