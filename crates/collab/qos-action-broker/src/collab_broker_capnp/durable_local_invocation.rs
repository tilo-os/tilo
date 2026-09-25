//! Durable local RPCs use the existing journal and invocation dispatcher.
use super::*;
use resolved_invocation::ResolvedOffer;

#[derive(Clone)]
pub(super) struct LocalCaller {
    peer: u64,
    route: CrdtRoute,
    admitted: AdmittedJoinIdentity,
    time: crate::peer_actions::PolicyTime,
}
impl LocalCaller {
    fn capture(channel: &CapnpChannelHandle) -> Result<Self, String> {
        let time = channel
            .state
            .registered_policy_time()?
            .ok_or("durable local caller needs original policy time")?;
        let caller = Self {
            peer: channel.peer_id,
            route: channel.route(),
            admitted: channel.verified_identity.clone(),
            time,
        };
        caller.current(&channel.state)?;
        Ok(caller)
    }
    fn owner_of(admitted: &AdmittedJoinIdentity) -> Result<InvocationOwner, String> {
        let cert = admitted
            .certificate
            .as_ref()
            .ok_or("durable local caller certificate missing")?;
        if admitted.enrolled {
            return Err("enrolled caller requires its task-owned surface".into());
        }
        let owner = InvocationOwner::AuthenticatedLocal {
            root: cert.user_public_key,
            device: cert.device_public_key,
            agent: admitted.agent_id.clone(),
        };
        owner.validate()?;
        Ok(owner)
    }
    fn owner(&self) -> InvocationOwner {
        Self::owner_of(&self.admitted).expect("validated original local owner")
    }
    fn current(&self, state: &CapnpBrokerState) -> Result<AdmittedJoinIdentity, String> {
        if !state.is_accepting() {
            return Err("invocation broker retired".into());
        }
        state.checked_policy_time(Some(&self.time))?;
        if let Some(registration) = &self.admitted.worker_registration {
            let workers = state
                .host_workers
                .lock()
                .map_err(|_| "worker registry unavailable")?;
            if workers
                .get(&self.peer)
                .and_then(|worker| worker.worker_registration.as_ref())
                != Some(registration)
            {
                return Err("original local caller connection retired or replaced".into());
            }
        }
        let current = state
            .revalidate_admission(&self.route, &self.admitted)
            .map_err(|error| error.to_string())?;
        if Self::owner_of(&current)? != Self::owner_of(&self.admitted)? {
            return Err("original local caller changed".into());
        }
        Ok(current)
    }
    fn origin(&self) -> InvocationCaller {
        InvocationCaller::Local {
            route: self.route.clone(),
            admitted: self.admitted.clone(),
        }
    }
}

pub(super) struct Request {
    invocation: tilo_actions::InvocationId,
    binding: runtime_collab::PublishedInvocationBinding,
    revision: String,
    resource: String,
    action: String,
    argument: Option<String>,
    confirmation: Option<DirectoryActionConfirmation>,
}
impl Request {
    fn intent(&self) -> serde_json::Value {
        let b = &self.binding;
        serde_json::json!({"binding":{"host":b.host,"peer_id":b.peer_id,"app_id":b.app_id,"record_id":b.record_id,"root":b.root,"device":b.device,"target_binding":b.target_binding},"revision":self.revision,"resource":self.resource,"action":self.action,"argument":self.argument})
    }
}

/// Borrowed, genuine caller evidence; never constructed from an actor string.
#[derive(Clone, Copy)]
pub(super) enum DurableContext<'a> {
    Peer(&'a VerifiedActionCaller, &'a CallerInvocation),
    Local {
        caller: &'a LocalCaller,
        request: &'a Request,
        offer: &'a ResolvedOffer,
        directories: &'a LocalActionDirectoryRegistry,
    },
}
impl<'a> DurableContext<'a> {
    pub(super) fn owner(self) -> InvocationOwner {
        match self {
            Self::Peer(c, _) => c.owner(),
            Self::Local { caller, .. } => caller.owner(),
        }
    }
    pub(super) fn time(self) -> Option<&'a crate::peer_actions::PolicyTime> {
        match self {
            Self::Peer(c, _) => c.policy_time.as_ref(),
            Self::Local { caller, .. } => Some(&caller.time),
        }
    }
    pub(super) fn invocation(self) -> tilo_actions::InvocationId {
        match self {
            Self::Peer(_, r) => r.invocation,
            Self::Local { request, .. } => request.invocation,
        }
    }
    pub(super) fn intent(self) -> serde_json::Value {
        match self {
            Self::Peer(_, r) => r.intent(),
            Self::Local { request, .. } => request.intent(),
        }
    }
    pub(super) fn origin(self) -> InvocationCaller {
        match self {
            Self::Peer(c, _) => InvocationCaller::Certified(c.clone()),
            Self::Local { caller, .. } => caller.origin(),
        }
    }
    pub(super) fn network(self) -> Option<tilo_relay::client::CertifiedAction> {
        match self {
            Self::Peer(c, _) => c.network.clone(),
            Self::Local { .. } => None,
        }
    }
    pub(super) fn before_admission(
        self,
        state: &CapnpBrokerState,
        host: uuid::Uuid,
        peer: u64,
        offered: &(PublishedSurface, PublishedAction),
        sender: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) -> Result<(), String> {
        let current = match self {
            Self::Peer(c, _) => state.current_action_caller(c)?,
            Self::Local {
                caller,
                request,
                offer,
                directories,
            } => {
                if request.binding.host != *host.as_bytes() {
                    return Err("durable invocation host changed".into());
                }
                let current = caller.current(state)?;
                offer.check_registration(state, directories, peer)?;
                if offer.binding_for_host(state, peer, host)? != request.binding {
                    return Err("original invocation target binding changed".into());
                }
                current
            }
        };
        state.require_current_stable_sender(peer, sender)?;
        let now = state.checked_policy_time(self.time())?.1 / 1_000_000_000;
        state.authorize_invocation_permission(
            &offered.0.route.cluster_id,
            &offered.0.app_id,
            &offered.1.action,
            offered.1.contract.permission,
            &current.broker_actor(),
            now,
        )?;
        state.checked_policy_time(self.time())?;
        Ok(())
    }
}

impl CapnpBrokerState {
    pub(super) fn authorize_invocation_permission(
        &self,
        room: &str,
        app: &str,
        action: &str,
        permission: tilo_actions::PermissionRequirement,
        actor: &BrokerActor,
        now: u64,
    ) -> Result<(), String> {
        let tree = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .ok_or("workspace no longer hosted")?;
        let caller = if actor.is_agent {
            // Preserve the existing common dispatch policy projection. Genuine
            // durable callers revalidate live membership/delegation separately.
            workspace.actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
        } else {
            workspace.actor_for_person_at(&actor.id, now)
        };
        let verb = match permission {
            tilo_actions::PermissionRequirement::None => None,
            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        };
        if verb.is_some_and(|verb| {
            workspace.decide_at(
                &caller,
                &ResourceRef::Action {
                    app: app.into(),
                    action: action.into(),
                },
                verb,
                now,
            ) != CapDecision::Allow
        }) {
            return Err("caller no longer authorized for invocation".into());
        }
        Ok(())
    }
    pub(super) fn require_current_stable_sender(
        &self,
        peer: u64,
        sender: &mpsc::UnboundedSender<InvocationEnvelope>,
    ) -> Result<(), String> {
        if !self
            .stable_invocation_subscribers
            .lock()
            .map_err(|_| "application capability unavailable")?
            .contains(&peer)
        {
            return Err("application_stable_identity_unavailable".into());
        }
        if sender.is_closed()
            || !self
                .invocation_subscribers
                .lock()
                .map_err(|_| "application invocation stream unavailable")?
                .get(&peer)
                .is_some_and(|current| !current.is_closed() && current.same_channel(sender))
        {
            return Err("application invocation stream closed or replaced".into());
        }
        Ok(())
    }
    fn local_status(
        &self,
        caller: &LocalCaller,
        host: uuid::Uuid,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<DurableInvocation>, String> {
        let current = caller.current(self)?;
        if host != self.invocation_host()? {
            return Err("durable invocation host mismatch".into());
        }
        self.expire_stale_invocations();
        let key = InvocationKey {
            room: caller.route.cluster_id.clone(),
            host,
            owner: caller.owner(),
            invocation,
        };
        let record = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("durable admission unavailable")?
            .lookup(&key)?;
        if let Some(record) = &record {
            let contract: tilo_actions::ActionContract =
                serde_json::from_value(record.intent["contract"].clone())
                    .map_err(|_| "journal action contract invalid")?;
            let app = record.intent["app_id"]
                .as_str()
                .ok_or("journal app invalid")?;
            let action = record.intent["action"]
                .as_str()
                .ok_or("journal action invalid")?;
            let now = self.checked_policy_time(Some(&caller.time))?.1 / 1_000_000_000;
            self.authorize_invocation_permission(
                &caller.route.cluster_id,
                app,
                action,
                contract.permission,
                &current.broker_actor(),
                now,
            )?;
        }
        caller.current(self)?;
        Ok(record)
    }
    fn local_existing(
        &self,
        caller: &LocalCaller,
        request: &Request,
    ) -> Result<Option<DurableInvocation>, String> {
        let existing = self.local_status(
            caller,
            uuid::Uuid::from_bytes(request.binding.host),
            request.invocation,
        )?;
        if existing
            .as_ref()
            .is_some_and(|record| record.intent["request"] != request.intent())
        {
            return Err("invocation identity reused with different intent".into());
        }
        Ok(existing)
    }
}

fn array<const N: usize>(bytes: &[u8], name: &str) -> Result<[u8; N], capnp::Error> {
    let value: [u8; N] = bytes
        .try_into()
        .map_err(|_| capnp::Error::failed(format!("invalid {name}")))?;
    if value == [0; N] {
        return Err(capnp::Error::failed(format!("empty {name}")));
    }
    Ok(value)
}
fn text(
    value: capnp::text::Reader<'_>,
    name: &str,
    max: usize,
    required: bool,
    identifier: bool,
) -> Result<String, capnp::Error> {
    let value = value.to_str().map_err(capnp::Error::from)?;
    if identifier {
        validate_directory_identifier(value, name, max, required)?;
    } else {
        validate_directory_text(value, name, max, required)?;
    }
    Ok(value.to_owned())
}
fn confirmation(value: &str) -> Result<Option<DirectoryActionConfirmation>, capnp::Error> {
    let value: serde_json::Value = if value.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(value).map_err(|error| {
            capnp::Error::failed(format!("invalid confirmation evidence: {error}"))
        })?
    };
    Ok(
        if let Some(phrase) = value
            .get("confirmation_phrase")
            .and_then(serde_json::Value::as_str)
        {
            Some(DirectoryActionConfirmation::Phrase(phrase.into()))
        } else if value.get("confirm").and_then(serde_json::Value::as_bool) == Some(true) {
            Some(DirectoryActionConfirmation::Confirmed)
        } else {
            None
        },
    )
}
fn write_observation(
    mut out: runtime_collab::collab_capnp::published_invocation_observation::Builder<'_>,
    host: uuid::Uuid,
    invocation: tilo_actions::InvocationId,
    record: Option<&DurableInvocation>,
) -> Result<(), capnp::Error> {
    out.set_host(host.as_bytes());
    out.set_invocation(invocation.0.as_bytes());
    if let Some(record) = record {
        let update = serde_json::to_string(&record.update)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        if update.len() > CapnpBrokerState::DIRECTORY_MAX_BYTES as usize {
            return Err(capnp::Error::failed(
                "invocation observation exceeds byte budget".into(),
            ));
        }
        out.set_found(true);
        out.set_id(record.wire_id);
        out.set_update_json(&update);
        out.set_uncertain(record.uncertain);
        out.set_cancellation_requested(record.cancellation_requested);
    } else {
        out.set_found(false);
        out.set_id(0);
        out.set_update_json("");
        out.set_uncertain(false);
        out.set_cancellation_requested(false);
    }
    Ok(())
}

pub(super) fn invoke(
    channel: Rc<CapnpChannelHandle>,
    params: channel_handle::InvokeResolvedPublishedActionDurableParams,
    mut results: channel_handle::InvokeResolvedPublishedActionDurableResults,
) -> Promise<(), capnp::Error> {
    let parsed = (|| {
        let p = params.get()?;
        let b = p.get_binding()?;
        let binding = runtime_collab::PublishedInvocationBinding {
            host: array(b.get_host()?, "invocation host")?,
            peer_id: b.get_peer_id(),
            app_id: text(
                b.get_app_id()?,
                "binding app",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
                true,
            )?,
            record_id: text(
                b.get_record_id()?,
                "binding record",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                false,
                true,
            )?,
            root: array(b.get_root()?, "binding root")?,
            device: array(b.get_device()?, "binding device")?,
            target_binding: array(b.get_target_binding()?, "target binding")?,
        };
        if binding.peer_id == 0 || binding.peer_id == ui::collaboration::CURRENT_CONNECTION_PEER_ID
        {
            return Err(capnp::Error::failed(
                "durable invocation needs exact target peer".into(),
            ));
        }
        let argument = text(
            p.get_argument()?,
            "invocation argument",
            DIRECTORY_MAX_PARAMS_BYTES,
            false,
            false,
        )?;
        let request = Request {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(array(
                p.get_invocation()?,
                "invocation UUID",
            )?)),
            binding,
            revision: text(
                p.get_revision()?,
                "invocation revision",
                DIRECTORY_MAX_QUERY_REVISION_BYTES,
                true,
                true,
            )?,
            resource: text(
                p.get_resource()?,
                "invocation resource",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
                true,
            )?,
            action: text(
                p.get_action()?,
                "invocation action",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
                true,
            )?,
            argument: p.get_has_argument().then_some(argument),
            confirmation: confirmation(&text(
                p.get_confirmation_json()?,
                "invocation confirmation",
                DIRECTORY_MAX_CONFIRMATION_BYTES,
                false,
                false,
            )?)?,
        };
        let caller = LocalCaller::capture(&channel).map_err(capnp::Error::failed)?;
        Ok((caller, request))
    })();
    let (caller, request) = match parsed {
        Ok(value) => value,
        Err(error) => return Promise::err(error),
    };
    Promise::from_future(async move {
        let state = &channel.state;
        let host = uuid::Uuid::from_bytes(request.binding.host);
        if let Some(record) = state
            .local_existing(&caller, &request)
            .map_err(capnp::Error::failed)?
        {
            return write_observation(
                results.get().init_observation(),
                host,
                request.invocation,
                Some(&record),
            );
        }
        if state
            .current_invocation_binding(
                &channel.directories,
                request.binding.peer_id,
                &caller.route.cluster_id,
                host,
            )
            .map_err(capnp::Error::failed)?
            != request.binding
        {
            return Err(capnp::Error::failed(
                "original invocation target binding changed".into(),
            ));
        }
        let actor = caller
            .current(state)
            .map_err(capnp::Error::failed)?
            .broker_actor();
        let offer = state
            .resolve_invocation_offer(
                &channel.directories,
                &actor,
                &caller.route.cluster_id,
                request.binding.peer_id,
                &request.revision,
                &request.resource,
                &request.action,
            )
            .await
            .map_err(capnp::Error::failed)?;
        let _admission = state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        let current = caller.current(state).map_err(capnp::Error::failed)?;
        if let Some(record) = state
            .local_existing(&caller, &request)
            .map_err(capnp::Error::failed)?
        {
            return write_observation(
                results.get().init_observation(),
                host,
                request.invocation,
                Some(&record),
            );
        }
        if state.invocation_host().map_err(capnp::Error::failed)? != host {
            return Err(capnp::Error::failed(
                "durable invocation host mismatch".into(),
            ));
        }
        offer
            .check_registration(state, &channel.directories, request.binding.peer_id)
            .map_err(capnp::Error::failed)?;
        if offer
            .binding_for_host(state, request.binding.peer_id, host)
            .map_err(capnp::Error::failed)?
            != request.binding
        {
            return Err(capnp::Error::failed(
                "original invocation target binding changed".into(),
            ));
        }
        let offered = offer.offered(request.binding.peer_id);
        let actor = current.broker_actor();
        let token = state
            .directory_confirmation_token(
                request.binding.peer_id,
                &request.resource,
                &request.action,
                request.argument.as_deref(),
                &actor,
                &offered.1.contract,
                request.confirmation.clone(),
            )
            .map_err(|error| {
                capnp::Error::failed(format!("directory confirmation refused: {error:?}"))
            })?;
        let context = DurableContext::Local {
            caller: &caller,
            request: &request,
            offer: &offer,
            directories: &channel.directories,
        };
        let job = state
            .dispatch_invocation_from_offered_owned(
                request.binding.peer_id,
                offered,
                request.argument.as_deref(),
                &actor,
                token.as_deref(),
                None,
                Some(context),
                Some(&request.revision),
                None,
                None,
                None,
                None,
            )
            .map_err(capnp::Error::failed)?;
        drop(job.completion);
        let record = state
            .local_existing(&caller, &request)
            .map_err(capnp::Error::failed)?
            .ok_or_else(|| capnp::Error::failed("admitted invocation missing".into()))?;
        write_observation(
            results.get().init_observation(),
            host,
            request.invocation,
            Some(&record),
        )
    })
}
pub(super) fn observe(
    channel: Rc<CapnpChannelHandle>,
    params: channel_handle::PublishedInvocationObservationParams,
    mut results: channel_handle::PublishedInvocationObservationResults,
) -> Promise<(), capnp::Error> {
    let outcome = (|| {
        let p = params.get()?;
        let host = uuid::Uuid::from_bytes(array(p.get_host()?, "invocation host")?);
        let id = tilo_actions::InvocationId(uuid::Uuid::from_bytes(array(
            p.get_invocation()?,
            "invocation UUID",
        )?));
        let caller = LocalCaller::capture(&channel).map_err(capnp::Error::failed)?;
        let record = channel
            .state
            .local_status(&caller, host, id)
            .map_err(capnp::Error::failed)?;
        write_observation(results.get().init_observation(), host, id, record.as_ref())
    })();
    match outcome {
        Ok(()) => Promise::ok(()),
        Err(error) => Promise::err(error),
    }
}
pub(super) fn cancel(
    channel: Rc<CapnpChannelHandle>,
    params: channel_handle::CancelDurablePublishedInvocationParams,
    mut results: channel_handle::CancelDurablePublishedInvocationResults,
) -> Promise<(), capnp::Error> {
    let outcome = (|| {
        let p = params.get()?;
        let host = uuid::Uuid::from_bytes(array(p.get_host()?, "invocation host")?);
        let id = tilo_actions::InvocationId(uuid::Uuid::from_bytes(array(
            p.get_invocation()?,
            "invocation UUID",
        )?));
        let caller = LocalCaller::capture(&channel).map_err(capnp::Error::failed)?;
        let _admission = channel
            .state
            .admission_lock
            .lock()
            .map_err(|_| capnp::Error::failed("invocation admission unavailable".into()))?;
        let record = channel
            .state
            .local_status(&caller, host, id)
            .map_err(capnp::Error::failed)?;
        if let Some(record) = &record {
            if !record.update.is_terminal() && !record.cancellation_requested {
                channel
                    .state
                    .cancel_durable_record(record)
                    .map_err(capnp::Error::failed)?;
            }
        }
        let record = channel
            .state
            .local_status(&caller, host, id)
            .map_err(capnp::Error::failed)?;
        write_observation(results.get().init_observation(), host, id, record.as_ref())
    })();
    match outcome {
        Ok(()) => Promise::ok(()),
        Err(error) => Promise::err(error),
    }
}
