//! The person's own confirmation for someone acting for them.
//!
//! An agent, or an application, never confirms an action by its own word:
//! where an action asks for confirmation, or the room's grants let the
//! caller take it only after asking, the person who admitted the
//! connection is asked on their own device, on a sheet only the host draws.
//! While they decide, the view the action would run in shows the caller
//! waiting for them. Their answer is a token naming that one invocation
//! ([`Confirmation::Witnessed`]); the broker retries the invocation with it.
//! Nobody present means nobody to ask: the invocation stays refused.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHostError, Caller, Confirmation, InvocationReceipt, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, serve,
};

use super::action_host::{ConnectionActionHost, RowScope};
use super::execution_activity::ShownActor;
use super::{BrokerActor, CapnpBrokerState, CrdtRoute};
use crate::app_actions::{AppActionConsent, AppActionConsentRequest};

/// What the person is asked, and what their answer is bound to.
struct Question {
    request: AppActionConsentRequest,
    actor: BrokerActor,
    /// The view the caller is shown waiting in, when the action runs in one.
    shown: Option<(CrdtRoute, ShownActor)>,
}

/// Answer `message` from `caller`, a connection `person` admitted. An
/// invocation refused for want of a confirmation the caller may not give
/// itself is put to `person` first, and retried with their answer.
pub(super) async fn serve_answering(
    state: Arc<CapnpBrokerState>,
    host: ConnectionActionHost,
    caller: Caller,
    person: String,
    message: RemoteActionMessage,
) -> Option<RemoteActionMessage> {
    let first = serve(&host, &caller, message.clone())?.await?;
    let RemoteActionMessage::Invoke(request) = message else {
        return Some(first);
    };
    if !needs_witness(&first) || BrokerActor::for_caller(&caller).direct() {
        return Some(first);
    }
    let Some(question) = state.witness_question(&caller, &person, &request) else {
        return Some(first);
    };
    let Some(answer) = state.ask_person(&question) else {
        return Some(first);
    };
    state.show_waiting(&question, &request, true);
    let answer = answer.await.unwrap_or(AppActionConsent::Deny);
    if answer == AppActionConsent::Deny {
        // Allowed, the invocation's own progress takes the waiting's place.
        state.show_waiting(&question, &request, false);
        return Some(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Rejected {
                error: ActionHostError::NotPermitted,
            },
        }));
    }
    let token = state.witness_confirmation(
        question.actor.id(),
        (&question.request.resource, &question.request.action),
        request.argument.to_slot().as_deref(),
    )?;
    let mut retried = request;
    retried.confirmation = Some(Confirmation::Witnessed(token));
    serve(&host, &caller, RemoteActionMessage::Invoke(retried))?.await
}

/// Whether `reply` refused an invocation for want of a confirmation.
fn needs_witness(reply: &RemoteActionMessage) -> bool {
    matches!(
        reply,
        RemoteActionMessage::Receipt(InvocationReceipt {
            status: ReceiptStatus::Rejected {
                error: ActionHostError::ConfirmationRequired
            },
            ..
        })
    )
}

impl CapnpBrokerState {
    /// The question `request` puts to `person`: the action as the catalogue
    /// offers it to `caller`. `None` for an action not offered.
    fn witness_question(
        &self,
        caller: &Caller,
        person: &str,
        request: &InvokeRequest,
    ) -> Option<Question> {
        let room = caller.room()?;
        let actor = self.caller_actor(caller, room);
        let provider = request.target.provider.as_str();
        let row = self
            .eager_catalogue(caller, RowScope::Provider(provider))
            .0
            .into_iter()
            .find(|row| {
                row.provider == provider
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|descriptor| descriptor.address == request.target.address)
            })?;
        let descriptor = row.action?;
        let shown = self
            .live_surfaces(RowScope::Provider(provider))
            .into_iter()
            .find_map(|surface| {
                let action = surface.actions.iter().find(|action| {
                    action.resource == descriptor.address.resource
                        && action.action == descriptor.address.action
                })?;
                let shown = ShownActor::of(
                    Some(caller),
                    &actor,
                    request.context.as_ref(),
                    &surface,
                    action,
                )?;
                Some((surface.route.clone(), shown))
            });
        let asker = match caller.principal() {
            tilo_actions::remote::CallerPrincipal::Agent(agent) => agent.clone(),
            tilo_actions::remote::CallerPrincipal::App(app) => app.clone(),
            tilo_actions::remote::CallerPrincipal::Person(id) => id.clone(),
        };
        Some(Question {
            request: AppActionConsentRequest {
                handle: format!("confirm:{}", request.invocation.0),
                room: room.to_owned(),
                person: person.to_owned(),
                app_id: asker,
                target_app: row.app_id,
                resource: descriptor.address.resource.clone(),
                action: descriptor.address.action.clone(),
                resource_label: descriptor.resource_label.clone(),
                action_label: descriptor.action_label.clone(),
                risk: descriptor.contract.risk,
                needs_confirmation: true,
                confirming: true,
            },
            actor,
            shown,
        })
    }

    /// Put `question` to the person on their own device. `None` when nobody
    /// is there to answer.
    fn ask_person(
        &self,
        question: &Question,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionConsent>> {
        let prompter = self
            .app_action_consent
            .read()
            .ok()
            .and_then(|prompter| prompter.clone())?;
        prompter.ask(question.request.clone())
    }

    /// Show the caller waiting for the person in the view the action would
    /// run in (`waiting`), or no longer waiting: the person declined.
    fn show_waiting(&self, question: &Question, request: &InvokeRequest, waiting: bool) {
        let Some((route, shown)) = &question.shown else {
            return;
        };
        let phase = if waiting {
            tilo_actions::InteractionPhase::Waiting {
                reason: Some(format!(
                    "waiting for {} to confirm",
                    question.request.person
                )),
            }
        } else {
            tilo_actions::InteractionPhase::Cancelled { message: None }
        };
        self.publish_interaction(route, shown, request.invocation, phase);
    }
}
