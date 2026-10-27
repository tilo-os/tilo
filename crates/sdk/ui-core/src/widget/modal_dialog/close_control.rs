//! The dialog's own controls as the host reaches them: its close control
//! ([`ModalDialog::on_dismiss`]) and its footer buttons, each published as
//! an action of control `<dialog>.<close|primary|secondary|tertiary>`; the
//! host's custody of the close control's emitter; and the operations that
//! run them.
//!
//! A person's press runs at once, as it always did. An agent's or a
//! script's arrives as the `invoke` of the published control: Cancel and the
//! close control are view actions, the primary and third answers effects,
//! which the host admits under the invoker's grants.

use automerge::ObjId;

use super::{ModalDialog, Slot};
use crate::widget::Widget;
use crate::widget::activation::{NATIVE_INVOKE, NativeControl, invoked_control};

/// Every footer slot, with the name its control is published under and its
/// operation kind.
const FOOTER: [(Slot, &str, tilo_actions::OperationKind); 3] = [
    (
        Slot::Primary,
        "primary",
        tilo_actions::OperationKind::Effect,
    ),
    (
        Slot::Secondary,
        "secondary",
        tilo_actions::OperationKind::View,
    ),
    (
        Slot::Tertiary,
        "tertiary",
        tilo_actions::OperationKind::Effect,
    ),
];

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
    /// The published id of the footer control in `slot`.
    fn footer_id(&self, name: &str) -> String {
        format!("{}.{name}", self.test_id.as_deref().unwrap_or(&self.title))
    }

    /// The footer buttons the dialog shows and can take now, with their
    /// published name and kind.
    fn published_footer(
        &self,
    ) -> impl Iterator<Item = (Slot, &'static str, tilo_actions::OperationKind)> + '_ {
        FOOTER.into_iter().filter(|(slot, _, _)| {
            self.open && self.slot_action(*slot).is_some() && self.slot_enabled(*slot)
        })
    }

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
        let invoked = invoked_control(op)?;
        let slot = invoked.and_then(|control| {
            self.published_footer()
                .find(|(_, name, _)| self.footer_id(name) == control)
                .map(|(slot, _, _)| slot)
        });
        if let Some(slot) = slot {
            self.activate(slot);
            return Ok(Some(crate::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
        }
        crate::widget::start_in_children(self, op, doc, path)
    }

    /// The close control and the footer buttons while the dialog is open.
    fn dismissal_resources(&self) -> Vec<tilo_actions::Resource> {
        let close = self
            .dismiss
            .as_ref()
            .filter(|_| self.open)
            .and_then(crate::widget::dismissal::Dismissal::resource);
        let footer = self.published_footer().filter_map(|(slot, name, kind)| {
            let action = self.slot_action(slot)?;
            Some(NativeControl::resource_of(
                &self.footer_id(name),
                &crate::i18n!(&action.label),
                kind,
            ))
        });
        close.into_iter().chain(footer).collect()
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

    /// The children's controls, with the close control and the footer
    /// buttons bound to their actions.
    fn dismissal_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        for (slot, name, _) in self.published_footer() {
            let target = self.slot_target(slot);
            if bindings.iter().any(|binding| binding.control_id == target) {
                bindings.retain(|binding| binding.control_id != target);
                bindings.push(crate::widget::ControlBindingMetadata {
                    control_id: target,
                    kind: crate::widget::ControlBindingKind::Action(
                        tilo_actions::ActionAddress::new(
                            format!("tilo.control:{}", self.footer_id(name)),
                            "invoke",
                        ),
                    ),
                });
            }
        }
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
        let footer = self.primary_action.is_some()
            || self.secondary_action.is_some()
            || self.tertiary_action.is_some();
        if footer {
            &[NATIVE_INVOKE]
        } else {
            self.dismiss
                .as_ref()
                .map_or(&[], crate::widget::dismissal::Dismissal::handlers)
        }
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
