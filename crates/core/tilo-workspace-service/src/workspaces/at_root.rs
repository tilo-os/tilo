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
//! The service runs one such call itself: what an agent whose grant reaches one workspace asks
//! a host's `workspace.create-own` for beyond it (a new workspace, under a name or labels, or
//! one of the person's to enter; [`kept_for_root`]). Every host keeps it so, the person's own
//! device that founds their workspaces among them, and the question names where the workspace
//! will live: the host that kept it. On the person's yes it is founded here, as the person and
//! with them as its owner, and placed at that host, only where the question named it. The agent
//! is let work in it only where the person picked that with their approve ([`LET_WORK`]). Every
//! other call is the host's own ([`RootKept`]): the daemon runs the device's root-only actions
//! through it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use serde_json::Value;
use tilo_actions::remote::asked::{AT_ROOT, AskedDispatch, AskedFacts, Fact, KeptAsk};
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_remote_actions::asked::AskedStore;
use tilo_server_protocol::workspaces::{
    CREATE_OWN_WORKSPACE, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    WorkspaceLabels, WorkspaceSummary,
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

/// What a host's `workspace.create-own` asks for, as its arguments name it.
#[derive(Debug, Default)]
struct Asked {
    /// The name of a new workspace.
    name: Option<String>,
    /// The workspace it names: one of the person's to enter, or the identity a new one takes.
    workspace: Option<String>,
    /// The labels a new workspace is kept under for the person's programs.
    labels: Option<WorkspaceLabels>,
}

impl Asked {
    /// What `request` asks, where it is a host's `workspace.create-own`.
    fn of(request: &InvokeRequest) -> Option<Self> {
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
        let text = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let labels = match arguments.get("labels") {
            None | Some(Value::Null) => None,
            Some(labels) => Some(serde_json::from_value(labels.clone()).ok()?),
        };
        Some(Self {
            name: text("name"),
            workspace: text("workspace"),
            labels,
        })
    }

    /// Whether it enters a workspace already made rather than making one.
    const fn enters(&self) -> bool {
        self.name.is_none() && self.workspace.is_some()
    }
}

/// Whether `request`, asked under `certificate`, waits for a device that keeps its person's
/// root rather than for this host: a key whose grant reaches one workspace already made, asking
/// for more than that (another workspace, a name, labels). No host makes that for the key; the
/// person's own device does, once they say yes there.
#[must_use]
pub fn kept_for_root(certificate: Option<&DeviceCert>, request: &InvokeRequest) -> bool {
    let Some(certificate) = certificate.filter(|certificate| held_to_its_workspace(certificate))
    else {
        return false;
    };
    Asked::of(request).is_some_and(|asked| {
        Workspaces::a_grant_asks_for_more(
            certificate,
            (asked.name.as_deref(), asked.workspace.as_deref()),
            asked.labels.as_ref(),
        )
    })
}

/// Whether `certificate` is an agent's grant that reaches its one workspace and makes none.
fn held_to_its_workspace(certificate: &DeviceCert) -> bool {
    certificate
        .capabilities
        .agent
        .as_ref()
        .is_some_and(|scope| !scope.makes_workspaces())
}

/// Where a make kept for the person's root is placed: the host that kept it, as the person is
/// shown it with the question, so they know where the workspace will live before they say yes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Place {
    /// The host's device key, in hex.
    device: String,
    /// The host, by the name its operator gave its device.
    site: String,
    /// Who runs it, as people know the host (`tilo.sh`), where it keeps people's workspaces.
    operator: Option<String>,
    /// Where it is reached.
    address: iroh_base::EndpointAddr,
}

impl Place {
    /// What the person reads of the place before they answer.
    fn facts(&self) -> [Fact; 2] {
        let site = match &self.operator {
            Some(operator) => format!("{}, run by {operator}", self.site),
            None => self.site.clone(),
        };
        let mut address = self.device.clone();
        let reached: Vec<String> = self.address.addrs.iter().map(ToString::to_string).collect();
        if !reached.is_empty() {
            address = format!("{address} ({})", reached.join(", "));
        }
        [Fact::new("Kept at", site), Fact::new("Address", address)]
    }

    /// The device it names, where it reads as one.
    fn device(&self) -> Option<[u8; 32]> {
        hex::decode(&self.device).ok()?.try_into().ok()
    }
}

/// `facts` as the person is asked a make kept for their root, asked under `certificate` and
/// placed at `place`: the agent named as its person named its key, where the workspace will
/// live, and, for a new workspace, the choice to let the agent work in it, which they may leave
/// unpicked. Entering a workspace already made is the yes itself.
fn asked_at_root(
    mut facts: AskedFacts,
    certificate: Option<&DeviceCert>,
    request: &InvokeRequest,
    place: &Place,
) -> AskedFacts {
    let label = certificate.map(|certificate| certificate.device_label.clone());
    let agent = match (&mut facts.by, label) {
        (tilo_actions::remote::asked::AskedBy::Agent(named), Some(label)) => {
            named.name.clone_from(&label);
            label
        }
        (tilo_actions::remote::asked::AskedBy::Agent(named), None) => named.name.clone(),
        (_, label) => label.unwrap_or_else(|| "the agent".to_owned()),
    };
    facts.facts.extend(place.facts());
    if !Asked::of(request).is_some_and(|asked| asked.enters()) {
        facts.choices = vec![let_work(&agent)];
        facts.many = true;
    }
    facts
}

/// What a host keeps beside a make it keeps for the person's root: the certificate the agent's
/// key asked under, which names the key and what the person called it, and where the workspace
/// is placed, as the question names it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MakeContext {
    certificate: DeviceCert,
    #[serde(default)]
    place: Option<Place>,
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
    /// This host, as a make kept here for the person's root is placed at it.
    fn place_here(&self) -> Place {
        let cert = self.device_cert();
        Place {
            device: hex::encode(cert.device_public_key),
            site: cert.device_label.clone(),
            operator: self.own().map(|own| own.offer().host.clone()),
            address: self.inner.host.endpoint.addr(),
        }
    }

    /// Keep `request`, which `caller` asked under `certificate` and only a device that keeps
    /// the person's root runs, as its asked record for that device, showing `facts` and where
    /// the workspace will live: in the workspace the call acts in, else in the one the key's
    /// grant reaches, else in the person's own workspace here. `None` where no store here keeps
    /// asked records.
    pub(crate) async fn keep_for_root(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        (caller, certificate): (&Caller, Option<&DeviceCert>),
        request: &InvokeRequest,
        facts: AskedFacts,
    ) -> Option<Result<(tilo_actions::remote::asked::RecordRef, AskedFacts), String>> {
        let store = self.asked_store()?;
        let reached = certificate
            .and_then(|certificate| certificate.capabilities.agent.as_ref())
            .and_then(tilo_user_identity::AgentScope::workspace)
            .map(|id| uuid::Uuid::from_bytes(id).to_string());
        let workspace = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => match reached {
                Some(reached) if self.state(&reached).await.is_some() => Some(reached),
                _ => caller
                    .user_public_key()
                    .and_then(|person| self.own_workspace_of(person)),
            },
        };
        let Some(workspace) = workspace else {
            return Some(Err("no workspace here keeps it for its person".to_owned()));
        };
        let place = self.place_here();
        let facts = asked_at_root(facts, certificate, request, &place);
        let context = certificate
            .map(|certificate| MakeContext {
                certificate: certificate.clone(),
                place: Some(place),
            })
            .and_then(|context| serde_json::to_value(context).ok())
            .unwrap_or(Value::Null);
        let beside: Arc<dyn AskedDispatch> = Arc::new(KeptBeside(context));
        let records = store.writes_for(AT_ROOT, Arc::downgrade(&beside));
        Some(
            records
                .write_asked(&workspace, caller, request, &facts)
                .map(|record| (record, facts)),
        )
    }

    /// `ended`, how the device that keeps `person`'s root ended a make this host kept for it: one
    /// that made or entered the workspace has it kept here as the person's own, labelled as
    /// `request` asked, once it is placed here.
    pub(crate) async fn took_in(
        &self,
        person: [u8; 32],
        request: &InvokeRequest,
        ended: InvocationUpdate,
    ) -> InvocationUpdate {
        let InvocationUpdate::Succeeded { invocation, output } = &ended else {
            return ended;
        };
        let made = match serde_json::from_value::<CreatedWorkspace>(
            output.value.clone().unwrap_or_default(),
        ) {
            Ok(made) => made,
            Err(error) => {
                return failed(
                    *invocation,
                    &format!("the device that keeps your identity answered no workspace: {error}"),
                );
            }
        };
        let workspace = made.workspace.workspace;
        if self.state(&workspace).await.is_none() {
            return failed(
                *invocation,
                &format!("the workspace {workspace} was made, and not placed on this host"),
            );
        }
        if let Some(own) = self.own()
            && let Err(error) = own.record(&person, &workspace)
        {
            return failed(*invocation, &error);
        }
        if let Some(labels) = Asked::of(request).and_then(|asked| asked.labels)
            && let Err(error) = self.label(&workspace, &labels).await
        {
            return failed(*invocation, &error.to_string());
        }
        ended
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

    /// What `request` asks, as the person: a new workspace founded once, here, with them as its
    /// owner and placed at the host the question named, or one of theirs entered; the agent
    /// that asked let work in it where they picked that with their approve, or where it asked
    /// to enter.
    #[expect(
        clippy::too_many_lines,
        reason = "one make read top to bottom: whose root, where it is placed, who is let in, and the founding"
    )]
    fn make(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        asked: Asked,
        context: &Value,
    ) -> Result<RootRun, ActionCallError> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or_else(|| refused("this device keeps no workspaces"))?;
        let here = workspaces.device_cert();
        let (person, this_device) = (here.user_public_key, here.device_public_key);
        // The root kept here is the one of the person asked, and no other's.
        if tilo_actions::remote::asked::asked_person(caller) != Some(hex::encode(person)) {
            return Err(refused(
                "it is answered on a device that keeps the root of the person it asks",
            ));
        }
        let (certificate, agent) = agent_of(caller, context)?;
        signed_by_the_agent(request, &certificate)?;
        // Taken up again after this device started again, its approve is not carried: what it
        // founded before is answered, and nothing is founded without the yes in hand.
        let approval = request.approval();
        let answered = approval.and_then(|approval| self.store.kept_ask(&approval.record));
        let MakeContext { place, .. } = serde_json::from_value(context.clone())
            .map_err(|_| refused("the key that asked is not kept with the question"))?;
        let place = place.ok_or_else(|| {
            refused("the question named nowhere for the workspace to live: ask again")
        })?;
        // Placed only where the question the person answered named it.
        if let Some(answered) = &answered
            && !place
                .facts()
                .iter()
                .all(|fact| answered.facts.facts.contains(fact))
        {
            return Err(refused(
                "the workspace is placed only where the question named, and this is another \
                 place: nothing was made",
            ));
        }
        let at = place
            .device()
            .ok_or_else(|| refused("the question names no device to place it at"))?;
        let placed_at = (at != this_device).then(|| (at, place.address.clone()));
        // Picked among the choices the person was shown with the question, and only then.
        let let_in = asked.enters()
            || approval.is_some_and(|approval| {
                approval.chosen.contains(&LET_WORK)
                    && answered
                        .as_ref()
                        .is_some_and(|ask| ask.facts.many && ask.facts.choices.len() > LET_WORK)
            });
        let approved = approval.is_some();
        let invocation = request.invocation;
        let kept = Arc::clone(&self.kept);
        Ok(Box::pin(async move {
            // A new workspace's id is the invocation's: a make run again after this device
            // started again finds the one it founded, never founds a second.
            let workspace = asked
                .workspace
                .clone()
                .unwrap_or_else(|| made_id(invocation));
            let (summary, placed) = match workspaces.summary(&workspace).await {
                Ok(summary) if asked.workspace.is_none() || is_member(&summary, person) => {
                    (summary, None)
                }
                Ok(_) => {
                    return failed(
                        invocation,
                        &format!("{workspace} is not yours, so nothing was made"),
                    );
                }
                Err(_) if asked.enters() => {
                    return failed(
                        invocation,
                        &format!(
                            "This device keeps no {workspace}, so it can't let the agent in there"
                        ),
                    );
                }
                Err(_) if !approved => {
                    return failed(
                        invocation,
                        "It was answered before this device started again, and nothing was made: \
                         ask again",
                    );
                }
                Err(_) => {
                    let name = asked
                        .name
                        .clone()
                        .unwrap_or_else(|| crate::i18n::workspace_name(None));
                    let founded = match placed_at {
                        Some(at) => workspaces.found_at(&name, Some(&workspace), Some(at)).await,
                        None => workspaces
                            .found(&name, Some(&workspace))
                            .await
                            .map(|summary| (summary, None)),
                    };
                    match founded {
                        Ok(founded) => founded,
                        Err(why) => return failed(invocation, &why),
                    }
                }
            };
            let mut grant = None;
            if let_in {
                let (key, agent, label) = agent;
                match kept
                    .certify_agent(person, key, &summary.workspace, &agent, &label)
                    .await
                {
                    Ok(certified) => grant = certified,
                    Err(why) => {
                        return failed(
                            invocation,
                            &format!("the workspace was made, and the agent was not let in: {why}"),
                        );
                    }
                }
            }
            let entry = let_in.then_some((&certificate, placed));
            let mut made = match workspaces.made_at_root(summary, entry).await {
                Ok(Value::Object(made)) => made,
                Ok(other) => {
                    return failed(invocation, &format!("the workspace answered {other}"));
                }
                Err(error) => return failed(invocation, &error.to_string()),
            };
            if let Some(grant) = grant {
                match serde_json::to_value(grant) {
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

/// Whether `person` is a member of the workspace `summary` describes, not removed.
fn is_member(summary: &WorkspaceSummary, person: [u8; 32]) -> bool {
    summary
        .members
        .iter()
        .any(|member| member.root() == Some(person) && !member.removed)
}

/// Whether `request` is the call the agent whose certificate is `certificate` signed itself, as
/// it asked it, whoever carried it here: a site that kept it carries the ask and never makes one
/// up. Checked as of when the agent signed it, since the person answers it later.
fn signed_by_the_agent(
    request: &InvokeRequest,
    certificate: &DeviceCert,
) -> Result<(), ActionCallError> {
    let signed = request.signed.as_ref().ok_or_else(|| {
        refused("the ask carries no signature of the agent that asked: nothing was made")
    })?;
    let agent = tilo_server_protocol::invocation::verify_host_invocation(
        request,
        &tilo_user_identity::RevocationList::new(),
        signed.statement().issued_at_ns,
    )
    .map_err(|error| {
        refused(&format!(
            "the ask is not the call the agent signed ({error}): nothing was made"
        ))
    })?;
    if agent.device_public_key != certificate.device_public_key {
        return Err(refused(
            "the ask was signed by another key than the agent's: nothing was made",
        ));
    }
    Ok(())
}

/// An agent by its key, the agent it is and the name its person gave it.
type AgentNamed = ([u8; 32], String, String);

/// The agent key that asked, as its certificate kept beside the call names it: the certificate,
/// with its key, the agent it is and the name its person gave it.
fn agent_of(caller: &Caller, context: &Value) -> Result<(DeviceCert, AgentNamed), ActionCallError> {
    let MakeContext { certificate, .. } = serde_json::from_value(context.clone())
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
        .map_or_else(|| tilo_member_types::key_agent_id(&key), str::to_owned);
    let label = certificate.device_label.clone();
    Ok((certificate, (key, agent, label)))
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
        let run = if let Some(asked) = Asked::of(&request) {
            self.make(caller, &request, asked, context)?
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
