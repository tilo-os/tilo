//! The person's own confirmation for someone acting for them.
//!
//! An agent, or an application, never confirms an action by its own word:
//! where an action asks for confirmation, or the workspace's grants let the
//! caller take it only after asking, the person who admitted the
//! connection is asked on their own device, on a sheet only the host draws.
//! While they decide, the view the action would run in shows the caller
//! waiting for them. Their answer is a token naming that one invocation
//! ([`Confirmation::Approved`]); the broker retries the invocation with it.
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
    ActionCallError, Caller, Confirmation, ConfirmedInvocation, InvocationReceipt, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionMessage, serve,
};

use super::action_provider::{ConnectionActionProvider, RowScope};
use super::execution_activity::ShownActor;
use super::{AppBrokerState, BrokerActor, CrdtRoute};
use crate::apps::app_actions::{AppActionAnswer, AppActionConsentRequest};

/// These Store actions disclose signed content outside the workspace.
/// Their confirmation must come from the host, not the publishing app.
pub(super) fn requires_store_approval(app: &str, action: &str) -> bool {
    app == "so.tilo.store" && matches!(action, "submit-report" | "appeal")
}

/// The id a consent question names `caller` by in `app_id`.
fn asker_id(caller: &Caller) -> String {
    let acting = caller.acting();
    if let Some(agent) = acting.agent_id() {
        agent.to_owned()
    } else if let Some(app) = acting.app() {
        app.to_string()
    } else if let Some(bot) = acting.bot_id() {
        format!("bot:{bot}")
    } else {
        acting.person_id().unwrap_or_default().to_owned()
    }
}

/// The Store disclosure `request` puts to the person, signed as `caller`'s
/// person or this device's in `workspace`.
///
/// # Errors
/// No identity to sign with, or an `operation` that does not decode: the
/// request is refused with why.
fn store_disclosure(
    caller: &Caller,
    workspace: &str,
    request: &InvokeRequest,
) -> Result<crate::apps::app_actions::ModerationConsentPreview, ActionCallError> {
    let signing_key = match caller.user_public_key() {
        Some(key) => *key,
        None => tilo_identity::trusted_device::current(workspace)
            .map(|(_, certificate)| certificate.user_public_key)
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("no identity to sign the Store disclosure with: {error}"),
            })?,
    };
    crate::apps::app_actions::ModerationConsentPreview::from_argument(
        &request.target.address.action,
        &request.argument,
        &signing_key,
    )
    .map_err(|reason| ActionCallError::InvalidRequest { reason })
}

/// The grant allowing the agent session `caller` works in every later step
/// like `descriptor` of `app`, given by `person`: `None` for a caller in no
/// agent session, and for a privileged step, which is asked every time.
fn session_answer(
    caller: &Caller,
    workspace: &str,
    person: &str,
    (app, descriptor): (&str, &tilo_actions::ActionDescriptor),
) -> Option<tilo_action_policy::Grant> {
    let acting = caller.acting();
    let (session, agent) = acting.agent_session.as_ref().zip(acting.agent_id())?;
    let need = descriptor.contract.need();
    if need.privileged {
        return None;
    }
    let scope = crate::apps::services::session_answer_scope(
        app,
        (&descriptor.resource_kind, &descriptor.address.action),
        descriptor.contract.asks_for_argument(None),
    );
    Some(tilo_action_policy::Grant::new(
        crate::apps::services::session_answer_id(session.as_str(), (&scope, need.kind)),
        tilo_action_policy::Grantee::agent(agent.to_owned()),
        scope.node(workspace),
        tilo_action_policy::KindSet::of(need.kind),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    ))
}

/// What the person is asked, and what their answer is bound to.
pub(super) struct Question {
    pub(super) request: AppActionConsentRequest,
    /// The grant allowing an agent session every later step like this one,
    /// where its person may give it.
    pub(super) answer: Option<tilo_action_policy::Grant>,
    actor: BrokerActor,
    binding: super::ApprovalBinding,
    /// The view the caller is shown waiting in, when the action runs in one.
    shown: Option<(CrdtRoute, ShownActor)>,
}

/// Answer `message` from `caller`, a connection `person` admitted. An
/// invocation refused for want of a confirmation the caller may not give
/// itself is put to `person` first, and retried with their answer; with no
/// `person` to ask, that refusal is the answer.
pub(super) async fn serve_answering(
    state: Arc<AppBrokerState>,
    provider: ConnectionActionProvider,
    caller: Caller,
    person: Option<String>,
    message: RemoteActionMessage,
) -> Option<RemoteActionMessage> {
    let first = serve(&provider, &caller, message.clone())?.await?;
    let RemoteActionMessage::Invoke(request) = message else {
        return Some(first);
    };
    let Some(person) = person.filter(|_| needs_approval(&first)) else {
        return Some(first);
    };
    let approved = match state
        .request_approval(&caller, Some(&person), &request)
        .await
    {
        Some(Ok(approved)) => approved,
        Some(Err(ActionCallError::ConfirmationRequired { .. })) | None => return Some(first),
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
    approved.apply(&mut retried);
    serve(&provider, &caller, RemoteActionMessage::Invoke(retried))?.await
}

/// Whether `reply` refused an invocation for want of a confirmation.
fn needs_approval(reply: &RemoteActionMessage) -> bool {
    matches!(
        reply,
        RemoteActionMessage::Receipt(InvocationReceipt {
            status: ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { .. }
            },
            ..
        })
    )
}

impl AppBrokerState {
    /// Resolve only this provider's live publication. Host-owned providers
    /// have no application publication; unrelated discovery cannot change them.
    pub(super) fn approval_publication(&self, provider: &str) -> (u64, u64) {
        let peer = if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            self.canonical_token_directory(provider)
                .ok()
                .map(|root| root.peer_id)
        } else if super::launchable::is_launchable_provider(provider) {
            self.launchable.surface_for(provider).and_then(|retained| {
                self.publishers
                    .surfaces
                    .lock()
                    .ok()?
                    .values()
                    .find_map(|surface| {
                        (surface.live
                            && surface.app_id == retained.app_id
                            && surface.route == retained.route)
                            .then_some(surface.peer_id)
                    })
            })
        } else {
            tilo_failure::ignored!(
                "a provider that is not a peer number names a canonical, launchable or provider one",
                provider.parse::<u64>().ok()
            )
        };
        peer.and_then(|peer| {
            self.approval_publications
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&peer)
                .copied()
        })
        .unwrap_or_default()
    }

    /// Capture before asking, and reconstruct from the original request at
    /// admission. Republishing this provider invalidates the pending answer.
    pub(crate) fn approval_binding(
        &self,
        actor: &tilo_action_policy::MemberId,
        workspace: &str,
        request: &InvokeRequest,
    ) -> super::ApprovalBinding {
        super::ApprovalBinding {
            actor: actor.clone(),
            provider: request.target.provider.clone(),
            workspace: workspace.into(),
            revision: request.revision.clone(),
            publication: self.approval_publication(&request.target.provider),
            invocation: request.invocation,
            resource: request.target.address.resource.clone(),
            action: request.target.address.action.clone(),
            argument_digest: super::argument_digest(request.argument.to_slot().as_deref()),
        }
    }

    /// The binding a confirmation of `offered`, invoked by `actor` with `argument` as
    /// `invocation`, is held to.
    #[cfg(test)]
    pub(super) fn offered_approval_binding(
        &self,
        actor: &tilo_action_policy::MemberId,
        offered: &(super::PublishedSurface, super::PublishedAction),
        argument: Option<&str>,
        invocation: tilo_actions::InvocationId,
    ) -> super::ApprovalBinding {
        super::ApprovalBinding {
            actor: actor.clone(),
            provider: offered.0.peer_id.to_string(),
            workspace: offered.0.route.cluster_id.clone(),
            revision: None,
            publication: self.approval_publication(&offered.0.peer_id.to_string()),
            invocation,
            resource: offered.1.resource.clone(),
            action: offered.1.action.clone(),
            argument_digest: super::argument_digest(argument),
        }
    }

    pub(super) fn consume_answer(
        &self,
        confirmation: Option<&Confirmation>,
        binding: &super::ApprovalBinding,
    ) -> bool {
        match confirmation {
            Some(Confirmation::Approved(token)) => self.take_approval(token, binding),
            _ => false,
        }
    }

    /// Ask the person `caller` acts for — `person`, or the person its
    /// delegation names — to confirm `request` on their own device, showing
    /// the caller waiting meanwhile. The answer is the token naming their
    /// confirmation of that one invocation.
    pub fn request_approval(
        self: &Arc<Self>,
        caller: &Caller,
        person: Option<&str>,
        request: &InvokeRequest,
    ) -> tilo_actions::remote::Pending<Result<ConfirmedInvocation, ActionCallError>> {
        use tilo_actions::remote::Pending;
        let Some(person) = (if let Some(id) = caller.acting().person_id() {
            // A direct caller cannot nominate another person to answer.
            Some(id.to_owned())
        } else {
            person
                .map(str::to_owned)
                .or_else(|| caller.acting_for_person().map(str::to_owned))
                .or_else(|| {
                    caller
                        .user_public_key()
                        .map(|root| super::person_id_for_root(*root))
                })
        }) else {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        };
        // A foreign device is not the person's: they are asked on one of
        // their own devices, which draws the question and answers it.
        if Self::asks_on_own_device(caller) {
            let binding = self.approval_binding(
                &tilo_action_policy::MemberId::person(person),
                caller.workspace().unwrap_or_default(),
                request,
            );
            // The other device shows no draft, so nothing on it is changed.
            return self
                .own_device_approval(caller, &binding, request)
                .map(|answer| answer.map(ConfirmedInvocation::unchanged));
        }
        let question = match self.approval_question(caller, &person, request) {
            Ok(Some(question)) => question,
            Ok(None) => {
                return Pending::ready(Err(if BrokerActor::for_caller(caller).direct() {
                    ActionCallError::confirmation_required()
                } else {
                    ActionCallError::UnknownAction
                }));
            }
            Err(refused) => return Pending::ready(Err(refused)),
        };
        if question.actor.direct()
            && !requires_store_approval(&question.request.target_app, &question.binding.action)
        {
            // Other direct actions keep confirming on the caller's screen.
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        }
        let Some(answer) = self.ask_person(&question) else {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
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
                    completer.complete(Err(ActionCallError::Unavailable {
                        reason: "the confirmation sheet closed without the person's answer".into(),
                    }));
                    return;
                };
                if !answer.allowed() {
                    // Allowed, the invocation's own progress takes the
                    // waiting's place.
                    state.show_waiting(&question, &request, false);
                    completer.complete(Err(ActionCallError::NotPermitted));
                    return;
                }
                completer.complete(state.approval_answer(&question, &request, &answer.edits));
            });
        if spawned.is_err() {
            return Pending::ready(Err(ActionCallError::Unavailable {
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
    pub(super) fn approval_answer(
        &self,
        question: &Question,
        request: &InvokeRequest,
        edits: &[tilo_actions::DraftEdit],
    ) -> Result<ConfirmedInvocation, ActionCallError> {
        let mut binding = question.binding.clone();
        let arguments = if edits.is_empty() {
            None
        } else {
            let refused = |reason: String| ActionCallError::InvalidRequest { reason };
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
            .approval_confirmation(&binding)
            .ok_or(ActionCallError::confirmation_required())?;
        Ok(ConfirmedInvocation {
            confirmation: Confirmation::Approved(token),
            arguments,
        })
    }

    /// Resolve a directory's original identity and selected content revision.
    fn approval_directory(
        &self,
        provider: &str,
        workspace: &str,
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
                .find(|candidate| candidate.peer_id == peer && candidate.cluster_id == workspace)?
        };
        (directory.cluster_id == workspace).then_some(directory)
    }

    /// The question `request` puts to `person`: the action as the catalogue
    /// offers it to `caller`. `Ok(None)` for an action not offered; an error
    /// refuses a request the person cannot be asked about as sent (a Store
    /// disclosure whose operation does not decode), with the reason.
    pub(super) fn approval_question(
        &self,
        caller: &Caller,
        person: &str,
        request: &InvokeRequest,
    ) -> Result<Option<Question>, ActionCallError> {
        let Some(workspace) = caller.workspace() else {
            return Ok(None);
        };
        let actor = self.caller_actor(caller, workspace);
        let binding = self.approval_binding(actor.member(), workspace, request);
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
        let (answer, app_id, labels, kind, risk, draft) = if let Some(row) = offered {
            let Some(descriptor) = row.action else {
                return Ok(None);
            };
            let draft = super::rendered_draft(&descriptor, &request.argument);
            (
                session_answer(caller, workspace, person, (&row.app_id, &descriptor)),
                row.app_id,
                (descriptor.resource_label, descriptor.action_label),
                Some(descriptor.contract.kind),
                descriptor.contract.risk,
                draft,
            )
        } else {
            let Some(directory) =
                self.approval_directory(provider, workspace, request.revision.as_deref())
            else {
                return Ok(None);
            };
            (
                None,
                directory.app_id,
                (address.resource.clone(), address.action.clone()),
                None,
                tilo_actions::RiskLevel::Low,
                None,
            )
        };
        let argument = request.argument.to_slot();
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
                    argument.as_deref(),
                )?;
                Some((surface.route.clone(), shown, action.contract.kind))
            });
        let kind = kind
            .or_else(|| shown.as_ref().map(|&(_, _, kind)| kind))
            .unwrap_or(tilo_actions::ActionKind::Undoable);
        // Only the Store's typed external actions use this disclosure. A broken
        // payload or unavailable identity must not fall back to a generic prompt.
        let moderation = if requires_store_approval(&app_id, &address.action) {
            Some(store_disclosure(caller, workspace, request)?)
        } else {
            None
        };
        Ok(Some(Question {
            request: AppActionConsentRequest {
                handle: format!("confirm:{}", request.invocation.0),
                workspace: workspace.to_owned(),
                person: person.to_owned(),
                app_id: asker_id(caller),
                agent_session: None,
                target_app: app_id,
                resource_label: labels.0,
                action_label: labels.1,
                kind,
                risk,
                confirming: true,
                moderation,
                scope: answer
                    .as_ref()
                    .and_then(|_| caller.agent_name().map(str::to_owned)),
                draft,
                ask: None,
            },
            binding,
            answer,
            actor,
            shown: shown.map(|(route, shown, _)| (route, shown)),
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
