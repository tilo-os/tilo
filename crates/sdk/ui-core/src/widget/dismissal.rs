//! How a surface that closes itself (a [`ModalDialog`](super::ModalDialog),
//! a [`Popover`](super::Popover)) is dismissed: through an action, never a
//! private callback. The surface either publishes a close control of its own,
//! a view action of control `<id>.close` that runs the app's handler, or
//! dismisses through an action the app or host already publishes ("Keep
//! editing", "Cancel"). Escape, a click outside and the close button all
//! take that way, so agents and scripts close the surface as a person does.

use super::activation::{NATIVE_INVOKE, NativeControl};
use super::{ActionEmitter, ControlBindingMetadata};

pub(crate) struct Dismissal {
    /// The close control's id, `<surface>.close`, when the surface publishes
    /// one of its own.
    id: String,
    native: Option<NativeControl>,
    action: ActionEmitter<()>,
}

impl Dismissal {
    /// A close control of the surface's own, `<surface>.close`, running
    /// `handler`.
    pub(crate) fn published(surface: &str, handler: Box<dyn FnMut()>) -> Self {
        let id = format!("{surface}.close");
        let (native, binding) = NativeControl::new(
            &id,
            &crate::i18n!("Close"),
            tilo_actions::OperationKind::View,
            handler,
        );
        Self {
            id,
            native: Some(native),
            action: ActionEmitter::new(binding),
        }
    }

    /// Dismiss through an action published elsewhere.
    pub(crate) fn through(binding: tilo_actions::ActionBinding<()>) -> Self {
        Self {
            id: String::new(),
            native: None,
            action: ActionEmitter::new(binding),
        }
    }

    /// The same close control, published under `surface`'s id instead.
    pub(crate) fn renamed(self, surface: &str) -> Self {
        match self.native {
            Some(native) => Self::published(surface, native.into_handler()),
            None => self,
        }
    }

    /// The person dismissed the surface. Its own close control is a view
    /// action, which no host decides and no history records: it runs at
    /// once, wherever the surface is hosted, as an `invoke` of it does. An
    /// action published elsewhere goes through the host.
    pub(crate) fn emit(&mut self) {
        if let Some(native) = &mut self.native {
            native.run();
        } else if let Err(error) = self.action.emit(()) {
            tracing::warn!(%error, "dismissal refused");
        }
    }

    /// The operation types the surface answers for its close control.
    pub(crate) fn handlers(&self) -> &'static [&'static str] {
        if self.native.is_some() {
            &[NATIVE_INVOKE]
        } else {
            &[]
        }
    }

    /// Run the handler for the close control's own `invoke`; `None` when the
    /// operation is not for it.
    pub(crate) fn start(
        &mut self,
        op: &super::ResourceOperation<'_>,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        match &mut self.native {
            Some(native) => native.start(&self.id, op),
            None => Ok(None),
        }
    }

    /// The close control the surface publishes, if it has one of its own.
    pub(crate) fn resource(&self) -> Option<tilo_actions::ResourceSpec> {
        self.native.as_ref().map(NativeControl::resource)
    }

    pub(crate) fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.action.install_custody(domain, attachment);
    }

    pub(crate) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.take()
    }

    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.action.refresh_availability(allowed);
    }

    /// `bindings` with the semantic control at `target` bound to the
    /// dismissal's action instead.
    pub(crate) fn bind(&self, target: String, bindings: &mut Vec<ControlBindingMetadata>) {
        bindings.retain(|binding| binding.control_id != target);
        bindings.push(self.action.metadata(target));
    }
}
