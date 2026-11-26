//! What waits for a person's yes on this host.
//!
//! An action the caller's grants leave to a person is refused with what they would be shown
//! ([`ActionCallError::ConfirmationRequired`]) unless the call carries their approve
//! ([`InvokeRequest::approval`]). Whoever dispatches the call keeps it as its own asked
//! `tilo.invocations` record ([`AskedRecords`]); nothing here keeps a question, waits for an
//! answer or asks a device. The reading of a workspace's invocations lists the asked records a
//! person may answer, across the workspaces here, read from those records as they sync.
//!
//! Each record written here is told to the person it waits for as a thing of their log
//! ([`Workspaces::tell`]), which their devices raise as a notification and which reaches a
//! browser of theirs when no device is looking: a question with Approve and Decline, which
//! answer the record itself, or, where only they can type what it takes, none, and opening it
//! shows the ask. Once the record is answered anywhere, the thing waits no more.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::remote::asked::{
    self, AskedBy, AskedDispatch, AskedFacts, AskedRecords, Named, RecordRef, Resolve,
};
use tilo_actions::remote::person_telling::{About, Kind, Offered, OfferedTarget, Tell, Teller};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
};
use tilo_lock::MutexRecover as _;
use tilo_remote_actions::asked::AskedStore;

use super::{ToTell, WeakWorkspaces, Workspaces};

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

/// The person `caller` asks, as their key.
fn asked_person(caller: &Caller) -> Option<[u8; 32]> {
    hex::decode(asked::asked_person(caller)?)
        .ok()?
        .try_into()
        .ok()
}

/// The id the record `record` is told under in its person's log.
fn told_id(record: &RecordRef) -> String {
    format!("asked:{}/{}", record.workspace, record.record)
}

/// What the person reads of `facts` where the record is told: the asker's own words, else
/// the verb and what it is on.
fn told_words(facts: &AskedFacts) -> String {
    if !facts.words.trim().is_empty() {
        return facts.words.clone();
    }
    match &facts.on {
        Some(on) => format!("{} {on}", facts.verb),
        None => facts.verb.clone(),
    }
}

/// The thing the asked `record` is told as: who asks, in their words, with Approve and
/// Decline answering the record, except where only the person can type what it takes.
fn telling(record: &RecordRef, facts: &AskedFacts) -> ToTell {
    let answer = |id: &str, label: &str, main: bool| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("resolution".into(), Value::String(id.to_owned()));
        Offered {
            id: id.to_owned(),
            label: label.to_owned(),
            main,
            target: OfferedTarget {
                app: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
                action: asked::RESOLVE.to_owned(),
            },
            arguments,
        }
    };
    let actions = if facts.answered_by.is_some() || !facts.takes.is_empty() {
        Vec::new()
    } else {
        vec![
            answer("approve", "Approve", true),
            answer("decline", "Decline", false),
        ]
    };
    let from = match &facts.by {
        AskedBy::Agent(agent) => Some(Teller {
            agent: agent.id.clone(),
            name: agent.name.clone(),
        }),
        AskedBy::App { .. } | AskedBy::Device(_) | AskedBy::Program { .. } => None,
    };
    ToTell {
        id: Some(told_id(record)),
        kind: Kind::Question,
        tell: Tell {
            words: told_words(facts),
            about: Some(About {
                workspace: record.workspace.clone(),
                app: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
            }),
            actions,
            ..Tell::default()
        },
        from,
        workspace: Some(record.workspace.clone()),
        words_stay: false,
        spender: None,
    }
}

/// The asked records as the host keeps them: each one written is told to the person it waits
/// for, and taken back from their log once it is answered.
struct Told {
    records: Arc<dyn AskedRecords>,
    workspaces: WeakWorkspaces,
    /// The records told and not yet seen answered, with the person each waits for.
    waiting: Mutex<BTreeSet<([u8; 32], RecordRef)>>,
}

impl Told {
    /// Take back from their log each record told that waits no more.
    fn settle(&self, workspaces: &Workspaces) {
        let waiting = self.waiting.lock_recover().clone();
        let mut asked: std::collections::BTreeMap<[u8; 32], BTreeSet<RecordRef>> =
            std::collections::BTreeMap::new();
        for (person, record) in waiting {
            let still = asked.entry(person).or_insert_with(|| {
                self.records
                    .asked_of(&person)
                    .into_iter()
                    .map(|asked| asked.record)
                    .collect()
            });
            if still.contains(&record) {
                continue;
            }
            if let Err(error) = workspaces.withdraw_told(&person, &told_id(&record)) {
                tracing::debug!(%error, "an answered ask stays in its person's log");
            }
            self.waiting.lock_recover().remove(&(person, record));
        }
    }
}

impl AskedRecords for Told {
    fn write_asked(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String> {
        let record = self
            .records
            .write_asked(workspace, caller, request, facts)?;
        let value = (asked_person(caller), self.workspaces.upgrade());
        if let (Some(person), Some(workspaces)) = value {
            self.waiting.lock_recover().insert((person, record.clone()));
            let told = telling(&record, facts);
            tokio::spawn(async move {
                if let Err(error) = workspaces.tell(&person, told).await {
                    tracing::warn!(%error, "an asked record was not told to its person");
                }
            });
        }
        Ok(record)
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<asked::Asked> {
        self.records.asked_of(person)
    }

    fn changed(&self) -> Pending<()> {
        self.records.changed()
    }
}

impl Workspaces {
    /// `records`, each one written told to the person it waits for and taken back from their
    /// log once it is answered: what every dispatch of this host keeps its asked calls through.
    pub fn told(&self, records: Arc<dyn AskedRecords>) -> Arc<dyn AskedRecords> {
        let told = Arc::new(Told {
            records,
            workspaces: self.downgrade(),
            waiting: Mutex::default(),
        });
        let follows = Arc::downgrade(&told);
        tokio::spawn(async move {
            loop {
                let Some(changed) = follows.upgrade().map(|told| told.records.changed()) else {
                    return;
                };
                if changed.await.is_none() {
                    return;
                }
                let (Some(told), Some(workspaces)) = (follows.upgrade(), told_workspaces(&follows))
                else {
                    return;
                };
                told.settle(&workspaces);
            }
        });
        told
    }

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
            let registered = providers.upgrade().and_then(|providers| {
                providers
                    .get(&target.provider)
                    .or_else(|| providers.published(target, super::providers::Reach::ThisDevice))
            });
            let params = registered
                .and_then(|registered| {
                    tilo_actions::api::locate(&registered.published, &target.address)
                        .map(|(_, action)| action.params().to_vec())
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
            let registered = providers.upgrade().and_then(|providers| {
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
        self.keep_vault_asks_in(store);
        self.action_service()
            .admit_approvals_from(Arc::clone(store));
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

    /// The records this host's dispatch writes through, once both the store and the dispatch
    /// are named.
    fn link_asked(&self) {
        if let (Some(store), Some(dispatch)) = (
            self.inner.asked_store.get(),
            self.inner.asked_dispatch.get(),
        ) {
            let _ =
                self.inner.asked_records.set(self.told(
                    store.for_dispatch(super::actions::HOST_DISPATCH, Arc::downgrade(dispatch)),
                ));
        }
    }

    /// The store asked records are kept in here, once the host named it.
    pub fn asked_store(&self) -> Option<Arc<AskedStore>> {
        self.inner.asked_store.get().cloned()
    }

    /// Answer `request`, a person's `<record>#resolve`, as `caller`: its receipt once the call
    /// it answers is admitted again or ended.
    pub fn resolve_asked(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
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
        let workspace = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => caller
                .user_public_key()
                .and_then(|person| self.own_workspace_of(person)),
        };
        Some(match workspace {
            Some(workspace) => records.write_asked(&workspace, caller, request, facts),
            None => Err("no workspace here keeps it for its person".to_owned()),
        })
    }

    /// Whether a grant in `workspace`'s policy lets `agent` take `node`, an action that
    /// reaches outside, without its person's yes each time: a grant decides it, never a
    /// default.
    pub async fn granted(
        &self,
        workspace: &str,
        agent: &tilo_action_policy::MemberId,
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
        agent: &tilo_action_policy::MemberId,
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

/// The workspaces `told` tells for, while both are still there.
fn told_workspaces(told: &std::sync::Weak<Told>) -> Option<Workspaces> {
    told.upgrade()?.workspaces.upgrade()
}
