//! LocalSet half of the certified peer facade. Kept beside broker admission so
//! no application capability or actor constructor crosses the network queue.
use super::*;
use qos_relay::peer_actions::{ActionMessage, Confirmation};

impl CapnpBrokerState {
    pub(crate) fn peer_action_queue(&self) -> crate::shell::peer_actions::PeerActionQueue {
        self.peer_action_queue.clone()
    }

    pub(super) fn verify_network_action_caller(
        &self,
        evidence: &qos_relay::client::CertifiedAction,
    ) -> Result<VerifiedActionCaller, String> {
        let cert = evidence.certificate();
        let room = evidence.action().room.to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        qos_user_identity::verify_device_cert_at(cert, now).map_err(|_| "certificate rejected")?;
        if cert.capabilities.can_act_as_host || evidence.action().source != cert.device_public_key {
            return Err("device authority required".into());
        }
        if let Some(authority) = self.certificate_authority.get() {
            authority.authorize(evidence.action().room, cert)?;
        } else if let Some(devices) = &self.trusted_devices {
            if !devices.contains(&cert.device_public_key) {
                return Err("device not enrolled".into());
            }
        } else {
            let profile = crate::identity_registry::selected_identity_profile(&room)
                .map_err(|_| "identity lookup failed")?
                .ok_or("workspace identity unavailable")?;
            let fleet = crate::device_fleet::DeviceFleetStore::from_env()
                .and_then(|store| store.load(&profile.id))
                .map_err(|_| "device authority unavailable")?;
            if cert.user_public_key != profile.user_public_key
                || fleet.is_revoked(&cert.device_public_key)
            {
                return Err("device revoked".into());
            }
            // The relay proves device-key possession. A currently certified
            // device under an admitted root need not belong to a fixed pool.
        }
        let tree = self
            .membership
            .as_ref()
            .ok_or("membership unavailable")?
            .read();
        let admitted = admit_verified_join(
            &tree,
            &CrdtRoute::cluster(&room),
            VerifiedJoinIdentity {
                user_public_key: cert.user_public_key,
                device_public_key: cert.device_public_key,
            },
        )?;
        Ok(VerifiedActionCaller {
            room,
            presentation: Vec::new(),
            network: Some(evidence.clone()),
            admitted,
        })
    }

    pub(super) fn start_peer_action_bridge(
        self: &Arc<Self>,
        directories: Rc<LocalActionDirectoryRegistry>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let Some(mut receiver) = self.peer_action_rx.lock().ok().and_then(|mut rx| rx.take())
        else {
            return None;
        };
        let state = self.clone();
        Some(tokio::task::spawn_local(async move {
            while let Some(request) = receiver.recv().await {
                let state = state.clone();
                let directories = directories.clone();
                tokio::task::spawn_local(async move {
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
                                )
                                .await
                                .unwrap_or_else(|_| ActionMessage::Rejected {
                                    code: "action_unavailable_or_not_authorised".into(),
                                })),
                        },
                    };
                    // Dropped reply is uncertainty, never rollback/re-execution.
                    let _ = request.reply.send(response);
                });
            }
        }))
    }

    async fn peer_action(
        &self,
        directories: &LocalActionDirectoryRegistry,
        evidence: &qos_relay::client::CertifiedAction,
        poll: Option<qos_actions::InvocationId>,
        notification: Option<qos_actions::InvocationUpdate>,
    ) -> Result<ActionMessage, String> {
        let caller = self.verify_network_action_caller(evidence)?;
        let host = self.invocation_host()?;
        if let Some(invocation) = poll {
            let mut observation = self.peer_observation(&caller, invocation)?;
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
            ActionMessage::CatalogueRefresh => {
                let catalogue = serde_json::from_str(
                    &self
                        .action_catalogue_json_for(&caller.room, &caller.admitted.broker_actor())?,
                )
                .map_err(|_| "catalogue unavailable")?;
                Ok(ActionMessage::BrokerCatalogue { host, catalogue })
            }
            ActionMessage::Status { invocation } => self.peer_observation(&caller, *invocation),
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
                    if record.intent["request"] != request.intent() {
                        return Err("changed invocation intent".into());
                    }
                    return self.peer_observation(&caller, *invocation);
                }
                let actor = caller.admitted.broker_actor();
                let offered = if let Some(revision) = revision {
                    let registered = directories
                        .registered(request.peer_id, &caller.room)
                        .map_err(|_| "directory unavailable")?;
                    if registered.app_id != *app_id || registered.route.record_id != *record_id {
                        return Err("directory route changed".into());
                    }
                    let resolved = tokio::time::timeout(
                        Duration::from_secs(2),
                        self.resolve_action_directory_for_actor(
                            directories,
                            &actor,
                            &caller.room,
                            request.peer_id,
                            revision,
                            resource,
                            action,
                        ),
                    )
                    .await
                    .map_err(|_| "directory timeout")??;
                    let runtime_collab::ActionDirectoryResolution::Found(row) = resolved else {
                        return Err("directory revision or action changed".into());
                    };
                    let published =
                        published_directory_action(&row).map_err(|_| "invalid action")?;
                    (
                        PublishedSurface {
                            peer_id: request.peer_id,
                            app_id: registered.app_id,
                            route: registered.route,
                            actions: vec![published.clone()],
                            live: true,
                        },
                        published,
                    )
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
                let _admission = self
                    .admission_lock
                    .lock()
                    .map_err(|_| "admission unavailable")?;
                let actor = self.current_action_caller(&caller)?.broker_actor();
                if let Some(record) = self.verified_invocation_status(&caller, *invocation)? {
                    if record.intent["request"] != request.intent() {
                        return Err("changed invocation intent".into());
                    }
                    return self.peer_observation(&caller, *invocation);
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
                    request.argument.as_deref(),
                    &actor,
                    token.as_deref(),
                    None,
                    Some((&caller, &request)),
                    revision.as_deref(),
                )?;
                self.peer_observation(&caller, *invocation)
            }
            _ => Err("unsupported action request".into()),
        }
    }
    fn peer_observation(
        &self,
        caller: &VerifiedActionCaller,
        invocation: qos_actions::InvocationId,
    ) -> Result<ActionMessage, String> {
        let record = self.verified_invocation_status(caller, invocation)?;
        Ok(ActionMessage::Observation {
            host: self.invocation_host()?,
            invocation,
            update: record.as_ref().map(|r| r.update.clone()),
            uncertain: record.as_ref().is_some_and(|r| r.uncertain),
            cancellation_requested: record.as_ref().is_some_and(|r| r.cancellation_requested),
        })
    }
}
