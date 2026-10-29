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
//!
//! The sheet shows the invocation as a draft, and the person may change its
//! fields before allowing it. Their edits make new arguments
//! ([`tilo_actions::Draft::apply_edits`]), and the token is bound to those,
//! with the same invocation id: the caller is handed them
//! ([`ConfirmedInvocation`]) and invokes again with exactly them. Admission
//! never swaps arguments; arguments other than the ones allowed find no
//! token and are refused.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHostError, Caller, Confirmation, ConfirmedInvocation, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionMessage, serve,
};

use super::action_host::{ConnectionActionHost, RowScope};
use super::execution_activity::ShownActor;
use super::{BrokerActor, CapnpBrokerState, CrdtRoute};
use crate::app_actions::{AppActionAnswer, AppActionConsentRequest};

/// These Store actions disclose signed content outside the workspace.
/// Their confirmation must come from the host, not the publishing app.
pub(super) fn requires_store_witness(app: &str, action: &str) -> bool {
    app == "so.tilo.store" && matches!(action, "submit-report" | "appeal")
}

/// The Store disclosure `request` puts to the person, signed as `caller`'s
/// person or this device's in `room`.
///
/// # Errors
/// No identity to sign with, or an `operation` that does not decode: the
/// request is refused with why.
fn store_disclosure(
    caller: &Caller,
    room: &str,
    request: &InvokeRequest,
) -> Result<crate::app_actions::ModerationConsentPreview, ActionHostError> {
    let signing_key = match caller.user_public_key() {
        Some(key) => *key,
        None => tilo_identity_host::broker::trusted_current_device(room)
            .map(|(_, certificate)| certificate.user_public_key)
            .map_err(|error| ActionHostError::Unavailable {
                reason: format!("no identity to sign the Store disclosure with: {error}"),
            })?,
    };
    crate::app_actions::ModerationConsentPreview::from_argument(
        &request.target.address.action,
        &request.argument,
        &signing_key,
    )
    .map_err(|reason| ActionHostError::InvalidRequest { reason })
}

/// What the person is asked, and what their answer is bound to.
pub(super) struct Question {
    pub(super) request: AppActionConsentRequest,
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
    // Invoked again as the person allowed it: with their confirmation, and
    // with the arguments as they changed them, which it is bound to.
    let mut retried = request;
    witnessed.apply(&mut retried);
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
            // optional-decode: a provider that is not a peer number names a canonical, launchable or host provider, not a live publication
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
        let mut binding = run.native.clone();
        binding.revision = runtime_collab::native_task_revision(lease)
            .ok_or("invalid native lease")?
            .1;
        let actor = binding.actor()?;
        self.authorize_task_action(
            run,
            &crate::services::NativeTaskActionRequirement {
                binding: binding.clone(),
                app: app.into(),
                resource_kind: offered.resource_kind.clone(),
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                need: offered.contract.need(),
                consequential: offered.contract.asks_for_argument(argument),
                attended: offered.contract.is_attended(),
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
        )?;
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
                preview: None,
                scope: None,
                draft: None,
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
        if !answer.allowed() {
            return Err("Store disclosure declined".into());
        }
        if !answer.edits.is_empty() {
            // A disclosure is confirmed exactly as it is shown.
            return Err("a Store disclosure cannot be changed on the sheet".into());
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
    ) -> tilo_actions::remote::Pending<Result<ConfirmedInvocation, ActionHostError>> {
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
        // A foreign device is not the person's: they are asked on one of
        // their own devices, which draws the question and answers it.
        if Self::asks_on_own_device(caller) {
            let binding = self.witness_binding(&person, caller.room().unwrap_or_default(), request);
            // The other device shows no draft, so nothing on it is changed.
            return self
                .own_device_witness(caller, &binding, request)
                .map(|answer| answer.map(ConfirmedInvocation::unchanged));
        }
        let question = match self.witness_question(caller, &person, request) {
            Ok(Some(question)) => question,
            Ok(None) => {
                return Pending::ready(Err(if BrokerActor::for_caller(caller).direct() {
                    ActionHostError::ConfirmationRequired
                } else {
                    ActionHostError::UnknownAction
                }));
            }
            Err(refused) => return Pending::ready(Err(refused)),
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
        // The person answers in their own time; nothing here holds a
        // runtime while they decide.
        let spawned = std::thread::Builder::new()
            .name("confirmation".into())
            .spawn(move || {
                // Resolved by the sheet: the person's answer, or its sender
                // dropped with the sheet, which answers the caller that no
                // one answered rather than that the person declined.
                let Ok(answer) = answer.blocking_recv() else {
                    state.show_waiting(&question, &request, false);
                    completer.complete(Err(ActionHostError::Unavailable {
                        reason: "the confirmation sheet closed without the person's answer".into(),
                    }));
                    return;
                };
                if !answer.allowed() {
                    // Allowed, the invocation's own progress takes the
                    // waiting's place.
                    state.show_waiting(&question, &request, false);
                    completer.complete(Err(ActionHostError::NotPermitted));
                    return;
                }
                completer.complete(state.witness_answer(&question, &request, &answer.edits));
            });
        if spawned.is_err() {
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "the confirmation could not be asked".into(),
            }));
        }
        pending
    }

    /// The person's allowing answer to `question`, about `request`, as the
    /// confirmation its caller invokes again with: a one-use token bound to
    /// the invocation id and to the exact arguments the person allowed.
    /// Those are `request`'s, as their `edits` to the draft they were shown
    /// change them ([`tilo_actions::Draft::apply_edits`]); the caller is
    /// handed the changed arguments to invoke with, and any others are
    /// refused at admission like a confirmation for other arguments.
    ///
    /// # Errors
    /// The edits change what the draft does not offer for editing, or the
    /// confirmation could not be kept.
    fn witness_answer(
        &self,
        question: &Question,
        request: &InvokeRequest,
        edits: &[tilo_actions::DraftEdit],
    ) -> Result<ConfirmedInvocation, ActionHostError> {
        let mut binding = question.binding.clone();
        let arguments = if edits.is_empty() {
            None
        } else {
            let refused = |reason: String| ActionHostError::InvalidRequest { reason };
            let (Some(draft), InvokeArgument::Json(asked)) =
                (question.request.draft.as_ref(), &request.argument)
            else {
                return Err(refused("nothing in this request can be changed".into()));
            };
            let edited = draft.apply_edits(asked, edits).map_err(refused)?;
            (edited != *asked).then_some(edited)
        };
        if let Some(edited) = &arguments {
            let argument = InvokeArgument::Json(edited.clone());
            binding.argument_digest = super::argument_digest(argument.to_slot().as_deref());
        }
        let token = self
            .witness_confirmation(&binding)
            .ok_or(ActionHostError::ConfirmationRequired)?;
        Ok(ConfirmedInvocation {
            confirmation: Confirmation::Witnessed(token),
            arguments,
        })
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
            // optional-decode: a provider that is not a peer number names a canonical, launchable or host provider, not a live publication
            let peer = provider.parse::<u64>().ok()?;
            self.action_directory_roots()
                .into_iter()
                .find(|candidate| candidate.peer_id == peer && candidate.cluster_id == room)?
        };
        (directory.cluster_id == room).then_some(directory)
    }

    /// The question `request` puts to `person`: the action as the catalogue
    /// offers it to `caller`. `Ok(None)` for an action not offered; an error
    /// refuses a request the person cannot be asked about as sent (a Store
    /// disclosure whose operation does not decode), with the reason.
    pub(super) fn witness_question(
        &self,
        caller: &Caller,
        person: &str,
        request: &InvokeRequest,
    ) -> Result<Option<Question>, ActionHostError> {
        let Some(room) = caller.room() else {
            return Ok(None);
        };
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
        let (app_id, labels, risk, draft) = if let Some(row) = offered {
            let Some(descriptor) = row.action else {
                return Ok(None);
            };
            // The invocation as the person reviews it, field by field: the
            // draft a run's question shows them, rendered the same way.
            let draft = match &request.argument {
                tilo_actions::remote::InvokeArgument::Json(arguments) => {
                    Some(tilo_actions::Draft::render(&descriptor, arguments))
                }
                _ => None,
            }
            .filter(|draft| !draft.fields.is_empty());
            (
                row.app_id,
                (descriptor.resource_label, descriptor.action_label),
                descriptor.contract.risk,
                draft,
            )
        } else {
            let Some(directory) =
                self.witness_directory(provider, room, request.revision.as_deref())
            else {
                return Ok(None);
            };
            (
                directory.app_id,
                (address.resource.clone(), address.action.clone()),
                tilo_actions::RiskLevel::Low,
                None,
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
            Some(store_disclosure(caller, room, request)?)
        } else {
            None
        };
        Ok(Some(Question {
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
                preview: None,
                scope: None,
                draft,
            },
            binding,
            actor,
            shown,
        }))
    }

    /// Put `question` to the person on their own device. `None` when nobody
    /// is there to answer.
    fn ask_person(
        &self,
        question: &Question,
    ) -> Option<tokio::sync::oneshot::Receiver<AppActionAnswer>> {
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
