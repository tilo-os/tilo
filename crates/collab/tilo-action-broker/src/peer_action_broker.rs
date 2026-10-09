//! `LocalSet` half of the certified peer facade. Kept beside broker admission so
//! no application capability or actor constructor crosses the network queue.
use super::{
    Arc, CallerInvocation, CapnpBrokerState, CrdtRoute, DirectoryActionConfirmation,
    InvocationInputs, LocalActionDirectoryRegistry, Rc, VerifiedActionCaller, VerifiedJoinIdentity,
    admit_verified_join_at,
};
use tilo_delivery_protocol::peer_actions::{ActionMessage, Confirmation};

impl CapnpBrokerState {
    pub(crate) fn peer_action_queue(&self) -> crate::peer_actions::PeerActionQueue {
        self.peer_action_queue.clone()
    }

    pub(super) fn verify_network_action_caller(
        &self,
        evidence: &tilo_relay_session::client::CertifiedAction,
    ) -> Result<VerifiedActionCaller, String> {
        let registered = self.registered_policy_time()?;
        self.verify_network_action_caller_with_policy_time(evidence, registered.as_ref())
    }
    pub(super) fn verify_network_action_caller_with_policy_time(
        &self,
        evidence: &tilo_relay_session::client::CertifiedAction,
        submitted: Option<&crate::peer_actions::PolicyTime>,
    ) -> Result<VerifiedActionCaller, String> {
        let cert = evidence.certificate();
        let room = evidence.action().room.to_string();
        let (policy_time, now) = self.checked_policy_time(submitted)?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|_| "certificate rejected")?;
        if cert.capabilities.can_act_as_host || evidence.action().source != cert.device_public_key {
            return Err("device authority required".into());
        }
        if let Some(authority) = self.certificate_authority.get() {
            match &policy_time {
                Some(time) => {
                    authority.authorize_with_policy_time(evidence.action().room, cert, time)?;
                }
                None => authority.authorize(evidence.action().room, cert)?,
            }
        } else if let Some(devices) = &self.trusted_devices {
            if !devices.contains(&cert.device_public_key) {
                return Err("device not enrolled".into());
            }
        } else {
            return Err("current certificate authority unavailable".into());
        }
        let verified = VerifiedJoinIdentity::of(cert, self.capped_in(&room, cert));
        let admitted = self
            .membership
            .as_ref()
            .ok_or("membership unavailable")?
            .decide_in_room(&room, |workspace| {
                admit_verified_join_at(
                    workspace,
                    &CrdtRoute::cluster(&room),
                    verified,
                    now / 1_000_000_000,
                )
            })??;

        Ok(VerifiedActionCaller {
            room,
            presentation: Vec::new(),
            network: Some(evidence.clone()),
            policy_time,
            admitted,
        })
    }

    pub(super) fn start_peer_action_bridge(
        self: &Arc<Self>,
        directories: Rc<LocalActionDirectoryRegistry>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        /// Hands the queue back when the bridge task is dropped with its
        /// broker thread's runtime, so a restarted thread bridges again.
        struct Lent {
            state: Arc<CapnpBrokerState>,
            receiver: Option<tokio::sync::mpsc::Receiver<crate::peer_actions::Request>>,
        }
        impl Drop for Lent {
            fn drop(&mut self) {
                if let Ok(mut slot) = self.state.peer_action_rx.lock() {
                    *slot = self.receiver.take();
                }
            }
        }
        let receiver = self
            .peer_action_rx
            .lock()
            .ok()
            .and_then(|mut rx| rx.take())?;
        let mut lent = Lent {
            state: self.clone(),
            receiver: Some(receiver),
        };
        let state = self.clone();
        Some(tokio::task::spawn_local(async move {
            while let Some(receiver) = lent.receiver.as_mut() {
                let Some(request) = receiver.recv().await else {
                    break;
                };
                let state = state.clone();
                let directories = directories.clone();
                tokio::task::spawn_local(async move {
                    if request.reply.is_closed()
                        && request.evidence.as_ref().is_some_and(|e| {
                            matches!(&e.action().message, ActionMessage::CatalogueRefresh { .. })
                        })
                    {
                        drop(request);
                        return;
                    }
                    let policy_time = request.policy_time().cloned();
                    let response = match state.invocation_host() {
                        Err(_) => Err(()),
                        Ok(host) => match request.evidence.as_ref() {
                            None => Ok(ActionMessage::BrokerCatalogue {
                                host,
                                catalogue: serde_json::Value::Null,
                            }),
                            Some(evidence) => Ok(state
                                .peer_action(
                                    &directories,
                                    evidence,
                                    request.poll,
                                    request.notification,
                                    policy_time.as_ref(),
                                )
                                .await
                                .unwrap_or_else(|_| ActionMessage::Rejected {
                                    code: "action_unavailable_or_not_authorised".into(),
                                })),
                        },
                    };
                    // Dropped reply is uncertainty, never rollback/re-execution.
                    let _ = request.reply.send(response);
                    // Explicit use captures the guard in this future despite
                    // precise field capture. Cancellation also drops it here.
                    #[expect(
                        clippy::used_underscore_binding,
                        reason = "the permit is named only to release it at this point"
                    )]
                    drop(request._permit);
                });
            }
        }))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one certified-request dispatcher over the relay action vocabulary"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the resolved offer keeps its capacity permit until dispatch; the lint misreads its later borrows"
    )]
    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    async fn peer_action(
        &self,
        directories: &LocalActionDirectoryRegistry,
        evidence: &tilo_relay_session::client::CertifiedAction,
        poll: Option<tilo_actions::InvocationId>,
        notification: Option<tilo_actions::InvocationUpdate>,
        policy_time: Option<&crate::peer_actions::PolicyTime>,
    ) -> Result<ActionMessage, String> {
        let caller = self.verify_network_action_caller_with_policy_time(evidence, policy_time)?;
        let host = self.invocation_host()?;
        if let Some(invocation) = poll {
            let mut observation = self.peer_observation(&caller, invocation).await?;
            if let Some(update) = notification {
                if update.invocation() != invocation {
                    return Err("notification identity mismatch".into());
                }
                if let ActionMessage::Observation {
                    update: retained @ Some(_),
                    uncertain: false,
                    ..
                } = &mut observation
                {
                    *retained = Some(update);
                }
            }
            return Ok(observation);
        }
        match &evidence.action().message {
            ActionMessage::RecordEffectReceipt {
                invocation,
                redacted,
            } => {
                let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
                    room: caller.room.clone(),
                    operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                        invocation: invocation.0,
                        redacted: redacted.as_bytes().to_vec(),
                    },
                };
                let _order = self.owner_commit_order.lock().await;
                self.publisher
                    .commit_member_effect_receipt(request, evidence.certificate().clone())
                    .await?;
                Ok(ActionMessage::EffectReceiptCommitted {
                    invocation: *invocation,
                })
            }
            ActionMessage::CatalogueRefresh { scope } => {
                let catalogue = serde_json::from_str(&self.action_catalogue_json_for(
                    &caller.room,
                    &caller.admitted.broker_actor(),
                    scope.as_ref(),
                )?)
                .map_err(|_| "catalogue unavailable")?;
                Ok(ActionMessage::BrokerCatalogue { host, catalogue })
            }
            ActionMessage::Status { invocation } => {
                self.peer_observation(&caller, *invocation).await
            }
            ActionMessage::Cancel { invocation } => {
                self.cancel_verified_invocation(&caller, *invocation)?;
                Ok(ActionMessage::CancellationRequested {
                    invocation: *invocation,
                })
            }
            ActionMessage::InvokeBound {
                host: requested_host,
                invocation,
                app_peer,
                app_id,
                record_id,
                resource,
                action,
                argument,
                confirmation,
                revision,
            } => {
                if *requested_host != host {
                    return Err("journal host changed".into());
                }
                let request = CallerInvocation {
                    recipient: Some(evidence.action().target),
                    invocation: *invocation,
                    #[cfg(test)]
                    host,
                    peer_id: app_peer.parse().map_err(|_| "invalid app peer")?,
                    app_id: app_id.clone(),
                    record_id: record_id.clone(),
                    resource: resource.clone(),
                    action: action.clone(),
                    argument: argument
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|_| "invalid argument")?,
                    revision: revision.clone(),
                };
                if request.peer_id == ui::collaboration::CURRENT_CONNECTION_PEER_ID {
                    return Err("local sentinel forbidden".into());
                }
                if let Some(record) = self.verified_invocation_status(&caller, *invocation)? {
                    if !record.admitted_request_is(&request.intent()) {
                        return Err("changed invocation intent".into());
                    }
                    return self.peer_observation(&caller, *invocation).await;
                }
                let actor = caller.admitted.broker_actor();
                let resolved_offer = if let Some(revision) = revision {
                    let offer = self
                        .resolve_invocation_offer(
                            directories,
                            &actor,
                            &caller.room,
                            request.peer_id,
                            revision,
                            resource,
                            action,
                        )
                        .await?;
                    if offer.registered.app_id != *app_id
                        || offer.registered.route.record_id != *record_id
                    {
                        return Err("directory route changed".into());
                    }
                    Some(offer)
                } else {
                    None
                };
                let offered = if let Some(offer) = &resolved_offer {
                    offer.offered(request.peer_id)
                } else {
                    let surface = self
                        .surfaces
                        .lock()
                        .map_err(|_| "catalogue unavailable")?
                        .get(&request.peer_id)
                        .cloned()
                        .filter(|s| {
                            s.live
                                && s.app_id == *app_id
                                && s.route.cluster_id == caller.room
                                && s.route.record_id == *record_id
                        })
                        .ok_or("exact action route unavailable")?;
                    let published = surface
                        .actions
                        .iter()
                        .find(|a| a.resource == *resource && a.action == *action)
                        .cloned()
                        .ok_or("action no longer offered")?;
                    (surface, published)
                };
                // The directory await is outside admission serialization. Recheck
                // current certificate, room permission and duplicate before a grant.
                let admission = self
                    .admission_lock
                    .lock()
                    .map_err(|_| "admission unavailable")?;
                let actor = self.current_action_caller(&caller)?.broker_actor();
                if let Some(offer) = &resolved_offer {
                    offer.check_registration(self, directories, request.peer_id)?;
                }
                if let Some(record) = self.verified_invocation_status(&caller, *invocation)? {
                    if !record.admitted_request_is(&request.intent()) {
                        return Err("changed invocation intent".into());
                    }
                    drop(admission);
                    return self.peer_observation(&caller, *invocation).await;
                }
                let confirmation = confirmation.as_ref().map(|c| match c {
                    Confirmation::Confirm => DirectoryActionConfirmation::Confirmed,
                    Confirmation::Phrase(p) => DirectoryActionConfirmation::Phrase(p.clone()),
                });
                let token = self
                    .directory_confirmation_token(
                        request.peer_id,
                        resource,
                        action,
                        request.argument.as_deref(),
                        &actor,
                        &offered.1.contract,
                        confirmation,
                    )
                    .map_err(|_| "confirmation required")?;
                self.dispatch_invocation_from_offered(
                    request.peer_id,
                    offered,
                    InvocationInputs {
                        requested: None,
                        caller: None,
                        execution: None,
                        argument: request.argument.as_deref(),
                        actor: &actor,
                        confirmation_token: token.as_deref(),
                        context: None,
                    },
                    Some((&caller, &request)),
                    revision.as_deref(),
                )?;
                drop(admission);
                self.peer_observation(&caller, *invocation).await
            }
            _ => Err("unsupported action request".into()),
        }
    }
    async fn peer_observation(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<ActionMessage, String> {
        let mut record = self.verified_invocation_status(caller, invocation)?;
        if let Some(retained) = record {
            record = Some(
                self.reconcile_prepared_effect(retained, || {
                    self.verified_invocation_status(caller, invocation)?;
                    Ok(())
                })
                .await?,
            );
        }
        Ok(ActionMessage::Observation {
            host: self.invocation_host()?,
            invocation,
            update: record.as_ref().map(|r| r.update.clone()),
            uncertain: record.as_ref().is_some_and(|r| r.uncertain),
            cancellation_requested: record.as_ref().is_some_and(|r| r.cancellation_requested),
        })
    }
}
