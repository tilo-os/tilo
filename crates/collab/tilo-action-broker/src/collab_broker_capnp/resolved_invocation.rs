//! Revision-bound invocation through the original certified channel.
use super::{
    AdmissionProvenance, BrokerActor, CapnpBrokerState, CapnpChannelHandle,
    DIRECTORY_MAX_CONFIRMATION_BYTES, DIRECTORY_MAX_IDENTIFIER_BYTES, DIRECTORY_MAX_PARAMS_BYTES,
    DIRECTORY_MAX_QUERY_REVISION_BYTES, DirectoryActionConfirmation, Duration, HostWorkerReadGuard,
    InvocationCaller, InvocationInputs, LocalActionDirectoryRegistry, PrivateWorkerRegistration,
    Promise, PublishedAction, PublishedSurface, Rc, RegisteredActionDirectory, channel_handle,
    invocation_target_peer, published_directory_action, validate_directory_identifier,
    validate_directory_target_input, validate_directory_text,
};

struct CapturedResolution {
    registered: RegisteredActionDirectory,
    worker: Option<PrivateWorkerRegistration>,
    resolution: runtime_collab::ActionDirectoryResolution,
    permit: tokio::sync::OwnedSemaphorePermit,
}

pub(super) struct ResolvedOffer {
    pub(super) registered: RegisteredActionDirectory,
    published: PublishedAction,
    worker: Option<PrivateWorkerRegistration>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn check_registration(
    registered: &RegisteredActionDirectory,
    worker: Option<&PrivateWorkerRegistration>,
    state: &CapnpBrokerState,
    directories: &LocalActionDirectoryRegistry,
    peer: u64,
) -> Result<(), String> {
    let current = directories
        .registered(peer, &registered.route.cluster_id)
        .map_err(|error| error.to_string())?;
    if !Rc::ptr_eq(&current.registration, &registered.registration)
        || current.app_id != registered.app_id
        || current.route != registered.route
    {
        return Err("application directory registration changed during resolution".into());
    }
    let current_worker = state
        .host_workers
        .lock()
        .map_err(|_| "worker registry unavailable")?
        .get(&peer)
        .and_then(|current| current.worker_registration.clone());
    if current_worker.as_ref() != worker {
        return Err("application worker connection changed during resolution".into());
    }
    Ok(())
}
impl ResolvedOffer {
    pub(super) fn check_registration(
        &self,
        state: &CapnpBrokerState,
        directories: &LocalActionDirectoryRegistry,
        peer: u64,
    ) -> Result<(), String> {
        check_registration(
            &self.registered,
            self.worker.as_ref(),
            state,
            directories,
            peer,
        )
    }
    pub(super) fn offered(&self, peer_id: u64) -> (PublishedSurface, PublishedAction) {
        (
            PublishedSurface {
                peer_id,
                app_id: self.registered.app_id.clone(),
                route: self.registered.route.clone(),
                actions: vec![self.published.clone()],
                live: true,
            },
            self.published.clone(),
        )
    }
}
fn binding_worker<'a>(
    registered: &RegisteredActionDirectory,
    worker: Option<&'a PrivateWorkerRegistration>,
    state: &CapnpBrokerState,
    peer: u64,
) -> Result<&'a PrivateWorkerRegistration, String> {
    let worker = worker.ok_or("registered original worker unavailable")?;
    let current = state
        .host_workers
        .lock()
        .map_err(|_| "worker registry unavailable")?
        .get(&peer)
        .cloned()
        .ok_or("worker connection retired")?;
    if current.worker_registration.as_ref() != Some(worker) || worker.route != registered.route {
        return Err("resolved worker registration changed".into());
    }
    state
        .revalidate_admission(&worker.route, &current)
        .map_err(|error| error.to_string())?;
    if !state
        .stable_invocation_subscribers
        .lock()
        .map_err(|_| "application capability unavailable")?
        .contains(&peer)
    {
        return Err("application stable invocation stream unavailable".into());
    }
    if state
        .invocation_subscribers
        .lock()
        .map_err(|_| "application invocation stream unavailable")?
        .get(&peer)
        .is_none_or(tokio::sync::mpsc::UnboundedSender::is_closed)
    {
        return Err("application invocation stream closed".into());
    }
    Ok(worker)
}
fn binding_data(
    registered: &RegisteredActionDirectory,
    worker: &PrivateWorkerRegistration,
    peer: u64,
    host: uuid::Uuid,
) -> Result<runtime_collab::PublishedInvocationBinding, String> {
    let encoded = serde_json::to_vec(&(
        "tilo.resolved-invocation-target.v1",
        worker.identity,
        *registered.registration,
        &worker.route.app_id,
        &worker.route.cluster_id,
        &worker.route.record_id,
        worker.certificate.user_public_key,
        worker.certificate.device_public_key,
    ))
    .map_err(|error| error.to_string())?;
    Ok(runtime_collab::PublishedInvocationBinding {
        host: *host.as_bytes(),
        peer_id: peer,
        app_id: registered.app_id.clone(),
        record_id: worker.route.record_id.clone(),
        root: worker.certificate.user_public_key,
        device: worker.certificate.device_public_key,
        target_binding: *blake3::hash(&encoded).as_bytes(),
    })
}
impl CapturedResolution {
    fn binding(
        &self,
        state: &CapnpBrokerState,
        peer: u64,
    ) -> Result<runtime_collab::PublishedInvocationBinding, String> {
        let worker = binding_worker(&self.registered, self.worker.as_ref(), state, peer)?;
        binding_data(&self.registered, worker, peer, state.invocation_host()?)
    }
}
impl ResolvedOffer {
    pub(super) fn binding_for_host(
        &self,
        state: &CapnpBrokerState,
        peer: u64,
        host: uuid::Uuid,
    ) -> Result<runtime_collab::PublishedInvocationBinding, String> {
        let worker = binding_worker(&self.registered, self.worker.as_ref(), state, peer)?;
        binding_data(&self.registered, worker, peer, host)
    }
}
impl CapnpBrokerState {
    pub(super) fn current_invocation_binding(
        &self,
        directories: &LocalActionDirectoryRegistry,
        peer: u64,
        cluster: &str,
        host: uuid::Uuid,
    ) -> Result<runtime_collab::PublishedInvocationBinding, String> {
        let registered = directories
            .registered(peer, cluster)
            .map_err(|error| error.to_string())?;
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(&peer)
            .and_then(|current| current.worker_registration.clone());
        let current = binding_worker(&registered, worker.as_ref(), self, peer)?;
        binding_data(&registered, current, peer, host)
    }
    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    async fn capture_directory_resolution(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster: &str,
        peer: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<CapturedResolution, String> {
        let permit = directories
            .resolved_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| "resolved invocation capacity exhausted")?;
        let registered = directories
            .registered(peer, cluster)
            .map_err(|error| error.to_string())?;
        let worker = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(&peer)
            .and_then(|current| current.worker_registration.clone());
        let resolution = tokio::time::timeout(
            Duration::from_secs(2),
            self.resolve_action_directory_for_actor(
                directories,
                actor,
                cluster,
                peer,
                revision,
                resource,
                action,
            ),
        )
        .await
        .map_err(|_| "action directory resolution timeout")??;
        check_registration(&registered, worker.as_ref(), self, directories, peer)?;
        Ok(CapturedResolution {
            registered,
            worker,
            resolution,
            permit,
        })
    }
    #[expect(
        clippy::future_not_send,
        reason = "runs on the broker's single-threaded LocalSet"
    )]
    pub(super) async fn resolve_invocation_offer(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster: &str,
        peer: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<ResolvedOffer, String> {
        validate_directory_identifier(
            revision,
            "invocation revision",
            DIRECTORY_MAX_QUERY_REVISION_BYTES,
            true,
        )
        .map_err(|error| error.to_string())?;
        let captured = self
            .capture_directory_resolution(
                directories,
                actor,
                cluster,
                peer,
                revision,
                resource,
                action,
            )
            .await?;
        let runtime_collab::ActionDirectoryResolution::Found { row, .. } = captured.resolution
        else {
            return Err("directory revision or action changed".into());
        };
        let published = published_directory_action(&row).map_err(|error| error.to_string())?;
        Ok(ResolvedOffer {
            registered: captured.registered,
            published,
            worker: captured.worker,
            _permit: captured.permit,
        })
    }
}

/// Read-only discovery preserves ordinary resolution variants. A reliable
/// binding is emitted only from this same bounded captured resolution.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered parse, bounded capture, admission recheck and reply of a resolution"
)]
pub(super) fn resolve(
    channel: Rc<CapnpChannelHandle>,
    params: &channel_handle::ResolvePublishedActionParams,
    mut results: channel_handle::ResolvePublishedActionResults,
) -> Promise<(), capnp::Error> {
    let parsed = (|| {
        let request = params.get()?;
        let peer = request.get_peer_id();
        let revision = bounded_text(
            request.get_revision()?,
            "directory revision",
            DIRECTORY_MAX_QUERY_REVISION_BYTES,
            true,
            false,
        )?;
        let resource = bounded_text(
            request.get_resource()?,
            "directory resource",
            DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
            true,
        )?;
        let action = bounded_text(
            request.get_action()?,
            "directory action",
            DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
            true,
        )?;
        let route = channel.route();
        let time = channel
            .state
            .registered_policy_time()
            .map_err(capnp::Error::failed)?;
        channel
            .state
            .checked_policy_time(time.as_ref())
            .map_err(capnp::Error::failed)?;
        let admitted = channel
            .state
            .revalidate_admission(&route, &channel.verified_identity)?;
        validate_directory_target_input(
            &route.cluster_id,
            &revision,
            &resource,
            &action,
            None,
            None,
        )?;
        Ok((peer, revision, resource, action, route, time, admitted))
    })();
    let (peer, revision, resource, action, route, time, admitted) = match parsed {
        Ok(value) => value,
        Err(error) => return Promise::err(error),
    };
    let host_guard = HostWorkerReadGuard::for_channel(&channel.state, channel.peer_id, &admitted);
    Promise::from_future(async move {
        HostWorkerReadGuard::check(host_guard.as_ref())?;
        let state = &channel.state;
        let captured = state
            .capture_directory_resolution(
                &channel.directories,
                &admitted.broker_actor(),
                &route.cluster_id,
                peer,
                &revision,
                &resource,
                &action,
            )
            .await
            .map_err(capnp::Error::failed)?;
        HostWorkerReadGuard::check(host_guard.as_ref())?;
        let admission = state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        if !state.is_accepting() {
            return Err(capnp::Error::failed("invocation broker retired".into()));
        }
        state
            .checked_policy_time(time.as_ref())
            .map_err(capnp::Error::failed)?;
        let current = state.revalidate_admission(&route, &admitted)?;
        check_registration(
            &captured.registered,
            captured.worker.as_ref(),
            state,
            &channel.directories,
            peer,
        )
        .map_err(capnp::Error::failed)?;
        let binding = if time.is_some()
            && current.certificate.is_some()
            && !revision.is_empty()
            && matches!(
                &captured.resolution,
                runtime_collab::ActionDirectoryResolution::Found { .. }
            ) {
            // Missing journal/stream/worker custody permits browsing only.
            captured.binding(state, peer).ok()
        } else {
            None
        };
        let mut out = results.get();
        runtime_collab::write_action_directory_resolution(
            out.reborrow().init_resolution(),
            &captured.resolution,
        )?;
        if let Some(binding) = binding {
            let mut target = out.init_invocation_binding();
            target.set_host(&binding.host);
            target.set_peer_id(binding.peer_id);
            target.set_app_id(&binding.app_id);
            target.set_record_id(&binding.record_id);
            target.set_root(&binding.root);
            target.set_device(&binding.device);
            target.set_target_binding(&binding.target_binding);
        }
        drop(admission);
        drop(captured);
        Ok(())
    })
}

pub(super) fn invoke(
    channel: Rc<CapnpChannelHandle>,
    params: &channel_handle::InvokeResolvedPublishedActionParams,
    results: channel_handle::InvokeResolvedPublishedActionResults,
) -> Promise<(), capnp::Error> {
    invoke_checked(channel, params, results)
}

fn bounded_text(
    value: capnp::text::Reader<'_>,
    field: &str,
    max: usize,
    identifier: bool,
    required: bool,
) -> Result<String, capnp::Error> {
    let value = value
        .to_str()
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
    if identifier {
        validate_directory_identifier(value, field, max, required)?;
    } else {
        validate_directory_text(value, field, max, required)?;
    }
    Ok(value.to_owned())
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered parse, offer resolution, admission recheck and dispatch of an invocation"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the admission guard and resolved offer permit are held through dispatch on purpose"
)]
fn invoke_checked(
    channel: Rc<CapnpChannelHandle>,
    params: &channel_handle::InvokeResolvedPublishedActionParams,
    mut results: channel_handle::InvokeResolvedPublishedActionResults,
) -> Promise<(), capnp::Error> {
    let parsed = (|| {
        let request = params.get()?;
        let peer = invocation_target_peer(request.get_peer_id(), channel.peer_id);
        let revision = bounded_text(
            request.get_revision()?,
            "invocation revision",
            DIRECTORY_MAX_QUERY_REVISION_BYTES,
            true,
            true,
        )?;
        let resource = bounded_text(
            request.get_resource()?,
            "invocation resource",
            DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
            true,
        )?;
        let action = bounded_text(
            request.get_action()?,
            "invocation action",
            DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
            true,
        )?;
        // Bound even an ignored field before any caller-owned text is copied.
        let argument = bounded_text(
            request.get_argument()?,
            "invocation argument",
            DIRECTORY_MAX_PARAMS_BYTES,
            false,
            false,
        )?;
        let argument = request.get_has_argument().then_some(argument);
        let confirmation = bounded_text(
            request.get_confirmation_json()?,
            "invocation confirmation",
            DIRECTORY_MAX_CONFIRMATION_BYTES,
            false,
            false,
        )?;
        let confirmation = if confirmation.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str::<serde_json::Value>(&confirmation).map_err(|error| {
                capnp::Error::failed(format!("invalid confirmation evidence: {error}"))
            })?
        };
        let confirmation = if let Some(phrase) = confirmation
            .get("confirmation_phrase")
            .and_then(serde_json::Value::as_str)
        {
            Some(DirectoryActionConfirmation::Phrase(phrase.into()))
        } else if confirmation
            .get("confirm")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            Some(DirectoryActionConfirmation::Confirmed)
        } else {
            None
        };
        let time = channel
            .state
            .registered_policy_time()
            .map_err(capnp::Error::failed)?
            .ok_or_else(|| {
                capnp::Error::failed("resolved invocation requires original policy time".into())
            })?;
        channel
            .state
            .checked_policy_time(Some(&time))
            .map_err(capnp::Error::failed)?;
        let route = channel.route();
        let admitted = channel
            .state
            .revalidate_admission(&route, &channel.verified_identity)?;
        if admitted.certificate.is_none() {
            return Err(capnp::Error::failed(
                "resolved invocation requires original certificate".into(),
            ));
        }
        Ok((
            peer,
            revision,
            resource,
            action,
            argument,
            confirmation,
            time,
            route,
            admitted,
        ))
    })();
    let (peer, revision, resource, action, argument, confirmation, time, route, admitted) =
        match parsed {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
    Promise::from_future(async move {
        let state = &channel.state;
        let offer = state
            .resolve_invocation_offer(
                &channel.directories,
                &admitted.broker_actor(),
                &route.cluster_id,
                peer,
                &revision,
                &resource,
                &action,
            )
            .await
            .map_err(capnp::Error::failed)?;
        // No admission mutex or owner guard is held over directory RPC. The
        // original caller/time and exact capability registration are checked
        // again under the same serialization used by every owned dispatch.
        let admission = state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        if !state.is_accepting() {
            return Err(capnp::Error::failed("invocation broker retired".into()));
        }
        state
            .checked_policy_time(Some(&time))
            .map_err(capnp::Error::failed)?;
        let current = state.revalidate_admission(&route, &admitted)?;
        offer
            .check_registration(state, &channel.directories, peer)
            .map_err(capnp::Error::failed)?;
        let target = state
            .host_workers
            .lock()
            .map_err(|_| capnp::Error::failed("worker registry unavailable".into()))?
            .get(&peer)
            .cloned()
            .ok_or_else(|| capnp::Error::failed("resolved worker connection retired".into()))?;
        if offer.worker.is_none() {
            return Err(capnp::Error::failed(
                "resolved worker original authorizer unavailable".into(),
            ));
        }
        state.revalidate_admission(&offer.registered.route, &target)?;
        let actor = current.broker_actor();
        let offered = offer.offered(peer);
        let token = state
            .directory_confirmation_token(
                peer,
                &resource,
                &action,
                argument.as_deref(),
                &actor,
                &offered.1.contract,
                confirmation,
            )
            .map_err(|error| {
                capnp::Error::failed(format!("directory confirmation refused: {error:?}"))
            })?;
        let job = state
            .dispatch_invocation_from_offered_owned(
                peer,
                offered,
                InvocationInputs {
                    original_request: None,
                    requested: None,
                    caller: None,
                    execution: None,
                    argument: argument.as_deref(),
                    actor: &actor,
                    confirmation_token: token.as_deref(),
                    context: None,
                },
                AdmissionProvenance {
                    durable: None,
                    revision: Some(&revision),
                    origin: Some(InvocationCaller::Local {
                        route,
                        admitted: Box::new(current),
                    }),
                    retained_time: Some(&time),
                },
            )
            .map_err(capnp::Error::failed)?;
        let update = state.invocation_status(job.id).ok_or_else(|| {
            capnp::Error::failed("accepted invocation has no broker state".into())
        })?;
        let update = serde_json::to_string(&update)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        results.get().set_id(job.id);
        results.get().set_update_json(&update);
        drop(job.completion);
        drop(admission);
        drop(offer);
        Ok(())
    })
}
