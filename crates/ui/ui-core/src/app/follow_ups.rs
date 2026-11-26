//! Edits an effect submitted for its invoker, admitted by the broker as
//! theirs: the effect answers once the last of them is settled.

use super::App;
use super::runtime::BrokerReply;

impl App {
    /// Ask the broker to admit an edit the effect invocation `running`
    /// submitted for its invoker: it decides with the invoker's grants and,
    /// when they allow it, hands it back here as theirs.
    pub(crate) fn send_follow_up(
        &mut self,
        running: tilo_actions::InvocationId,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        context: Option<tilo_actions::ActionContext>,
        control: crate::app_core::FollowUpControl,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        let request_id = self.take_broker_request_id()?;
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: Some(running),
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::OWN_PROVIDER.into(),
                address: address.clone(),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
            context,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        self.actions
            .broker_requests
            .insert(BrokerReply::Receipt(request.invocation), request_id);
        self.actions
            .follow_ups
            .insert(request_id, (running, control));
        if let Some(continuation) = self.actions.acting_for.get_mut(&running) {
            continuation.outstanding.insert(request_id);
        }
        self.send_own_invocation(request);
        Ok(tilo_ui_base::widget::ResourceOperationStart::Accepted)
    }

    /// The broker answered the edit request `request_id` an effect
    /// submitted: a refusal fails the effect, and the effect's held answer
    /// goes out once its last edit is settled. Whether it was one.
    pub(super) fn settle_follow_up(
        &mut self,
        request_id: u64,
        result: &Result<(tilo_actions::InvocationId, tilo_actions::InvocationUpdate), String>,
    ) -> bool {
        let Some((running, control)) = self.actions.follow_ups.remove(&request_id) else {
            return false;
        };
        let refusal = match result {
            Err(reason) => Some(tilo_actions::ActionFailure {
                code: "permission_denied".into(),
                message: reason.clone(),
                details: None,
                retryable: false,
            }),
            Ok((_, tilo_actions::InvocationUpdate::Failed { error, .. })) => Some(error.clone()),
            Ok((_, tilo_actions::InvocationUpdate::Cancelled { .. })) => {
                Some(tilo_actions::ActionFailure {
                    code: "follow_up_cancelled".into(),
                    message: "an edit the effect submitted was cancelled".into(),
                    details: None,
                    retryable: false,
                })
            }
            Ok(_) => None,
        };
        // The control the edit came from hears how it ended, with what it made: the effect's own
        // answer may be what the edit made.
        let ended = match result {
            Ok((_, tilo_actions::InvocationUpdate::Succeeded { output, .. })) => Ok(
                tilo_ui_base::widget::ResourceOperationStart::Completed(output.clone()),
            ),
            Ok(_) if refusal.is_none() => Ok(tilo_ui_base::widget::ResourceOperationStart::Accepted),
            _ => Err(tilo_actions::ApiError::Failed(
                refusal
                    .as_ref()
                    .map(|refusal| refusal.message.clone())
                    .unwrap_or_default(),
            )),
        };
        let Some(continuation) = self.actions.acting_for.get_mut(&running) else {
            self.follow_up_settled(&control, running, &ended);
            return true;
        };
        continuation.outstanding.remove(&request_id);
        if continuation.refused.is_none() {
            continuation.refused = refusal;
        }
        let waits = !continuation.outstanding.is_empty() || continuation.deferred.is_none();
        self.follow_up_settled(&control, running, &ended);
        if waits {
            return true;
        }
        let Some(continuation) = self.actions.acting_for.remove(&running) else {
            return true;
        };
        let Some((wire_id, update)) = continuation.deferred.clone() else {
            return true;
        };
        let update = continuation.settle(update);
        self.settle_reply(wire_id, update);
        true
    }
}
