#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

impl AppBrokerState {
    /// Shared semantic admission; a local mutation report is never delivered.
    #[expect(
        clippy::too_many_lines,
        reason = "a freshly offered live action and its durable identity are consumed into one admission while all checks remain atomic"
    )]
    pub(in crate::collab_app_broker) fn admit_invocation_from_offered_owned(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        provenance: AdmissionProvenance<'_>,
        local_report: Option<&runtime_collab::LocalDocumentActionReport>,
    ) -> Result<StagedJob, super::asks::Refusal> {
        if local_report.is_some()
            && (!offered
                .1
                .operation_kind
                .reports_a_change(offered.1.contract.need().kind)
                || (offered.1.operation_kind != tilo_actions::OperationKind::Document
                    && local_report.is_some_and(|report| !report.mutation_hashes.is_empty()))
                || !matches!(
                    provenance.origin.as_ref(),
                    Some(InvocationCaller::Local { .. })
                ))
        {
            return Err(
                "local action report requires authenticated document or read-only admission".into(),
            );
        }
        if let Some(report) = local_report
            && (report.bound_invocation()? != report.invocation
                || inputs.requested != Some(tilo_actions::InvocationId(report.invocation)))
        {
            return Err("local report invocation does not bind capture".into());
        }
        let typed_local_arguments = local_report
            .map(|report| crate::local_document_action::typed_arguments(report, &offered.1.params))
            .transpose()?;
        let InvocationInputs {
            original_request,
            requested,
            caller,
            execution,
            argument,
            actor,
            confirmation_token,
            context,
        } = inputs;
        let AdmissionProvenance {
            revision,
            origin,
            retained_time,
        } = provenance;
        let continuing = ContinuingCaller::admitted(caller, origin.as_ref());
        let owner = self.invocation_owner(origin.as_ref(), &offered.0.route.cluster_id, actor)?;
        owner.validate()?;
        let session = self.agent_session_of(caller, &offered.0.route.cluster_id);
        // Never trust an envelope's invoker: the host sets it.
        let context = invoker::host_set_invoker(
            context,
            original_request,
            actor,
            caller,
            execution.as_ref(),
        )?;
        // Where an agent works from is what the host verified of its
        // connection: an envelope names no device.
        let context = invoker::working_from(
            context,
            match &session {
                Some(Ok(going)) => going.origin.as_ref(),
                _ => None,
            },
        );
        // The person's own call that their press on the shell's own surfaces made (a palette's
        // row, a menu's item, a shortcut's key) runs as their act in the app it reaches, as a press
        // in its window does: only the attestation this host holds says so, never a caller's
        // context.
        let pressed = requested
            .zip(actor.actor().person_id())
            .is_some_and(|(invocation, person)| self.pressed_call(invocation, person));
        let context = match (context, pressed) {
            (None, false) => None,
            (context, pressed) => Some(tilo_actions::ActionContext {
                pressed,
                ..context.unwrap_or_default()
            }),
        };
        // A certified local origin retains its original registered clock and
        // exact person/workspace through an asynchronous directory lookup. Never
        // recapture a replacement origin at dispatch or accept a payload actor.
        let original_time = if let Some(InvocationCaller::Certified { caller, .. }) = &origin {
            let admitted = self.current_action_caller(caller)?;
            // The person the admission certifies, or the one agent it admitted
            // for them (an MCP session their device handed over): never
            // another actor.
            let admitted_actor = match &admitted.agent_id {
                Some(agent) => {
                    actor.actor().member_id()
                        == Some(&tilo_action_policy::MemberId::agent(
                            delegated_agent_name(agent),
                            admitted.actor_key.clone(),
                        ))
                }
                None => actor.actor().person_id() == Some(admitted.actor_key.as_str()),
            };
            if caller.workspace != offered.0.route.cluster_id || !admitted_actor {
                return Err("original caller does not match the invocation route or actor".into());
            }
            Some(
                caller
                    .policy_time
                    .as_ref()
                    .ok_or("original policy clock unavailable")?,
            )
        } else if let Some(InvocationCaller::NativePerson {
            workspace,
            certificate,
            time,
            ..
        }) = &origin
        {
            self.current_native_person(workspace, certificate)?;
            if workspace != &offered.0.route.cluster_id
                || actor.actor().person_id()
                    != Some(person_id_for_root(certificate.user_public_key).as_str())
            {
                return Err(
                    "original native person does not match the invocation route or actor".into(),
                );
            }
            Some(time)
        } else {
            None
        };
        if original_time.is_some() && retained_time.is_some() {
            return Err("duplicate original invocation ownership".into());
        }
        let local_time = if let Some(InvocationCaller::Local { time, .. }) = &origin {
            self.checked_policy_time(time.as_ref())?;
            time.as_ref()
        } else {
            None
        };
        let retained_time = original_time.or(retained_time);
        let policy_time = match retained_time {
            Some(original) => self.checked_policy_time(Some(original))?.0,
            None => match &origin {
                Some(InvocationCaller::Local { .. }) => local_time.cloned(),
                _ => self.registered_policy_time()?,
            },
        };
        let now = self.checked_policy_time(policy_time.as_ref())?.1 / 1_000_000_000;

        if offered.0.app_id == tilo_desktop_model::commands::APP_ID
            && self
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?
                .values()
                .filter(|pending| pending.peer_id == peer_id)
                .count()
                >= 32
        {
            return Err("desktop invocation capacity exhausted".into());
        }
        // A stream the app has dropped takes nothing: refuse before the
        // journal admits what could never be delivered.
        let sender = self.invocation_stream(peer_id);
        if local_report.is_none() && sender.is_none() {
            return Err("the application has no invocation stream".into());
        }
        if offered.0.route.cluster_id.is_empty() {
            return Err("the application is not attached to a workspace".into());
        }
        if let tilo_actions::Availability::Unavailable { reason, .. } =
            &offered.1.contract.availability
        {
            return Err(format!("the application action is unavailable: {reason}").into());
        }
        // What the person allowed an agent session for every later step like
        // one it asked about: decided with the workspace's grants, as an
        // answer granted there is.
        let answers = match session {
            Some(Ok(going)) => going.answers,
            _ => Vec::new(),
        };
        // A record another workspace holds (a person's profile record) is
        // admitted by that workspace, wherever the window showing it is open.
        let held = self.keeping_workspace(peer_id, &offered.1.resource);
        // Every check of this admission is decided on one committed sample,
        // so a policy change cannot land between the action check and the
        // record check.
        let (asks, spent) = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_workspace(
                held.as_deref().unwrap_or(&offered.0.route.cluster_id),
                |workspace| {
                    let Some(workspace) = workspace else {
                        return Err(if held.is_some() {
                            super::shown_actions::NOT_HERE.to_owned()
                        } else {
                            "workspace no longer hosted".to_owned()
                        });
                    };
                    let answered;
                    let workspace = if answers.is_empty() {
                        workspace
                    } else {
                        let mut copy = workspace.clone();
                        copy.policy.grants.extend(answers.iter().cloned());
                        answered = copy;
                        &answered
                    };
                    if held.is_some() {
                        actor.admit_kept_elsewhere_in(workspace, now, (&offered.0, &offered.1))
                    } else {
                        actor.admit_published_in(workspace, now, (&offered.0, &offered.1))
                    }
                },
            )??;

        if execution.is_some()
            && !self
                .stable_invocation_subscribers
                .lock()
                .map_err(|_| "application capability unavailable")?
                .contains(&peer_id)
        {
            return Err("application_stable_identity_unavailable".into());
        }
        // A configured but unreadable/corrupt journal disables every new admission.
        let mut journal_guard = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?;
        let journal = journal_guard.as_mut().map_err(|error| error.clone())?;
        if owner.verified_actor().is_some() && journal.is_none() {
            return Err("action_evidence_unavailable: admission journal unavailable".into());
        }
        // What this admission is, decided before its ask is looked at: an
        // invocation the caller already had admitted under this id is answered
        // from its record, never asked again.
        let id = self
            .invocation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "invocation handle space exhausted")?;
        let authoritative = execution
            .as_ref()
            .map(ExecutionScope::invocation)
            .or(match &origin {
                Some(InvocationCaller::NativePerson { invocation, .. }) => Some(*invocation),
                _ => None,
            });
        if requested.is_some_and(|requested| {
            requested.0.is_nil() || authoritative.is_some_and(|held| held != requested)
        }) {
            return Err("requested invocation does not match admitted ownership".into());
        }
        let invocation = requested
            .or(authoritative)
            .unwrap_or_else(|| tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))));
        let mut receipt_context = tilo_actions::ActionContext::for_resource(
            &offered.1.resource,
            &offered.1.resource_kind,
            &offered.1.resource_label,
        )
        .with_app(&offered.0.app_id, &offered.0.app_id)
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(
                &offered.0.app_id,
                &offered.1.resource_kind,
                &offered.1.action,
            ),
            [],
        )));
        if let Some(ambient) = &context {
            receipt_context = receipt_context.enriched_by(ambient);
        }
        let intent = match journal.as_ref() {
            Some(_) => Some(Self::invocation_intent(
                &offered,
                IntentInputs {
                    peer_id,
                    actor,
                    argument,
                    revision,
                    original_request,
                    local_report,
                    typed_local_arguments: typed_local_arguments.as_ref(),
                    receipt_context: &receipt_context,
                },
            )),
            _ => None,
        };
        // Only what this connection's own admission decides has a record to
        // answer from here; a peer's or a run's ownership carries its own, and
        // a report is admitted through its own receipts.
        if local_report.is_none()
            && let (Some(journal), Some(intent)) = (journal.as_ref(), intent.as_ref())
            && let Some(record) = self.retained_invocation(
                journal,
                &offered.0.route.cluster_id,
                &owner,
                invocation,
                actor,
                now,
            )?
        {
            if !record.admitted_intent_is(intent) {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(self.retained_answer(&record));
        }
        let confirmed = self.asks_first(
            peer_id,
            &offered,
            &super::asks::Asking {
                request: original_request,
                caller,
                argument,
                actor,
                token: confirmation_token,
            },
            asks,
        )?;
        // Spent now, not once durable: a grant used once never admits twice, even should this
        // admission fail; and of two calls deciding on it at once, only one is admitted.
        if !self.spend_once_grants(&offered.0.route.cluster_id, &spent) {
            return Err(super::asks::Refusal::Error(
                tilo_actions::remote::ActionCallError::NotPermitted,
            ));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut admitted = tilo_invocation_journal::Durable::settled();
        let journal_key = if let Some(journal) = journal.as_mut() {
            let key = InvocationKey {
                workspace: offered.0.route.cluster_id.clone(),
                host: journal.host(),
                invocation,
                owner: owner.clone(),
            };
            let intent = intent.ok_or("invocation intent unavailable")?;
            if local_report.is_some() {
                self.local_report_receipts
                    .lock()
                    .map_err(|_| "local receipt cache unavailable")?
                    .admit(key.clone(), &intent, id, peer_id)?;
                Some(key)
            } else {
                let admission = if owner.verified_actor().is_some() {
                    let facts = tilo_invocation_journal::evidence_facts(&key, &intent)?;
                    Some(
                        tilo_invocation_journal::action_evidence::mint(facts, &intent).map_err(|why| {
                            format!("action_evidence_unavailable: host signing identity unavailable: {why}")
                        })?,
                    )
                } else {
                    // Actor-only legacy/system work is never upgraded into a certified person.
                    None
                };
                let (fresh, durable) =
                    journal.admit_with_evidence(key.clone(), intent, id, admission)?;
                if !fresh {
                    // A duplicate racing this admission: answer what the
                    // journal now retains instead of admitting it twice.
                    let record = journal.lookup(&key)?.ok_or("admitted invocation missing")?;
                    return Ok(self.retained_answer(&record));
                }
                admitted = durable;
                Some(key)
            }
        } else {
            None
        };
        drop(journal_guard);
        if local_report.is_some() {
            // The app reports speculative local execution (possibly a read/no-op).
            // Retain only admission, never execution, offload, or success evidence.
            return Ok(StagedJob {
                admitted,
                start: Admitted::Reported(InvocationJob { id, completion: rx }),
            });
        }
        let sender = sender.ok_or("application invocation stream unavailable")?;
        let untrusted = offered.1.contract.carries_untrusted_content();
        // Admitted: an offloadable action may now run on a better-placed
        // device, which the turn asks the route about. The device that runs it
        // is sent this invocation, so it answers the invocation the person
        // made rather than a second one of its own.
        let offload = self.offload_request(
            (&offered.0.route.cluster_id, &offered.0.app_id),
            &offered.1,
            tilo_actions::remote::InvokeRequest {
                invocation,
                target: tilo_actions::remote::application_action_target(
                    &offered.0.route.cluster_id,
                    &offered.0.app_id,
                    offered.1.resource.clone(),
                    offered.1.action.clone(),
                ),
                argument: tilo_actions::remote::InvokeArgument::from_slot(argument),
                context: context.clone(),
                // A device that runs it answers for the confirmation the
                // person gave here, so a confirmed action is not run there
                // unconfirmed.
                confirmation: original_request.and_then(|request| request.confirmation.clone()),
                revision: revision.map(str::to_owned),
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        );
        // What it was admitted to reach caps what its effect asks for: its
        // own reach, narrowed by what it continues unless the person
        // confirmed it.
        let own = tilo_action_policy::Reach {
            kind: offered.1.contract.kind,
            privileged: offered.1.contract.need().privileged,
        };
        let reach = match actor.ceiling {
            Some(ceiling) if !confirmed => own.narrowed(ceiling),
            _ => own,
        };
        let pending = PendingInvocation {
            reach,
            caller: continuing,
            activity: ActivityTarget::new(
                execution.clone(),
                (actor, caller, context.as_ref()),
                &offered.0,
                &offered.1,
                argument,
            ),
            owner,
            original: OriginalInvocationData::capture(
                super::super::InvocationRequest {
                    wire_id: id,
                    invocation,
                    address: tilo_actions::ActionAddress {
                        resource: offered.1.resource.clone(),
                        action: offered.1.action.clone(),
                    },
                    argument: argument.map(str::to_string),
                    actor: actor.actor().clone(),
                    presence: invoker::invoker_presence(context.as_ref()),
                },
                offered.0.route.clone(),
                &offered.1,
                origin,
                policy_time.clone(),
                execution.as_ref().map(ExecutionScope::lease),
            ),
            policy_time,
            journal_key,
            peer_id,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            prepared_effect: Arc::default(),
            last_sequence: 0,
            actor: actor.actor().clone(),
            cancellable: offered.1.contract.execution.cancellable(),
            retryable_on_disconnect: matches!(
                offered.1.contract.retry,
                tilo_actions::RetryPolicy::Idempotent
                    | tilo_actions::RetryPolicy::RetryWithInvocationId
            ),
            execution: offered.1.contract.execution.clone(),
            result: offered.1.contract.result.clone(),
            operation_kind: offered.1.operation_kind,
            cluster_id: offered.0.route.cluster_id.clone(),
            context: receipt_context,
            last_activity: tilo_clock::now(),
            cancellation_requested_at: None,
            cancellation_fence: Arc::new(AtomicBool::new(false)),
        };
        let envelope = InvocationEnvelope {
            id,
            invocation,
            resource: offered.1.resource,
            action: offered.1.action,
            argument: argument.map(str::to_string),
            actor: actor.actor().clone(),
            context,
        };
        Ok(StagedJob {
            admitted,
            start: Admitted::Delivered(Box::new(Delivery {
                id,
                completion: rx,
                pending,
                envelope,
                sender,
                offload,
                untrusted,
            })),
        })
    }
}
