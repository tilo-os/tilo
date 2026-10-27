//! The dialog's close control as the host reaches it: its published view
//! action, the host's custody of its emitter, and the operation that runs
//! the dismissal ([`ModalDialog::on_dismiss`]).

use automerge::ObjId;

use super::ModalDialog;
use crate::widget::Widget;

impl<C: Widget + 'static> ModalDialog<C> {
    /// Fires on Escape, on a scrim click and on the close control. A dialog
    /// with a dismiss handler and no secondary action shows the close
    /// control.
    ///
    /// Dismissal is the dialog's close control: a view action of control
    /// `<test id>.close` (the title stands in for a dialog without a test
    /// id), which every way of dismissing goes through, so agents and
    /// scripts close the dialog as a person does.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.dismiss = Some(crate::widget::dismissal::Dismissal::published(
            self.surface_id(),
            Box::new(f),
        ));
        self.laid_out_for = None;
        self
    }

    /// What the dialog's close control is named after: its test id, else
    /// its title.
    pub(super) fn surface_id(&self) -> &str {
        self.test_id.as_deref().unwrap_or(&self.title)
    }

    /// Dismiss through `binding`, an action the app or host already
    /// publishes ("Keep editing", "Cancel"), instead of a close control of
    /// the dialog's own.
    #[must_use]
    pub fn dismiss_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.dismiss = Some(crate::widget::dismissal::Dismissal::through(binding));
        self.laid_out_for = None;
        self
    }
}

/// The dialog's close control as the host reaches it, compiled once for
/// every content type: each `ModalDialog<C>` capability forwards here.
impl ModalDialog<dyn Widget> {
    fn start_dismissal(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if self.open
            && let Some(dismiss) = self.dismiss.as_mut()
            && let Some(started) = dismiss.start(op)?
        {
            return Ok(Some(started));
        }
        crate::widget::start_in_children(self, op, doc, path)
    }

    /// The close control while the dialog is open.
    fn dismissal_resources(&self) -> Vec<tilo_actions::Resource> {
        self.dismiss
            .as_ref()
            .filter(|_| self.open)
            .and_then(crate::widget::dismissal::Dismissal::resource)
            .into_iter()
            .collect()
    }

    fn install_dismissal_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(dismiss) = self.dismiss.as_mut() {
            dismiss.install_custody(domain, attachment);
        }
        for child in self.children_mut() {
            crate::widget::controls::install_control_custody(child, domain, attachment);
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

    /// The children's controls, with the close control bound to its action.
    fn dismissal_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        // The close control stands where the dialog shows it: with no way
        // out of its own, Escape and the scrim keep their meaning.
        if self.open
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

impl<C: Widget + 'static> crate::widget::ResourceOperations for ModalDialog<C> {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        self.dismiss
            .as_ref()
            .map_or(&[], crate::widget::dismissal::Dismissal::handlers)
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        self.as_dyn_mut().start_dismissal(op, doc, path)
    }
}

impl<C: Widget + 'static> crate::resources::PublishesResources for ModalDialog<C> {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        self.as_dyn().dismissal_resources()
    }
}

impl<C: Widget + 'static> crate::widget::controls::ActionControls for ModalDialog<C> {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.as_dyn_mut()
            .install_dismissal_custody(domain, attachment);
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
