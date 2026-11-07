//! The control contract of a framework root: the presses its dispatcher and
//! its view's controls queued, and how each one is answered.
//!
//! `docs/designs/app-language/one-app-model.md` §4 decides where a press goes:
//! over the app's own record when the root keeps one, and over the session's
//! connection when the session keeps the record. A hosted root's controls stay
//! unowned so that this root, and no host's local document door, submits them.

use super::ResourceRoot;
use super::widget::RootControls;

impl RootControls for ResourceRoot {
    /// A root the session serves keeps its view's controls unowned: each
    /// press is submitted over the connection that keeps the record, never
    /// run by a host's local document door, which has no document here
    /// (`docs/designs/app-language/one-app-model.md` §4). Any other root
    /// hands its tree to the host's domain as usual.
    fn install_control_custody(
        &mut self,
        domain: &ui_core::ControlDomain,
        attachment: ui_core::control_custody::CustodyAttachment,
    ) {
        if self.source.is_connection() {
            return;
        }
        ui_core::widget::controls::install_control_custody(
            self.content.as_mut(),
            domain,
            attachment,
        );
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui_core::widget::controls::AttributedControlInvocation> {
        let taken = self.dispatch.take_attributed().or_else(|| {
            ui_core::widget::controls::take_attributed_control_invocation(self.content.as_mut())
        });
        if let Some((invocation, _)) = &taken {
            self.share.note_view_invocation(invocation);
        }
        let connection_submits = self.source.is_connection();
        match taken {
            Some((invocation, None)) if connection_submits => {
                self.dispatch
                    .submit_view_control(&invocation.address, invocation.arguments);
                None
            }
            taken => taken,
        }
    }
    /// An edit this resource's dispatcher submitted was refused, or is held
    /// for the user's confirmation. Nothing changed in the document, so a
    /// surface that previewed the edit is re-presented the stored state; a
    /// confirmed edit then arrives as an ordinary document change.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        // The host delivers feedback to the exact emitting document owner;
        // forwarding stops at nested owner boundaries. Its children may have
        // arbitrary ids, including records removed since submission. Repair
        // this owner's projection without inferring custody from an id prefix
        // or retaining requests that History may capture or discard.
        if let Some(effect) = ui_core::widget::controls::result_on_behalf_of() {
            self.dispatch
                .note_continuation(effect, &address.action, result);
        }
        match result {
            Err(error) => {
                if !matches!(error, tilo_actions::DoorError::ConfirmationRequired) {
                    self.dispatch.refuse(&address.action, error.to_string());
                }
                self.typed.represent();
            }
            Ok(ui_core::host_operation::ResourceOperationStart::Completed(output)) => {
                self.dispatch.note_landed(&address.action, output);
            }
            Ok(_) => {}
        }
        ui_core::widget::controls::on_control_invocation_result(
            self.content.as_mut(),
            address,
            result,
        );
    }
    fn has_pending_control_invocation(&self) -> bool {
        self.dispatch.has_pending()
            || ui_core::widget::controls::has_pending_control_invocation(self.app_view())
    }
}
