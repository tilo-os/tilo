//! What a device that keeps a person's root runs at it: the calls kept for it
//! ([`AT_ROOT`]) by their other devices, their agents and the hosts of their workspaces.
//!
//! A call only the person's root signs, asked where no root of theirs is kept, waits as its asked
//! record in the workspace it was asked in. A device that keeps the root takes the record in
//! ([`AskedStore::take_in`]) and asks the person there, once. On their approve the runner takes
//! the record before anything runs ([`AskedStore::take`]), so the approve is used once: a second
//! one finds it answered. It runs the call as the person and writes how it ended into the record
//! ([`AskedStore::settle_for`]), which whoever asked follows.
//!
//! The service runs one such call itself: a new workspace of the person's, asked by an agent whose
//! grant reaches one workspace ([`kept_for_root`]). It is founded here, as the person and with
//! them as its owner ([`Workspaces::found`]). The agent is let work in it only where the person
//! picked that with their approve ([`LET_WORK`]). Every other call is the host's own
//! ([`RootKept`]): the daemon runs the device's root-only actions through it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use serde_json::Value;
use tilo_actions::remote::asked::{AT_ROOT, AskedDispatch, AskedFacts, KeptAsk};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_remote_actions::asked::AskedStore;
use tilo_server_protocol::workspaces::{
    CREATE_OWN_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
};
use tilo_user_identity::DeviceCert;

use super::{WeakWorkspaces, Workspaces};

/// A call the host runs at the root, on its way to how it ended.
pub type RootRun = Pin<Box<dyn Future<Output = InvocationUpdate> + Send>>;

/// A certificate the person's root signs, on its way.
pub type Certifying = Pin<Box<dyn Future<Output = Result<Option<DeviceCert>, String>> + Send>>;

/// What the host that keeps the person's root does with it, beside what this service runs
/// itself: its own operations only that root signs, and the certificate that lets an agent's
/// key work in a workspace.
pub trait RootKept: Send + Sync {
    /// Whether `request` is one of the host's own operations only the person's root signs.
    fn runs(&self, request: &InvokeRequest) -> bool;

    /// Run `request`, which [`Self::runs`], as `caller`'s person with their root, to its end.
    fn run(&self, request: InvokeRequest, caller: Caller) -> RootRun;

    /// Certify the agent key `key` for `workspace` with `person`'s root, under `name`, as the
    /// agent `agent` there: its certificate, or `None` where the person revoked the key.
    fn certify_agent(
        &self,
        person: [u8; 32],
        key: [u8; 32],
        workspace: &str,
        agent: &str,
        name: &str,
    ) -> Certifying;
}

/// The choice a make asked by an agent offers its person, by its place among the choices: let
/// the agent that asked work in the new workspace. Not picked, the agent gets no reach there.
pub const LET_WORK: usize = 0;

/// The words of [`LET_WORK`], for the agent `agent`.
#[must_use]
pub fn let_work(agent: &str) -> String {
    format!("Let {agent} work in it")
}

/// The name a new workspace of the person's is asked for with, where `request` is a make the
/// service runs at the root: a host's `workspace.create-own` naming a new workspace, and nothing
/// else (no workspace of theirs to enter, no labels).
fn asked_make(request: &InvokeRequest) -> Option<String> {
    let target = &request.target;
    if target.provider != HOST_ACTIONS_PROVIDER
        || target.address.resource != HOST_WORKSPACES_RESOURCE
        || target.address.action != CREATE_OWN_WORKSPACE
    {
        return None;
    }
    let InvokeArgument::Json(arguments) = &request.argument else {
        return None;
    };
    asked_name(arguments)
}

/// The name `arguments` of a host's `workspace.create-own` ask a new workspace for, where they
/// name nothing else (no workspace of the person's to enter, no labels).
fn asked_name(arguments: &serde_json::Map<String, Value>) -> Option<String> {
    if arguments.contains_key("workspace") || arguments.contains_key("labels") {
        return None;
    }
    arguments
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Whether `request`, asked under `certificate`, waits for a device that keeps its person's
/// root rather than for this host: a make of a new workspace by a key whose grant reaches one
/// workspace already made. No host founds that for the key; the person's own device does, once
/// they say yes there.
#[must_use]
pub fn kept_for_root(certificate: Option<&DeviceCert>, request: &InvokeRequest) -> bool {
    certificate.is_some_and(held_to_its_workspace) && asked_make(request).is_some()
}

/// [`kept_for_root`] for a host's `workspace.create-own` asked with `arguments` under
/// `certificate`.
pub(crate) fn waits_at_root(
    certificate: &DeviceCert,
    arguments: &serde_json::Map<String, Value>,
) -> bool {
    held_to_its_workspace(certificate) && asked_name(arguments).is_some()
}

/// Whether `certificate` is an agent's grant that reaches its one workspace and makes none.
fn held_to_its_workspace(certificate: &DeviceCert) -> bool {
    certificate
        .capabilities
        .agent
        .as_ref()
        .is_some_and(|scope| !scope.makes_workspaces())
}

/// `facts` as the person is asked a make kept for their root, asked under `certificate`: the
/// agent named as its person named its key, with the choice to let it work in the new
/// workspace, which they may leave unpicked.
#[must_use]
pub fn asked_at_root(mut facts: AskedFacts, certificate: Option<&DeviceCert>) -> AskedFacts {
    let label = certificate.map(|certificate| certificate.device_label.clone());
    let agent = match (&mut facts.by, label) {
        (tilo_actions::remote::asked::AskedBy::Agent(named), Some(label)) => {
            named.name.clone_from(&label);
            label
        }
        (tilo_actions::remote::asked::AskedBy::Agent(named), None) => named.name.clone(),
        (_, label) => label.unwrap_or_else(|| "the agent".to_owned()),
    };
    facts.choices = vec![let_work(&agent)];
    facts.many = true;
    facts
}

/// What a host keeps beside a make it keeps for the person's root: the certificate the agent's
/// key asked under, which names the key and what the person called it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MakeContext {
    certificate: DeviceCert,
}

/// What a host keeps beside one make as it writes its record: never handed an answer.
struct KeptBeside(Value);

impl AskedDispatch for KeptBeside {
    fn context(&self, _: InvocationId) -> Value {
        self.0.clone()
    }

    fn readmit(&self, _: &Caller, _: InvokeRequest, _: &Value) -> Result<(), ActionCallError> {
        Err(refused(tilo_actions::remote::asked::ANSWERED_AT_ROOT))
    }

    fn declined(&self, _: &Caller, _: InvocationId) -> Result<(), ActionCallError> {
        Ok(())
    }
}

impl Workspaces {
    /// Keep `request`, which `caller` asked under `certificate` and only a device that keeps
    /// the person's root runs, as its asked record for that device, showing `facts`: in the
    /// workspace the call acts in, else in the person's own workspace here. `None` where no
    /// store here keeps asked records.
    pub(crate) async fn keep_for_root(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        (caller, certificate): (&Caller, Option<&DeviceCert>),
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Option<Result<tilo_actions::remote::asked::RecordRef, String>> {
        let store = self.asked_store()?;
        let workspace = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => caller
                .user_public_key()
                .and_then(|person| self.own_workspace_of(person)),
        };
        let Some(workspace) = workspace else {
            return Some(Err("no workspace here keeps it for its person".to_owned()));
        };
        let context = certificate
            .map(|certificate| MakeContext {
                certificate: certificate.clone(),
            })
            .and_then(|context| serde_json::to_value(context).ok())
            .unwrap_or(Value::Null);
        let beside: Arc<dyn AskedDispatch> = Arc::new(KeptBeside(context));
        let records = store.writes_for(AT_ROOT, Arc::downgrade(&beside));
        Some(records.write_asked(&workspace, caller, request, facts))
    }

    /// Run here what waits for a device that keeps the person's root, with `kept`, the root
    /// this host keeps: every call kept for it in the workspaces here is taken in and asked of
    /// the person here, now and as more are kept. Once; a second call keeps the first.
    ///
    /// # Errors
    /// The workspaces keep no store of the calls that wait.
    pub fn serve_at_root(&self, kept: Arc<dyn RootKept>) -> Result<(), String> {
        let store = self
            .asked_store()
            .ok_or("the workspaces here keep no calls that wait")?;
        let at_root = AtRoot::new(&store, self, kept);
        if self.inner.at_root.set(Arc::clone(&at_root)).is_err() {
            return Ok(());
        }
        at_root.serve();
        let following = Arc::downgrade(&at_root);
        self.follow_asked_at_root(move |asks| {
            if let Some(at_root) = following.upgrade() {
                at_root.kept_changed(asks);
            }
        });
        Ok(())
    }

    /// Whether this host runs what waits for a device that keeps the person's root.
    #[must_use]
    pub fn serves_at_root(&self) -> bool {
        self.inner.at_root.get().is_some()
    }
}

/// The one runner of what waits for a device that keeps the person's root, at the store its
/// records are answered through.
pub struct AtRoot {
    store: Arc<AskedStore>,
    workspaces: WeakWorkspaces,
    kept: Arc<dyn RootKept>,
    runtime: tokio::runtime::Handle,
}

impl AtRoot {
    /// The runner at `store`, founding in `workspaces` and running the host's own operations
    /// with `kept`. It answers nothing until it serves ([`Self::serve`]). Call it inside the
    /// runtime the calls run on.
    #[must_use]
    pub fn new(
        store: &Arc<AskedStore>,
        workspaces: &Workspaces,
        kept: Arc<dyn RootKept>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: Arc::clone(store),
            workspaces: workspaces.downgrade(),
            kept,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Answer here, from now on, the approves of what waits for a device that keeps the
    /// person's root. The store holds this runner weakly: it answers while its holder keeps it.
    pub fn serve(self: &Arc<Self>) {
        let dispatch: Weak<dyn AskedDispatch> = Arc::downgrade(self) as Weak<dyn AskedDispatch>;
        let _ = self.store.for_dispatch(AT_ROOT, dispatch);
    }

    /// `asks`, the calls kept for a device that keeps the person's root, as they stand now:
    /// those that wait are taken in, for the person to answer here once.
    pub fn kept_changed(&self, asks: Vec<KeptAsk>) {
        self.store.take_in(asks);
    }

    /// The make `request` asks for `name`, as the person: founded once, here, with them as its
    /// owner, and the agent that asked let work in it where `request`'s approve picked that.
    fn make(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        name: String,
        context: &Value,
    ) -> Result<RootRun, ActionCallError> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or_else(|| refused("this device keeps no workspaces"))?;
        let person = workspaces.device_cert().user_public_key;
        // The root kept here is the one of the person asked, and no other's.
        if tilo_actions::remote::asked::asked_person(caller) != Some(hex::encode(person)) {
            return Err(refused(
                "it is answered on a device that keeps the root of the person it asks",
            ));
        }
        // Taken up again after this device started again, its approve is not carried: what it
        // founded before is answered, and nothing is founded without the yes in hand.
        let approval = request.approval();
        // Picked among the choices the person was shown with the question, and only then.
        let let_work = approval.is_some_and(|approval| {
            approval.chosen.contains(&LET_WORK)
                && self
                    .store
                    .kept_ask(&approval.record)
                    .is_some_and(|ask| ask.facts.many && ask.facts.choices.len() > LET_WORK)
        });
        let approved = approval.is_some();
        let agent = if let_work {
            Some(agent_of(caller, context)?)
        } else {
            None
        };
        let invocation = request.invocation;
        let kept = Arc::clone(&self.kept);
        Ok(Box::pin(async move {
            // The workspace's id is the invocation's: a make run again after this device started
            // again finds the one it founded, never founds a second.
            let workspace = made_id(invocation);
            let summary = match workspaces.summary(&workspace).await {
                Ok(summary) => summary,
                Err(_) if !approved => {
                    return failed(
                        invocation,
                        "It was answered before this device started again, and nothing was made: \
                         ask again",
                    );
                }
                Err(_) => match workspaces.found(&name, Some(&workspace)).await {
                    Ok(summary) => summary,
                    Err(why) => return failed(invocation, &why),
                },
            };
            let mut made = serde_json::Map::new();
            made.insert("workspace".into(), summary.workspace.clone().into());
            made.insert("name".into(), summary.name.clone().into());
            if let Some((key, agent, label)) = agent {
                match kept
                    .certify_agent(person, key, &summary.workspace, &agent, &label)
                    .await
                {
                    Ok(Some(grant)) => match serde_json::to_value(grant) {
                        Ok(grant) => {
                            made.insert("grant".into(), grant);
                        }
                        Err(why) => {
                            return failed(
                                invocation,
                                &format!(
                                    "the workspace was made, and the agent's grant was lost: {why}"
                                ),
                            );
                        }
                    },
                    Ok(None) => {}
                    Err(why) => {
                        return failed(
                            invocation,
                            &format!("the workspace was made, and the agent was not let in: {why}"),
                        );
                    }
                }
            }
            InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(Value::Object(made)),
                    resources: Vec::new(),
                },
            }
        }))
    }
}

/// The agent key that asked, as its certificate kept beside the call names it: its key, the
/// agent it is and the name its person gave it.
fn agent_of(
    caller: &Caller,
    context: &Value,
) -> Result<([u8; 32], String, String), ActionCallError> {
    let MakeContext { certificate } = serde_json::from_value(context.clone())
        .map_err(|_| refused("the key that asked is not kept with the question"))?;
    let key = *caller
        .device_public_key()
        .ok_or_else(|| refused("the call names no key that asked"))?;
    if certificate.device_public_key != key
        || Some(&certificate.user_public_key) != caller.user_public_key()
    {
        return Err(refused(
            "the key kept with the question is not the one that asked",
        ));
    }
    let agent = caller
        .acting()
        .agent_id()
        .map_or_else(|| tilo_action_policy::key_agent_id(&key), str::to_owned);
    Ok((key, agent, certificate.device_label))
}

/// The id of the workspace the make `invocation` founds.
fn made_id(invocation: InvocationId) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, invocation.0.as_bytes()).to_string()
}

fn refused(reason: &str) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.to_owned(),
    }
}

/// How `invocation` ended when it failed with `message`.
fn failed(invocation: InvocationId, message: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "refused".into(),
            message: message.to_owned(),
            details: None,
            retryable: false,
        },
    }
}

impl AskedDispatch for AtRoot {
    fn context(&self, _: InvocationId) -> Value {
        Value::Null
    }

    /// The person answered it here, once, whatever the action's own confirmation says: it runs
    /// under the root kept here, and how it ended is written into its record for whoever asked.
    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        context: &Value,
    ) -> Result<(), ActionCallError> {
        let run = if let Some(name) = asked_make(&request) {
            self.make(caller, &request, name, context)?
        } else if self.kept.runs(&request) {
            self.kept.run(request.clone(), caller.clone())
        } else {
            return Err(refused(
                "only what the person's root signs is answered at it",
            ));
        };
        // Taken before it runs: a second approve finds it answered, and after a restart this
        // device runs it again, never drops it.
        self.store.take(request.invocation);
        let (store, invocation) = (Arc::clone(&self.store), request.invocation);
        self.runtime.spawn(async move {
            let outcome = run.await;
            store.settle_for(invocation, outcome);
        });
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.store.settle_for(
            invocation,
            InvocationUpdate::Cancelled {
                invocation,
                message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
            },
        );
        Ok(())
    }

    fn withdrawn(&self, _: &Caller, invocation: InvocationId, message: &str) {
        self.store.settle_for(
            invocation,
            InvocationUpdate::Cancelled {
                invocation,
                message: Some(message.to_owned()),
            },
        );
    }
}
