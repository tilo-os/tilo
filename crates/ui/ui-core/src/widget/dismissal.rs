//! How a surface that closes itself (a [`ModalDialog`](super::ModalDialog), a
//! [`Popover`](super::Popover)) is dismissed: by the app's own handler, or through an action
//! the app or host already publishes ("Keep editing", "Cancel"). Escape, a click outside and
//! the close button all take that way.

use tilo_ui_base::{widget::ActionEmitter, widget::ControlBindingMetadata};

pub(crate) enum Dismissal {
    /// The app's handler, run at once.
    Own(Box<dyn FnMut()>),
    /// An action published elsewhere, which goes through the host.
    Through(ActionEmitter<()>),
}

impl Dismissal {
    /// Dismiss by running `handler`.
    pub(crate) fn own(handler: Box<dyn FnMut()>) -> Self {
        Self::Own(handler)
    }

    /// Dismiss through an action published elsewhere.
    pub(crate) fn through(binding: tilo_actions::ActionBinding<()>) -> Self {
        Self::Through(ActionEmitter::new(crate::Bind::bound(binding)))
    }

    /// The person dismissed the surface.
    pub(crate) fn emit(&mut self) {
        match self {
            Self::Own(handler) => handler(),
            Self::Through(action) => {
                if let Err(error) = action.emit(()) {
                    tracing::warn!(%error, "dismissal refused");
                }
            }
        }
    }

    pub(crate) fn attach(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        if let Self::Through(action) = self {
            let _ = action.attach(domain, attachment);
        }
    }

    pub(crate) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        match self {
            Self::Own(_) => None,
            Self::Through(action) => action.take(),
        }
    }

    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Self::Through(action) = self {
            action.refresh_availability(allowed);
        }
    }

    /// `bindings` with the semantic control at `target` bound to the dismissal's action
    /// instead, when it dismisses through one.
    pub(crate) fn bind(&self, target: String, bindings: &mut Vec<ControlBindingMetadata>) {
        if let Self::Through(action) = self {
            bindings.retain(|binding| binding.control_id != target);
            bindings.push(action.metadata(target));
        }
    }
}
