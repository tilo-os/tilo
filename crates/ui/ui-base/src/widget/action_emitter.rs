//! [`ActionEmitter`] — the reusable action-intent queue shared controls use, kept apart from the widget
//! toolkit itself so a control only pulls in the ownership and binding it queues through — and
//! [`BoundAction`], the bound action it queues for.

use std::collections::HashSet;
use std::sync::Arc;

use tilo_action_types::{ActionAddress, ControlInvocation};

use crate::widget::{ControlBindingKind, ControlBindingMetadata};

type Invoke<I> = Arc<dyn Fn(I) -> Result<ControlInvocation, ArgumentsRefused> + Send + Sync>;

/// A control bound to an exported action: the action's address, and how the control's value
/// becomes the action's checked arguments.
///
/// Only the toolkit above this crate (`tilo-ui`) makes one, and it makes it from the action
/// binding (`ActionBinding`) a resource exports, which checks every value against
/// the action's declared parameters before it is sent. That is why an app cannot bind a control
/// to an address and arguments of its own making: it hands the toolkit the binding, and the
/// toolkit hands the control this.
///
/// Rust has no visibility between two crates short of `pub`, so the one constructor,
/// [`Self::from_tilo_ui_binding`], is public and hidden from the docs, and the repository contract
/// `tools/policy/check_control_binding.py` refuses every call to it outside `tilo-ui`'s binding
/// module. There is no public field and no other constructor; an app crate cannot write one:
///
/// ```compile_fail
/// let bound = tilo_ui_base::widget::BoundAction::<()> { address: todo!(), shared: None, invoke: todo!() };
/// ```
pub struct BoundAction<I> {
    address: ActionAddress,
    /// The app publishing the resource and the resource's kind, when the action changes a
    /// document everyone in the workspace reads ([`Self::shared_resource`]).
    shared: Option<(String, String)>,
    invoke: Invoke<I>,
}

impl<I> Clone for BoundAction<I> {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            shared: self.shared.clone(),
            invoke: Arc::clone(&self.invoke),
        }
    }
}

impl<I> BoundAction<I> {
    /// The binding `tilo-ui` makes from an action binding: see the type's documentation for why no
    /// other crate calls this.
    #[doc(hidden)]
    pub fn from_tilo_ui_binding(
        address: ActionAddress,
        shared: Option<(String, String)>,
        invoke: impl Fn(I) -> Result<ControlInvocation, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            address,
            shared,
            invoke: Arc::new(move |input| invoke(input).map_err(ArgumentsRefused)),
        }
    }

    /// The invocation of the action with `input`'s arguments, checked against its parameters.
    ///
    /// # Errors
    ///
    /// Returns [`ArgumentsRefused`] when `input` does not make arguments the action takes.
    pub fn invoke(&self, input: I) -> Result<ControlInvocation, ArgumentsRefused> {
        (self.invoke)(input)
    }

    #[must_use]
    pub fn address(&self) -> &ActionAddress {
        &self.address
    }

    /// What a control bound here does, as a window publishes it: the address the API runs it by,
    /// so what a person presses and what a caller names at the API are one thing.
    #[must_use]
    pub fn identity(&self) -> ActionAddress {
        self.address.clone()
    }

    /// The record a control bound here writes into for everyone in the workspace to read, as the
    /// app publishing it, the resource and its kind: a change to a shared document (a message
    /// sent into a conversation), never an effect, a view change or the person's own record.
    #[must_use]
    pub fn shared_resource(&self) -> Option<(&str, &str, &str)> {
        let (app, kind) = self.shared.as_ref()?;
        Some((app, self.address.resource.as_str(), kind))
    }
}

/// A control's value that does not make arguments its action takes: refused where the control
/// emits it, never sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ArgumentsRefused(pub String);

/// A reusable action-intent queue for controls, including custom widgets.
/// Widgets call [`emit`](Self::emit) from any interaction path. Mounted shared
/// controls install independent host ownership; custom producers should use
/// [`new_in`](Self::new_in) with their actual owner domain. The private `take`
/// path remains only as explicitly unqualified migration compatibility.
pub struct ActionEmitter<I> {
    binding: BoundAction<I>,
    pending: crate::control_domain::ControlHolder,
    available: bool,
}

pub(crate) const ACTION_EMITTER_QUEUE_LIMIT: usize = 64;

impl<I> ActionEmitter<I> {
    pub fn new(binding: BoundAction<I>) -> Self {
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
    pub fn new_replace_one(binding: BoundAction<I>) -> Self {
        Self {
            binding,
            pending: crate::control_domain::ControlHolder::unowned(
                crate::control_domain::ControlProducerDiscipline::ReplaceOne,
            ),
            available: true,
        }
    }

    pub fn new_in(binding: BoundAction<I>, domain: &crate::ControlDomain) -> Self {
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
    pub fn emit(&mut self, input: I) -> Result<(), ArgumentsRefused> {
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
    pub fn press(&mut self, input: I) -> Result<(), ArgumentsRefused> {
        let invocation = self.binding.invoke(input)?;
        let _ = self.pending.emit(invocation);
        Ok(())
    }

    pub fn take(&mut self) -> Option<ControlInvocation> {
        self.pending.take()
    }

    pub fn binding(&self) -> &BoundAction<I> {
        &self.binding
    }

    pub fn refresh_availability(&mut self, allowed: &HashSet<ActionAddress>) {
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
