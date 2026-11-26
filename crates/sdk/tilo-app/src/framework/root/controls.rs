//! The control contract of a framework root: the presses its dispatcher and
//! its view's controls queued, and how each one is answered.
//!
//! A press goes over the app's own record: the root keeps it, and the host's
//! local document API runs what the view's controls submit.

use super::ResourceRoot;
use super::widget::RootControls;

impl RootControls for ResourceRoot {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        tilo_ui_base::widget::controls::attach_controls(self.content.as_mut(), domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }
    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<tilo_ui_base::widget::controls::AttributedControlInvocation> {
        let taken = self.dispatch.take_attributed().or_else(|| {
            tilo_ui_base::widget::controls::take_attributed_control_invocation(self.content.as_mut())
        });
        if let Some((invocation, _)) = &taken {
            self.share.note_view_invocation(invocation);
        }
        taken
    }
    /// An edit this resource's dispatcher submitted was refused, or is held
    /// for the user's confirmation. Nothing changed in the document, so a
    /// surface that previewed the edit is re-presented the stored state; a
    /// confirmed edit then arrives as an ordinary document change.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::ApiError>,
    ) {
        // The host delivers feedback to the exact emitting document owner;
        // forwarding stops at nested owner boundaries. Its children may have
        // arbitrary ids, including records removed since submission. Repair
        // this owner's projection without inferring ownership from an id prefix
        // or retaining requests that History may capture or discard.
        if let Some(effect) = tilo_ui_base::widget::controls::result_on_behalf_of() {
            self.dispatch
                .note_continuation(effect, &address.action, result);
        }
        match result {
            Err(error) => {
                if !matches!(error, tilo_actions::ApiError::ConfirmationRequired) {
                    self.dispatch.refuse(&address.action, error.to_string());
                }
                self.typed.represent();
            }
            Ok(ui_core::host_operation::ResourceOperationStart::Completed(output)) => {
                self.dispatch.note_landed(&address.action, output);
            }
            Ok(_) => {}
        }
        tilo_ui_base::widget::controls::on_control_invocation_result(
            self.content.as_mut(),
            address,
            result,
        );
    }
    fn has_pending_control_invocation(&self) -> bool {
        self.dispatch.has_pending()
            || tilo_ui_base::widget::controls::has_pending_control_invocation(self.app_view())
    }
}
