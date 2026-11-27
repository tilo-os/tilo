//! What waits for a person's yes on this host.
//!
//! An action the caller's grants leave to a person is refused with what they would be shown
//! ([`ActionCallError::ConfirmationRequired`]) unless the call carries their approve
//! ([`InvokeRequest::approval`]). Whoever dispatches the call keeps it as its own asked
//! `tilo.invocations` record ([`AskedRecords`]); nothing here keeps a question, waits for an
//! answer or asks a device. The reading of a workspace's invocations lists the asked records a
//! person may answer, across the workspaces here, read from those records as they sync.
//!
//! What the records are written through is wrapped where a crate built on the workspaces says
//! ([`Workspaces::wrap_asked_records`]): what tells each one to the person it waits for.

use std::sync::Arc;

use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::remote::asked::{
    AskedBy, AskedDispatch, AskedFacts, AskedRecords, Named, RecordRef, Resolve,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    Pending,
};
use tilo_remote_actions::asked::AskedStore;

use super::Workspaces;

/// Who asks, as the system names `caller`: its key where its call came in on one, its agent by
/// the name its key goes by, an app by its id, else the person's own device.
#[must_use]
pub fn asked_by(caller: &Caller) -> AskedBy {
    if let Some(key) = caller.key() {
        return AskedBy::Agent(Named {
            id: key.id().to_owned(),
            name: key.label().to_owned(),
        });
    }
    let acting = caller.acting();
    if let Some(agent) = acting.agent_id() {
        return AskedBy::Agent(Named {
            id: agent.to_owned(),
            name: caller.agent_called().unwrap_or(agent).to_owned(),
        });
    }
    match acting.app() {
        Some(app) => AskedBy::App {
            app: app.to_string(),
            title: app.to_string(),
            author: None,
        },
        None => AskedBy::default(),
    }
}

pub use tilo_actions::remote::asked::approved_or_asks;

/// Whether `caller`, on the device `certificate` certifies (this one where none is named),
/// answers for its person: a device that holds their identity, never a key they made for their
/// programs, which acts as them and still answers nothing for them, nor a stand-in or a device
/// confined to one workspace (its VM, or one placed for its records).
pub(crate) fn answers_on_a_device_of_theirs(
    caller: &Caller,
    certificate: Option<&tilo_user_identity::DeviceCert>,
) -> bool {
    caller.key().is_none()
        && certificate.is_none_or(|certificate| {
            certificate.capabilities.agent.is_none()
                && !certificate.capabilities.stand_in
                && certificate.capabilities.workspace_name().is_none()
        })
}

/// Why a call that waits for its person's yes could not be kept for them.
pub(crate) const NO_WORKSPACE_KEEPS_IT: &str = "no workspace here keeps it for its person";

/// What wraps the records each of a host's dispatches writes its asked calls through
/// ([`Workspaces::wrap_asked_records`]).
pub type AskedWrap = Arc<dyn Fn(Arc<dyn AskedRecords>) -> Arc<dyn AskedRecords> + Send + Sync>;

impl Workspaces {
    /// This host's one store of the calls that wait for a person's yes, kept and answered
    /// through its workspaces: grants written on the runtime `runtime` names, every waiting
    /// record kept in the record of the workspace it acts in, and the calls that waited before
    /// the host started waiting here again. The store asks on this device, and is handed every
    /// dispatch of this host ([`Self::keep_asked_in`]), so the reading of each workspace's
    /// invocations lists what waits there
    /// and `tilo.invocations#resolve` answers it.
    ///
    /// # Errors
    /// The workspaces keep no store of the calls that wait.
    pub async fn open_asked_store(
        &self,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<AskedStore>, String> {
        let earlier = self.asked_kept().await;
        let store = AskedStore::opened(
            Some(self.asked_grants(runtime)),
            Some(self.asked_keeping()),
            earlier,
        )?;
        store.asks_on(&self.device_cert().device_public_key);
        store.keep_no_secrets(self.secret_parameters());
        self.keep_asked_in(&store);
        Ok(store)
    }

    /// What each call's action takes secret, as this device publishes the action: the
    /// parameters its descriptor declares [`ParamType::Secret`](tilo_actions::ParamType::Secret),
    /// read from the provider registered for it ([`Self::providers`]) or from the host's own
    /// actions. `None` for an action described nowhere here (one an app lists only as it is
    /// asked, one only another device answers), whose every argument is then taken for a
    /// secret. What keeps a call (its asked record, the device's record of its invocations)
    /// keeps none of them.
    #[must_use]
    pub fn secret_parameters(&self) -> tilo_actions::remote::secret_arguments::SecretParameters {
        let providers = self.providers().downgrade();
        Arc::new(move |target| {
            let providers = providers.upgrade();
            let registered = providers.as_ref().and_then(|providers| {
                providers
                    .get(&target.provider)
                    .or_else(|| providers.published(target, super::providers::Reach::ThisDevice))
            });
            let params = registered
                .and_then(|registered| {
                    tilo_actions::api::locate(&registered.published, &target.address)
                        .map(|(_, action)| action.params().to_vec())
                })
                .or_else(|| {
                    providers
                        .as_ref()?
                        .in_workspace(target)
                        .and_then(|answers| answers.params(target))
                })
                .or_else(|| super::actions::published_params(target));
            params.map(|params| tilo_actions::remote::secret_arguments::secret_parameters(&params))
        })
    }

    /// Whether each call's action answers a secret, as this device publishes the action
    /// ([`tilo_actions::SECRET_ANSWER_TAG`]): what every record of the call keeps out of its
    /// answer, whoever asked for it. Read from the provider registered for the action or from
    /// the host's own actions, as [`Self::secret_parameters`] reads what its arguments are.
    #[must_use]
    pub fn secret_answers(&self) -> tilo_actions::remote::secret_arguments::SecretAnswers {
        let providers = self.providers().downgrade();
        Arc::new(move |request: &InvokeRequest| {
            let target = &request.target;
            if tilo_actions::remote::answers_secret(request) {
                return true;
            }
            let providers = providers.upgrade();
            if providers
                .as_ref()
                .and_then(|providers| providers.in_workspace(target))
                .is_some_and(|answers| answers.answers_a_secret(target))
            {
                return true;
            }
            let registered = providers.as_ref().and_then(|providers| {
                providers
                    .get(&target.provider)
                    .or_else(|| providers.published(target, super::providers::Reach::ThisDevice))
            });
            let published = registered
                .and_then(|registered| {
                    tilo_actions::api::locate(&registered.published, &target.address)
                        .map(|(_, action)| action.contract.answers_a_secret())
                })
                .unwrap_or(false);
            // A host's own action is read from its declaration even where a provider also
            // lists it: a surface that carries no tags must not hide it.
            published || super::actions::published_answers_secret(target) == Some(true)
        })
    }

    /// Keep every call that waits for a person's yes as its asked record in `store`, which the
    /// host hands this once and hands every dispatch of its own: a person's answer to one comes
    /// back to the dispatch that keeps it.
    fn keep_asked_in(&self, store: &Arc<AskedStore>) {
        assert!(
            self.inner.asked_store.set(Arc::clone(store)).is_ok(),
            "a host keeps one store of the calls that wait"
        );
        self.action_service()
            .admit_approvals_from(Arc::<AskedStore>::clone(store));
        self.link_asked();
    }

    /// Hand a person's approve of a call this host's dispatch keeps back to `dispatch`.
    ///
    /// # Panics
    /// A dispatch was handed already: an approve would reach that one, never this.
    pub(crate) fn answer_asked_with(&self, dispatch: Arc<dyn AskedDispatch>) {
        assert!(
            self.inner.asked_dispatch.set(dispatch).is_ok(),
            "a host hands one dispatch the calls that wait"
        );
        self.link_asked();
    }

    /// Wrap the records this host's dispatches write their asked calls through in `wrap` from
    /// now on: what tells each one to the person it waits for. `wrap` is handed each dispatch's
    /// records once the host names its store and that dispatch, with the store kept by then
    /// ([`Self::asked_store`]).
    ///
    /// # Panics
    /// A wrap was named already, or the records were linked before it: an asked call would be
    /// written through records nothing wraps.
    pub fn wrap_asked_records(&self, wrap: AskedWrap) {
        assert!(
            self.inner.asked_records.get().is_none()
                && self.inner.asked_device_calls.get().is_none(),
            "the asked records are wrapped before the host links them"
        );
        assert!(
            self.inner.asked_wrap.set(wrap).is_ok(),
            "the asked records are wrapped once"
        );
        self.link_asked();
    }

    /// The records each dispatch here writes through, once the store is named: the calls other
    /// devices bring this one's providers from then on ([`Self::device_call_records`]), and this
    /// host's own once its dispatch is named too.
    fn link_asked(&self) {
        let Some(store) = self.inner.asked_store.get() else {
            return;
        };
        let wrapped = |records| match self.inner.asked_wrap.get() {
            Some(wrap) => wrap(records),
            None => records,
        };
        if self.inner.asked_device_calls.get().is_none() {
            let work = Arc::downgrade(&self.inner.jobs);
            let work: std::sync::Weak<dyn AskedDispatch> = work;
            let _ = self.inner.asked_device_calls.set(wrapped(
                store.for_dispatch(super::device_actions::DEVICE_CALLS_DISPATCH, work),
            ));
        }
        if let Some(dispatch) = self.inner.asked_dispatch.get()
            && self.inner.asked_records.get().is_none()
        {
            let records = wrapped(
                store.for_dispatch(super::actions::HOST_DISPATCH, Arc::downgrade(dispatch)),
            );
            let _ = self.inner.asked_records.set(records);
        }
    }

    /// Where a call another device brings a provider here waits for its person's yes, once the
    /// host named its store: an approve of one is handed back to this device's work, which runs
    /// the call then.
    pub(crate) fn device_call_records(&self) -> Option<Arc<dyn AskedRecords>> {
        self.inner.asked_device_calls.get().cloned()
    }

    /// The store asked records are kept in here, once the host named it.
    pub fn asked_store(&self) -> Option<Arc<AskedStore>> {
        self.inner.asked_store.get().cloned()
    }

    /// Answer `request`, a person's `<record>#resolve`, as `caller`, on this device: its receipt
    /// once the call it answers is admitted again or ended. The record is answered where it is
    /// kept: here, when this device keeps it, and otherwise on the device of the person's in the
    /// workspace `caller` answers in that keeps it, which this device brings the answer to as
    /// theirs ([`Self::resolve_kept_here`] there). Who may answer is the store's one rule, and
    /// what this device brings elsewhere is only its own person's answer: an agent's, an
    /// app's or an automation's is refused here, as it would be there.
    pub fn resolve_asked(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let kept_here = self
            .asked_store()
            .is_some_and(|store| store.record(&request.target.address.resource).is_some());
        let own_person = hex::encode(self.device_cert().user_public_key);
        let workspace = caller
            .workspace()
            .and_then(collab_types::WorkspaceId::parse);
        let (Some(workspace), false) = (workspace, kept_here) else {
            return self.resolve_kept_here(caller, None, request);
        };
        if tilo_actions::remote::asked::answering_person(caller) != Some(own_person.as_str())
            || !answers_on_a_device_of_theirs(caller, None)
        {
            return self.resolve_kept_here(caller, None, request);
        }
        let (this, caller, request) = (self.clone(), caller.clone(), request.clone());
        let (done, answer) = tilo_actions::remote::pending();
        tokio::spawn(async move {
            done.complete(this.resolve_where_kept(workspace, &caller, request).await);
        });
        answer
    }

    /// `request`, the answer of this device's person to a record another device of theirs in
    /// `workspace` keeps, brought to each of those devices: the one that keeps it answers, and
    /// every other one has nothing asked under it.
    async fn resolve_where_kept(
        &self,
        workspace: collab_types::WorkspaceId,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Result<InvocationReceipt, ActionCallError> {
        use futures::StreamExt as _;
        let nothing_asked = || ActionCallError::Refused {
            reason: tilo_remote_actions::asked::NOTHING_ASKED.into(),
        };
        let here = self.device_cert();
        let (person, this_device) = (here.user_public_key, here.device_public_key);
        let Some(reading) = self.reading(workspace).await else {
            return Err(nothing_asked());
        };
        let (devices, keeper) = reading.replica.read(|peer| {
            let devices = peer
                .certificates()
                .into_iter()
                .filter(|certificate| {
                    certificate.user_public_key == person
                        && certificate.device_public_key != this_device
                        && certificate.capabilities.records.is_none()
                        && answers_on_a_device_of_theirs(caller, Some(certificate))
                })
                .map(|certificate| certificate.device_public_key)
                .collect::<Vec<_>>();
            (
                devices,
                super::standing::keeping_device_elsewhere(peer, &this_device),
            )
        });
        let workspace = workspace.to_string();
        let mut asked: futures::stream::FuturesUnordered<_> = devices
            .into_iter()
            .filter_map(|device| self.reached(device, keeper.as_ref(), &workspace))
            .map(|device| ActionProvider::invoke(&device, caller, request.clone()))
            .collect();
        // The device that keeps the record answers it; every other one has nothing asked under
        // it, keeps no asked records, or is not reached now. A refusal is the answer once no
        // device took it.
        let mut refused = None;
        while let Some(answer) = asked.next().await {
            match answer {
                Some(Ok(receipt)) => return Ok(receipt),
                Some(Err(ActionCallError::Refused { reason }))
                    if reason == tilo_remote_actions::asked::NOTHING_ASKED => {}
                None
                | Some(Err(
                    ActionCallError::UnknownAction
                    | ActionCallError::Unavailable { .. }
                    | ActionCallError::Unanswered,
                )) => {}
                Some(Err(error)) => {
                    refused.get_or_insert(error);
                }
            }
        }
        Err(refused.unwrap_or_else(nothing_asked))
    }

    /// The actions of `device`, a device of `workspace`, as this device reaches it: over the
    /// link `device` holds open to this one, else dialled, at the address it signed into the
    /// workspace where it is `keeper`, the device keeping the workspace, and by its key
    /// otherwise.
    fn reached(
        &self,
        device: [u8; 32],
        keeper: Option<&([u8; 32], iroh::EndpointAddr)>,
        workspace: &str,
    ) -> Option<tilo_peer_state::actions::RemoteDevice> {
        let service = self.action_service();
        if let Some(linked) = service.remote_held(device, workspace) {
            return Some(linked);
        }
        let address = match keeper {
            Some((kept_by, address)) if *kept_by == device => address.clone(),
            _ => iroh_base::EndpointAddr::new(iroh_base::EndpointId::from_bytes(&device).ok()?),
        };
        Some(service.remote(&self.inner.host.endpoint, &address, workspace))
    }

    /// Answer `request`, a person's `<record>#resolve`, as `caller`, from the records this
    /// device keeps, on the device `certificate` certifies where the answer came over a link:
    /// its receipt once the call it answers is admitted again or ended. Only a device that
    /// holds the person's identity answers for them ([`answers_on_a_device_of_theirs`]).
    pub fn resolve_kept_here(
        &self,
        caller: &Caller,
        certificate: Option<&tilo_user_identity::DeviceCert>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !answers_on_a_device_of_theirs(caller, certificate) {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: tilo_remote_actions::asked::ON_YOUR_DEVICE.into(),
            }));
        }
        let invocation = request.invocation;
        let answer = match &request.argument {
            InvokeArgument::Json(arguments) => {
                serde_json::from_value::<Resolve>(Value::Object(arguments.clone()))
            }
            InvokeArgument::None | InvokeArgument::Text(_) => {
                serde_json::from_value::<Resolve>(Value::Object(serde_json::Map::new()))
            }
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => {
                return Pending::ready(Err(ActionCallError::InvalidRequest {
                    reason: error.to_string(),
                }));
            }
        };
        let Some(store) = self.asked_store() else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let Some(at) = store.record(&request.target.address.resource) else {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: tilo_remote_actions::asked::NOTHING_ASKED.into(),
            }));
        };
        store.resolve(&at, caller, &answer).map(move |resolved| {
            resolved.map(|()| InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            })
        })
    }

    /// Where this host's own dispatch keeps the calls that wait for a person's yes, once the
    /// host named the store, each told to its person.
    pub fn asked_records(&self) -> Option<Arc<dyn AskedRecords>> {
        self.inner.asked_records.get().cloned()
    }

    /// Keep `request`, which `caller` made and which waits for its person's yes showing them
    /// `facts`, as its asked record: in the workspace the call acts in, else in the person's
    /// own workspace here. The person asked is the request's own, as its caller names them,
    /// never whoever admitted the connection it came on. The record is kept under the
    /// workspace's identity, which is the `workspace` every reading of its invocations is
    /// addressed by, never the name it shows: a record kept under the name would wait for its
    /// person where no reading of the workspace finds it. `None` where no store here keeps
    /// asked records.
    pub(crate) async fn keep_asked(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Option<Result<RecordRef, String>> {
        let records = self.asked_records()?;
        Some(match self.asked_workspace(workspace_id, caller).await {
            Some(workspace) => records.write_asked(&workspace, caller, request, facts),
            None => Err(NO_WORKSPACE_KEEPS_IT.to_owned()),
        })
    }

    /// The workspace a call `caller` made in `workspace_id` is kept in while it waits for its
    /// person, by the identity every reading of its invocations is addressed by: the workspace
    /// it acts in, else the person's own workspace here.
    pub(crate) async fn asked_workspace(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
    ) -> Option<String> {
        match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => caller
                .user_public_key()
                .and_then(|person| self.own_workspace_of(person)),
        }
    }

    /// Whether a grant in `workspace`'s policy lets `agent` take `node`, an action that
    /// reaches outside, without its person's yes each time: a grant decides it, never a
    /// default.
    pub async fn granted(
        &self,
        workspace: &str,
        agent: &tilo_member_types::MemberId,
        node: &TreePath,
    ) -> bool {
        self.granted_answer(workspace, agent, node).await == Some(Answer::Allow)
    }

    /// What a grant in `workspace`'s policy answers `agent` taking `node`, an action that
    /// reaches outside: `None` where no grant decides it. One a grant denies is refused, never
    /// put to its person.
    pub async fn granted_answer(
        &self,
        workspace: &str,
        agent: &tilo_member_types::MemberId,
        node: &TreePath,
    ) -> Option<Answer> {
        let policy = self.policy_of(workspace).await?;
        let decision = decide(
            policy.grants.iter(),
            &[Grantee::Member(agent.clone())],
            node,
            ActionKind::Outside,
            &Context::at(tilo_clock::unix_now_ms()),
        );
        decision.grant().is_some().then_some(decision.answer)
    }
}
