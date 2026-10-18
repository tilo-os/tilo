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

/// These Store actions disclose signed content outside the workspace.
/// Their confirmation must come from the host, not the publishing app.
pub(super) fn requires_store_witness(app: &str, action: &str) -> bool {
    app == "so.tilo.store" && matches!(action, "submit-report" | "appeal")
}

/// What the person is asked, and what their answer is bound to.
struct Question {
    request: AppActionConsentRequest,
    actor: BrokerActor,
    binding: super::WitnessBinding,
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
    if !needs_witness(&first) {
        return Some(first);
    }
    let witnessed = match state
        .request_witness(&caller, Some(&person), &request)
        .await
    {
        Some(Ok(witnessed)) => witnessed,
        Some(Err(ActionHostError::ConfirmationRequired)) | None => return Some(first),
        Some(Err(error)) => {
            return Some(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Rejected { error },
            }));
        }
    };
    let mut retried = request;
    retried.confirmation = Some(witnessed);
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
    /// Resolve only this provider's live publication. Host-owned providers
    /// have no application publication; unrelated discovery cannot change them.
    pub(super) fn witness_publication(&self, provider: &str) -> (u64, u64) {
        let peer = if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            self.canonical_token_directory(provider)
                .ok()
                .map(|root| root.peer_id)
        } else if super::launchable::is_launchable_provider(provider) {
            self.launchable.surface_for(provider).and_then(|retained| {
                self.surfaces.lock().ok()?.values().find_map(|surface| {
                    (surface.live
                        && surface.app_id == retained.app_id
                        && surface.route == retained.route)
                        .then_some(surface.peer_id)
                })
            })
        } else {
            provider.parse::<u64>().ok()
        };
        peer.and_then(|peer| {
            self.witness_publications
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&peer)
                .copied()
        })
        .unwrap_or_default()
    }

    /// Capture before asking, and reconstruct from the original request at
    /// admission. Republishing this provider invalidates the pending answer.
    pub(crate) fn witness_binding(
        &self,
        actor: &str,
        room: &str,
        request: &InvokeRequest,
    ) -> super::WitnessBinding {
        super::WitnessBinding {
            actor: actor.into(),
            provider: request.target.provider.clone(),
            room: room.into(),
            revision: request.revision.clone(),
            publication: self.witness_publication(&request.target.provider),
            invocation: request.invocation,
            resource: request.target.address.resource.clone(),
            action: request.target.address.action.clone(),
            argument_digest: super::argument_digest(request.argument.to_slot().as_deref()),
        }
    }

    pub(super) fn native_witness_binding(
        &self,
        actor: &str,
        offered: &(super::PublishedSurface, super::PublishedAction),
        argument: Option<&str>,
        invocation: tilo_actions::InvocationId,
    ) -> super::WitnessBinding {
        super::WitnessBinding {
            actor: actor.into(),
            provider: offered.0.peer_id.to_string(),
            room: offered.0.route.cluster_id.clone(),
            revision: None,
            publication: self.witness_publication(&offered.0.peer_id.to_string()),
            invocation,
            resource: offered.1.resource.clone(),
            action: offered.1.action.clone(),
            argument_digest: super::argument_digest(argument),
        }
    }

    pub(super) fn consume_answer(
        &self,
        confirmation: Option<&Confirmation>,
        binding: &super::WitnessBinding,
    ) -> bool {
        match confirmation {
            Some(Confirmation::Witnessed(token)) => self.take_witness(token, binding),
            _ => false,
        }
    }

    /// A question must not outlive the publisher or the contract it showed.
    pub(super) fn check_native_store_offer(
        &self,
        offered: &(super::PublishedSurface, super::PublishedAction),
    ) -> Result<(), String> {
        let current = self
            .surfaces
            .lock()
            .map_err(|_| "task surface unavailable")?
            .get(&offered.0.peer_id)
            .and_then(|surface| surface.action_projection(&offered.1.resource, &offered.1.action))
            .ok_or("Store action retired while awaiting confirmation")?;
        if !current.0.live
            || current.0.route != offered.0.route
            || current.0.app_id != offered.0.app_id
            || current.1.contract != offered.1.contract
            || current.1.params != offered.1.params
        {
            return Err("Store action changed while awaiting confirmation".into());
        }
        Ok(())
    }

    /// Local RPC approval material is not a disclosure witness. Only the host
    /// may ask the admitted person's device about this exact external payload.
    pub(super) async fn local_store_witness(
        &self,
        admitted: &super::AdmittedJoinIdentity,
        publication: &(super::PublishedSurface, super::PublishedAction),
        argument: Option<&str>,
        binding: &super::WitnessBinding,
    ) -> Result<Confirmation, String> {
        let (surface, offered) = publication;
        if !requires_store_witness(&surface.app_id, &offered.action) {
            return Err("external disclosure witness requested for another action".into());
        }
        let actor = admitted.broker_actor();
        let argument_value = argument.map_or(tilo_actions::remote::InvokeArgument::None, |text| {
            tilo_actions::remote::InvokeArgument::Text(text.into())
        });
        let moderation = crate::app_actions::ModerationConsentPreview::from_argument(
            &offered.action,
            &argument_value,
            &admitted.verified.user_public_key,
        )
        .ok_or("Store disclosure arguments are invalid")?;
        let question = Question {
            request: AppActionConsentRequest {
                handle: format!("confirm:{}", binding.invocation.0),
                room: surface.route.cluster_id.clone(),
                person: super::principal_id_for_root(admitted.verified.user_public_key),
                app_id: actor.id().into(),
                target_app: surface.app_id.clone(),
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                resource_label: offered.resource_label.clone(),
                action_label: offered.action_label.clone(),
                risk: offered.contract.risk,
                needs_confirmation: true,
                confirming: true,
                moderation: Some(moderation),
            },
            actor,
            binding: binding.clone(),
            shown: None,
        };
        let answer = self
            .ask_person(&question)
            .ok_or("host confirmation unavailable")?
            .await
            .map_err(|_| "host confirmation ended")?;
        if answer == AppActionConsent::Deny {
            return Err("Store disclosure declined".into());
        }
        self.witness_confirmation(&question.binding)
            .map(Confirmation::Witnessed)
            .ok_or_else(|| "host confirmation unavailable".into())
    }

    /// Native task approval is not a disclosure witness. Ask on the same typed
    /// host sheet as ordinary Store invocations, after any Task Center edits.
    /// Dispatch consumes the returned witness, and revalidates task authority.
    pub(super) async fn native_store_witness(
        &self,
        (run, lease, confirmed): (&super::PendingTaskRun, &str, bool),
        publication: &(super::PublishedSurface, super::PublishedAction),
        argument: Option<&str>,
        caller: &super::durable_local_invocation::LocalCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<String>, String> {
        let (surface, offered) = publication;
        let app = surface.app_id.as_str();
        if !requires_store_witness(app, &offered.action) {
            return Ok(None);
        }
        let mut binding = run.native.clone().ok_or("native task binding absent")?;
        binding.revision = runtime_collab::native_task_revision(lease)
            .ok_or("invalid native lease")?
            .1;
        let actor = binding.actor()?;
        let service = self
            .native_tasks(&run.cluster_id)
            .ok_or("native task owner unavailable")?;
        service.authorize_action(
            &run.cluster_id,
            &run.agent_id,
            &crate::services::NativeTaskActionRequirement {
                binding: Some(binding.clone()),
                app: app.into(),
                resource_kind: offered.resource_kind.clone(),
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                need: offered.contract.need(),
                consequential: !matches!(
                    offered.contract.confirmation_for_argument(argument),
                    tilo_actions::ConfirmationPolicy::None
                ),
                confirmed,
            },
        )?;
        let current = caller.current(self)?;
        let argument_value = argument.map_or(tilo_actions::remote::InvokeArgument::None, |text| {
            tilo_actions::remote::InvokeArgument::Text(text.into())
        });
        let moderation = crate::app_actions::ModerationConsentPreview::from_argument(
            &offered.action,
            &argument_value,
            &current.verified.user_public_key,
        )
        .ok_or("Store disclosure arguments are invalid")?;
        let question = Question {
            request: AppActionConsentRequest {
                handle: format!("confirm:{}", uuid::Uuid::new_v4()),
                room: run.cluster_id.clone(),
                person: binding.starter.ok_or("native task starter absent")?,
                app_id: run.agent_id.clone(),
                target_app: app.into(),
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                resource_label: offered.resource_label.clone(),
                action_label: offered.action_label.clone(),
                risk: offered.contract.risk,
                needs_confirmation: true,
                confirming: true,
                moderation: Some(moderation),
            },
            binding: self.native_witness_binding(actor.id(), publication, argument, invocation),
            actor,
            shown: None,
        };
        let answer = self
            .ask_person(&question)
            .ok_or("host confirmation unavailable")?
            .await
            .map_err(|_| "host confirmation ended")?;
        if answer == AppActionConsent::Deny {
            return Err("Store disclosure declined".into());
        }
        self.witness_confirmation(&question.binding)
            .map(Some)
            .ok_or_else(|| "host confirmation unavailable".into())
    }

    /// Ask the person `caller` acts for — `person`, or the person its
    /// delegation names — to confirm `request` on their own device, showing
    /// the caller waiting meanwhile. The answer is the token naming their
    /// confirmation of that one invocation.
    pub fn request_witness(
        self: &Arc<Self>,
        caller: &Caller,
        person: Option<&str>,
        request: &InvokeRequest,
    ) -> tilo_actions::remote::Pending<Result<Confirmation, ActionHostError>> {
        use tilo_actions::remote::Pending;
        let Some(person) = (match caller.principal() {
            // A direct caller cannot nominate another person to answer.
            tilo_actions::remote::CallerPrincipal::Person(id) => Some(id.clone()),
            _ => person
                .map(str::to_owned)
                .or_else(|| caller.acting_for_person().map(str::to_owned))
                .or_else(|| {
                    caller
                        .user_public_key()
                        .map(|root| super::principal_id_for_root(*root))
                }),
        }) else {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        };
        let Some(question) = self.witness_question(caller, &person, request) else {
            return Pending::ready(Err(if BrokerActor::for_caller(caller).direct() {
                ActionHostError::ConfirmationRequired
            } else {
                ActionHostError::UnknownAction
            }));
        };
        if question.actor.direct()
            && !requires_store_witness(&question.request.target_app, &question.request.action)
        {
            // Other direct actions keep confirming on the caller's screen.
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        }
        let Some(answer) = self.ask_person(&question) else {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        };
        self.show_waiting(&question, request, true);
        let (completer, pending) = tilo_actions::remote::pending();
        let state = Arc::clone(self);
        let request = request.clone();
        // "Allow for this task" lets a session-delegated agent's run go on
        // without asking again; it ends with the run.
        let task = match caller.principal() {
            tilo_actions::remote::CallerPrincipal::Agent(agent)
                if question.actor.session_delegated =>
            {
                caller
                    .execution_id()
                    .map(|execution| (agent.clone(), execution))
            }
            _ => None,
        };
        // The person answers in their own time; nothing here holds a
        // runtime while they decide.
        let spawned = std::thread::Builder::new()
            .name("confirmation".into())
            .spawn(move || {
                let answer = answer.blocking_recv().unwrap_or(AppActionConsent::Deny);
                if answer == AppActionConsent::Deny {
                    // Allowed, the invocation's own progress takes the
                    // waiting's place.
                    state.show_waiting(&question, &request, false);
                    completer.complete(Err(ActionHostError::NotPermitted));
                    return;
                }
                if answer == AppActionConsent::Session
                    && let Some((agent, execution)) = &task
                {
                    super::allow_for_task(
                        agent,
                        *execution,
                        (&question.request.room, &question.request.target_app),
                        &question.request.person,
                    );
                }
                completer.complete(
                    state
                        .witness_confirmation(&question.binding)
                        .map(Confirmation::Witnessed)
                        .ok_or(ActionHostError::ConfirmationRequired),
                );
            });
        if spawned.is_err() {
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "the confirmation could not be asked".into(),
            }));
        }
        pending
    }

    /// Resolve a directory's original identity and selected content revision.
    fn witness_directory(
        &self,
        provider: &str,
        room: &str,
        revision: Option<&str>,
    ) -> Option<super::ActionDirectoryRoot> {
        // A directory's contents can move without replacing its root.
        revision.filter(|revision| !revision.is_empty())?;
        let directory = if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            self.canonical_token_directory(provider).ok()?
        } else {
            let peer = provider.parse::<u64>().ok()?;
            self.action_directory_roots()
                .into_iter()
                .find(|candidate| candidate.peer_id == peer && candidate.cluster_id == room)?
        };
        (directory.cluster_id == room).then_some(directory)
    }

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
        let binding = self.witness_binding(actor.id(), room, request);
        let provider = request.target.provider.as_str();
        let address = &request.target.address;
        // The action as the catalogue offers it, or, for an application
        // that publishes a directory, as the person's request names it.
        let offered = self
            .eager_catalogue(caller, RowScope::Provider(provider))
            .0
            .into_iter()
            .find(|row| {
                row.provider == provider
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|descriptor| &descriptor.address == address)
            });
        let (app_id, labels, risk) = if let Some(row) = offered {
            let descriptor = row.action?;
            (
                row.app_id,
                (descriptor.resource_label, descriptor.action_label),
                descriptor.contract.risk,
            )
        } else {
            let directory = self.witness_directory(provider, room, request.revision.as_deref())?;
            (
                directory.app_id,
                (address.resource.clone(), address.action.clone()),
                tilo_actions::RiskLevel::Low,
            )
        };
        let shown = self
            .live_surfaces(RowScope::Provider(provider))
            .into_iter()
            .find_map(|surface| {
                let action = surface.actions.iter().find(|action| {
                    action.resource == address.resource && action.action == address.action
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
            tilo_actions::remote::CallerPrincipal::Bot(bot) => format!("bot:{bot}"),
            tilo_actions::remote::CallerPrincipal::Person(id) => id.clone(),
        };
        // Only the Store's typed external actions use this disclosure. A broken
        // payload or unavailable identity must not fall back to a generic prompt.
        let moderation = if requires_store_witness(&app_id, &address.action) {
            let signing_root = caller.user_public_key().copied().or_else(|| {
                tilo_identity_host::broker::trusted_current_device(room)
                    .ok()
                    .map(|(_, certificate)| certificate.user_public_key)
            })?;
            Some(crate::app_actions::ModerationConsentPreview::from_argument(
                &address.action,
                &request.argument,
                &signing_root,
            )?)
        } else {
            None
        };
        Some(Question {
            request: AppActionConsentRequest {
                handle: format!("confirm:{}", request.invocation.0),
                room: room.to_owned(),
                person: person.to_owned(),
                app_id: asker,
                target_app: app_id,
                resource: address.resource.clone(),
                action: address.action.clone(),
                resource_label: labels.0,
                action_label: labels.1,
                risk,
                needs_confirmation: true,
                confirming: true,
                moderation,
            },
            binding,
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
