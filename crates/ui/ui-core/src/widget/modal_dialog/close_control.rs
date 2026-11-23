//! The dialog's close control as the host reaches it ([`ModalDialog::on_dismiss`]), and the
//! host's ownership of its emitter when it dismisses through an action.

use super::ModalDialog;
use crate::widget::Widget;

impl<C: Widget + 'static> ModalDialog<C> {
    /// Fires on Escape, on a scrim click and on the close control.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.set_on_dismiss(Some(Box::new(f)));
        self
    }

    /// [`Self::on_dismiss`] after construction; `None` takes the close control away.
    pub fn set_on_dismiss(&mut self, f: Option<Box<dyn FnMut()>>) {
        self.dismiss = f.map(crate::widget::dismissal::Dismissal::own);
        self.laid_out_for = None;
    }

    /// Dismiss through `binding`, an action the app or host already publishes ("Keep
    /// editing", "Cancel"), instead of a close control of the dialog's own.
    #[must_use]
    pub fn dismiss_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.dismiss = Some(crate::widget::dismissal::Dismissal::through(binding));
        self.laid_out_for = None;
        self
    }
}

impl ModalDialog<dyn Widget> {
    fn install_dismissal_ownership(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        if let Some(dismiss) = self.dismiss.as_mut() {
            dismiss.attach(domain, attachment);
        }
        for child in self.children_mut() {
            crate::widget::controls::attach_controls(child, domain, attachment);
        }
    }

    fn take_dismissal_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.dismiss
            .as_mut()
            .and_then(crate::widget::dismissal::Dismissal::take)
            .or_else(|| {
                self.children_mut()
                    .into_iter()
                    .find_map(crate::widget::controls::take_control_invocation)
            })
    }

    fn dismissal_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        if self.up()
            && self.closable()
            && let Some(dismiss) = &self.dismiss
        {
            dismiss.bind(self.close_target(), &mut bindings);
        }
        bindings
    }

    fn refresh_dismissal_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(dismiss) = self.dismiss.as_mut() {
            dismiss.refresh_availability(allowed);
        }
        for child in self.children_mut() {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl<C: Widget + 'static> crate::widget::controls::ActionControls for ModalDialog<C> {
    fn attach_controls(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        self.as_dyn_mut()
            .install_dismissal_ownership(domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.as_dyn_mut().take_dismissal_invocation()
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        self.as_dyn().dismissal_bindings()
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.as_dyn_mut().refresh_dismissal_availability(allowed);
    }
}
