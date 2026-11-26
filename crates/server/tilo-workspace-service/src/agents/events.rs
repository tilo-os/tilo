//! A workspace's events on this server: kept, run, and answered as typed
//! actions ([`tilo_actions::remote::workspace_events`]).
//!
//! An event is a Shortcut the server runs with no device of anyone's on
//! ([`tilo_agent_session::bot::event`]). It is run by the engine that runs a
//! workspace's bots ([`Runner`]) for the member who set
//! it up, and is no member of the workspace: its program's steps are the
//! server's own (a judgement, a count, an instruction handed to that
//! member's assistant), so it changes no record and nothing has to admit it.
//! What it starts is the assistant's own work, under the assistant's own
//! bounds.
//!
//! The events are the workspace's, kept in its store beside its mail and its
//! models ([`Workspaces::workspace_root`]): every member whose role reaches
//! them sees and changes the workspace's events, with authorship recorded
//! in [`EventView::by`]. A personal assistant's look-round is visible only
//! to the person it runs for and their authorized agent. A server that starts again
//! runs every event it kept, and a moment that passed while it was down
//! fires once.
//!
//! The actions are answered as the workspace's records are: for a member on
//! a device of theirs, and their assistant on a session they handed it, in a
//! workspace whose policy lets them, and for a turn this server carries out
//! itself on a member's behalf. Whoever else asks, another workspace's
//! member included, is refused.
//!
//! An event says how its result is told ([`Definition::tells_as`]), and the
//! turn it starts carries that as its cause. Whether a thing then reaches a
//! person, and through their do-not-disturb, stays that person's own rule's
//! to say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::contract::{
    ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
    RiskLevel,
};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::person_telling::Kind;
use tilo_actions::remote::workspace_events::{self, Change, EventView, Events, Named, SetUp, Sort};
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionMessage, pending,
};
use tilo_actions::{ActionOutput, ExecutionLocus, MemberId, OperationKind};
use tilo_agent_session::bot::event::{Definition, told_as_of};
use tilo_agent_session::bot::host_steps::{HostStepError, HostSteps, Keeps as _};
use tilo_agent_session::bot::kept::{self, NoteChange};
use tilo_automation::TriggerSpec;
use tilo_automation::schedule::Timed;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::kept::KeptFile;
use crate::workspaces::{Store, Workspaces};

/// The most events one workspace keeps.
pub const MAX_EVENTS: usize = 200;
/// The file an event's description is kept in.
const DESCRIPTION: &str = "event.json";
/// The file an event's runner keeps what it took in.
const PROGRESS: &str = "progress.json";
/// The folder of the workspace's store its events are kept in.
const FOLDER: &str = "events";

/// What answers the steps of one event.
type StepsFor = Arc<dyn Fn(&Definition) -> Arc<dyn HostSteps> + Send + Sync>;

/// What runs an event's program: the engine that runs a workspace's bots.
pub trait Runner: Send + Sync {
    /// Run `definition`'s program in `workspace` of `workspaces` for `person` (hex root), the
    /// steps that ask this server answered by `steps`, where its runner had read up to kept in
    /// `progress`: what the program keeps between its runs, and the task it runs on.
    ///
    /// # Errors
    /// The program does not read, or what it kept does not.
    fn run(
        &self,
        workspaces: &Workspaces,
        workspace: &str,
        definition: &Definition,
        person: String,
        steps: Arc<dyn HostSteps>,
        progress: PathBuf,
    ) -> Result<(Arc<KeptFile>, tokio::task::JoinHandle<()>), String>;
}

/// Whether `message` is for a workspace's events.
#[must_use]
pub fn addresses(message: &RemoteActionMessage) -> bool {
    match message {
        RemoteActionMessage::CatalogueRequest { query, .. } => {
            query.provider.as_deref() == Some(workspace_events::PROVIDER)
        }
        RemoteActionMessage::Invoke(request) => {
            request.target.provider == workspace_events::PROVIDER
        }
        _ => false,
    }
}

/// What a server that runs no events answers `message`: that it offers no
/// such action, as for anything it does not know.
#[must_use]
pub fn not_served(message: RemoteActionMessage) -> Option<Pending<RemoteActionMessage>> {
    match message {
        RemoteActionMessage::CatalogueRequest { correlation, .. } => {
            Some(Pending::ready(RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Err(ActionCallError::UnknownAction),
            }))
        }
        RemoteActionMessage::Invoke(request) => Some(Pending::ready(RemoteActionMessage::Receipt(
            InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::UnknownAction,
                },
            },
        ))),
        _ => None,
    }
}

/// Who asks of a workspace's events: a caller on a device the handshake
/// verified, or a turn this server carries out with no device behind it.
#[derive(Clone, Debug)]
pub enum Authority {
    /// A caller on `certificate`'s device. What an agent on a person's
    /// session changes asks its person first, unless they answered that
    /// action in advance.
    Device(Box<DeviceCert>),
    /// A turn this server carries out for a member, with no device behind
    /// it: the member acting as themselves.
    Turn,
}

impl Authority {
    /// The authority of a caller on the device `certificate` names.
    #[must_use]
    pub fn of(certificate: &DeviceCert) -> Self {
        Self::Device(Box::new(certificate.clone()))
    }
}

/// Who asks for a workspace's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Asker {
    /// The person themselves, on a device of theirs.
    Person,
    /// An agent on a session the person handed it.
    Assistant,
}

/// One event running here.
struct Running {
    stored: Stored,
    kept: Arc<KeptFile>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One event as its store keeps it: what it is, who set it up, and the person
/// whose assistant its instruction starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    definition: Definition,
    /// Who set it up, as authorship: its changes are the workspace's.
    by: MemberId,
    /// The member whose assistant its instruction starts, by their root, hex:
    /// the one who set it up.
    person: String,
    /// The person's look-round. Earlier event files did not record their scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    personal: Option<bool>,
    /// The accepted proposal that installed this shared event, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal: Option<String>,
}

impl Stored {
    fn visible_to(&self, person: &[u8; 32]) -> bool {
        self.personal != Some(true) || self.person.eq_ignore_ascii_case(&hex::encode(person))
    }
}

/// Who the policy answered for, and where.
pub(super) struct Theirs {
    /// The workspace as its store names it.
    pub workspace: String,
    /// The person behind the caller: the person themselves, or the one whose
    /// agent asks.
    pub person: [u8; 32],
    /// Whether the caller is that person or an agent on their session.
    pub asker: Asker,
    /// Who makes a change now, as authorship keeps it.
    pub by: MemberId,
}

/// A native instruction whose retries must all name the same event.
pub(super) enum Installation<'a> {
    LookRound(&'a str),
    Proposal(&'a str),
}

struct Inner {
    workspaces: Workspaces,
    /// What the server answers an event's steps with.
    steps: StepsFor,
    /// What runs each event's program.
    runner: Arc<dyn Runner>,
    /// The events running in each workspace, by their id.
    running: Mutex<BTreeMap<String, BTreeMap<String, Running>>>,
}

/// The events of this server's workspaces: every one it keeps runs for as
/// long as this, or a clone of it, is held.
#[derive(Clone)]
pub struct WorkspaceEvents {
    inner: Arc<Inner>,
}

impl WorkspaceEvents {
    /// Run every event kept in its workspace's store, and keep new ones
    /// there. Their steps are answered by this server
    /// ([`super::assistant`]): with `actions`, the server's action service,
    /// an instruction an event hands over starts a turn of its author's
    /// assistant. `runner` runs each event's program. Called on the runtime the events work on.
    #[must_use]
    pub fn start(
        workspaces: Workspaces,
        actions: Option<tilo_agent_session::workspace_actions::WorkspaceActions>,
        runner: Arc<dyn Runner>,
    ) -> Self {
        Self::start_behind(workspaces, actions, None, runner)
    }

    /// [`Self::start`], with `before` put in front of what answers each
    /// event's steps: it is handed what would answer and answers in its
    /// place, so a step can be looked at before it is taken.
    #[must_use]
    pub fn start_behind(
        workspaces: Workspaces,
        actions: Option<tilo_agent_session::workspace_actions::WorkspaceActions>,
        before: Option<super::initiative::Before>,
        runner: Arc<dyn Runner>,
    ) -> Self {
        use super::assistant::{self, Starter};
        let plain = || -> StepsFor {
            let steps = assistant::host_steps(workspaces.clone());
            Arc::new(move |_: &Definition| Arc::clone(&steps))
        };
        let starting_turns =
            |actions: tilo_agent_session::workspace_actions::WorkspaceActions| -> StepsFor {
                let workspaces = workspaces.clone();
                Arc::new(move |event: &Definition| {
                    let kind = match event.tells_as() {
                        Kind::Reminder => assistant::REMINDER,
                        Kind::Brief => assistant::BRIEF,
                        Kind::Question | Kind::News | Kind::Mention => assistant::EVENT,
                    };
                    assistant::steps_starting_turns(
                        workspaces.clone(),
                        actions.clone(),
                        Starter {
                            name: event.name.clone(),
                            kind,
                        },
                    )
                })
            };
        let steps = actions.map_or_else(plain, starting_turns);
        let steps: StepsFor = match before {
            Some(before) => Arc::new(move |event: &Definition| before(steps(event))),
            None => steps,
        };
        Self::started(workspaces, steps, runner)
    }

    /// [`Self::start`], with what answers the events' steps given.
    #[must_use]
    pub fn start_with(
        workspaces: Workspaces,
        steps: Arc<dyn HostSteps>,
        runner: Arc<dyn Runner>,
    ) -> Self {
        Self::started(
            workspaces,
            Arc::new(move |_: &Definition| Arc::clone(&steps)),
            runner,
        )
    }

    fn started(workspaces: Workspaces, steps: StepsFor, runner: Arc<dyn Runner>) -> Self {
        let events = Self {
            inner: Arc::new(Inner {
                workspaces,
                steps,
                runner,
                running: Mutex::default(),
            }),
        };
        events.resume_kept();
        events
    }

    /// Start every event a server before this one kept. One that does not
    /// start is said and left where it is, so a fixed server runs it again.
    fn resume_kept(&self) {
        let mut kept = Vec::new();
        for (hexed, at) in directories(&self.inner.workspaces.workspace_stores_root()) {
            let Some(workspace) = workspace_of(&hexed) else {
                continue;
            };
            let old_look = old_look_round(&at);
            if let Err(error) = &old_look {
                tracing::warn!(%workspace, %error, "events of unknown scope are private until the look-round can be read");
            }
            for (id, dir) in directories(&at.join(FOLDER)) {
                let started = read_stored(&dir).and_then(|mut stored| {
                    if stored.definition.id != id {
                        return Err("the event's id differs from its directory".into());
                    }
                    if stored.personal.is_none() {
                        match &old_look {
                            Ok(Some(event)) if *event == id => {
                                stored.personal = Some(true);
                                write_stored(&dir, &stored)?;
                            }
                            Err(_) => stored.personal = Some(true),
                            _ => {}
                        }
                    }
                    if stored.proposal.is_some() && removed(&dir, &stored.person)? {
                        return Ok(None);
                    }
                    self.run(&workspace, stored, &dir).map(Some)
                });
                match started {
                    Ok(Some(event)) => kept.push((workspace.clone(), id, event)),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%workspace, event = %id, %error, "a kept event does not run");
                    }
                }
            }
        }
        let mut running = self.inner.running.lock_recover();
        for (workspace, id, event) in kept {
            running.entry(workspace).or_default().insert(id, event);
        }
    }

    fn dir(&self, workspace: &str, id: &str) -> PathBuf {
        self.inner
            .workspaces
            .workspace_root(workspace)
            .join(FOLDER)
            .join(id)
    }

    /// Keep `stored` in its directory and run it.
    fn begin(&self, workspace: &str, stored: Stored) -> Result<Running, String> {
        let dir = self.dir(workspace, &stored.definition.id);
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        write_stored(&dir, &stored)?;
        self.run(workspace, stored, &dir)
    }

    /// Run `stored` for its author's person from what `dir` keeps: where its
    /// runner had read up to, its counts and its note.
    fn run(&self, workspace: &str, stored: Stored, dir: &Path) -> Result<Running, String> {
        let (kept, task) = self.inner.runner.run(
            &self.inner.workspaces,
            workspace,
            &stored.definition,
            stored.person.clone(),
            (self.inner.steps)(&stored.definition),
            dir.join(PROGRESS),
        )?;
        Ok(Running { stored, kept, task })
    }

    /// Whether this host runs `event` for `person` in `workspace`. The teller
    /// recognizes a reminder by the original author of its scheduling event.
    #[must_use]
    pub fn set_up_by(&self, workspace: &str, event: &str, person: &[u8; 32]) -> bool {
        self.inner
            .running
            .lock_recover()
            .get(workspace)
            .and_then(|events| events.get(event))
            .is_some_and(|event| {
                event
                    .stored
                    .person
                    .eq_ignore_ascii_case(&hex::encode(person))
            })
    }

    /// Answer `message` for `workspace_id`: the actions' rows, or one
    /// action. `None` for a message that needs no reply.
    #[must_use]
    pub fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: Authority,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let page = match runtime() {
                    Some(runtime) => {
                        let (completer, answer) = pending();
                        let (this, caller) = (self.clone(), caller.clone());
                        runtime.spawn(async move {
                            let page = this
                                .catalogue(workspace_id, &caller, &authority, &query)
                                .await;
                            completer.complete(RemoteActionMessage::CatalogueResponse {
                                correlation,
                                result: Ok(page),
                            });
                        });
                        answer
                    }
                    None => Pending::ready(RemoteActionMessage::CatalogueResponse {
                        correlation,
                        result: Err(no_runtime()),
                    }),
                };
                Some(page)
            }
            RemoteActionMessage::Invoke(request) => Some(
                self.invoking(workspace_id, caller, authority, request)
                    .map(RemoteActionMessage::Receipt),
            ),
            _ => None,
        }
    }

    /// Take one action for `caller`, answered when the workspace's policy has.
    #[must_use]
    pub fn invoking(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: Authority,
        request: InvokeRequest,
    ) -> Pending<InvocationReceipt> {
        let invocation = request.invocation;
        let Some(runtime) = runtime() else {
            return Pending::ready(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: no_runtime(),
                },
            });
        };
        let (completer, answer) = pending();
        let (this, caller) = (self.clone(), caller.clone());
        runtime.spawn(async move {
            let receipt = this
                .invoke(workspace_id, &caller, &authority, &request)
                .await;
            completer.complete(receipt);
        });
        answer
    }

    /// The actions `caller` is offered on `workspace_id`'s events: none for
    /// a caller the workspace's policy does not let reach them.
    #[must_use]
    pub async fn catalogue(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
        query: &CatalogueQuery,
    ) -> CataloguePage {
        let mut page = match self
            .reach(
                workspace_id,
                caller,
                authority,
                Store::EVENTS,
                workspace_events::LIST,
                ActionKind::Read,
            )
            .await
        {
            Ok(_) => CataloguePage::from_rows(String::from("0"), rows(), query),
            Err(_) => CataloguePage::from_rows(String::from("0"), Vec::new(), query),
        };
        if query.provider.is_none() {
            page.providers = self
                .provider(workspace_id, caller, authority)
                .await
                .into_iter()
                .collect();
        }
        page
    }

    /// The events' provider as a catalogue lists it for `caller`, when the
    /// workspace's policy lets them reach its events.
    #[must_use]
    pub async fn provider(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
    ) -> Option<CatalogueProvider> {
        self.reach(
            workspace_id,
            caller,
            authority,
            Store::EVENTS,
            workspace_events::LIST,
            ActionKind::Read,
        )
        .await
        .ok()
        .map(|_| CatalogueProvider {
            provider: workspace_events::PROVIDER.to_owned(),
            app_id: workspace_events::PROVIDER.to_owned(),
            workspace: workspace_id.to_string(),
            document_id: String::new(),
            browsable: true,
        })
    }

    /// Who `caller` asks for in `store`, as the workspace's policy decides
    /// it: the member themselves on a device the handshake verified (an
    /// agent's change first put to their person where the policy asks), or a
    /// turn this server carries out for them. A bot, an app, and an agent no
    /// person's session stands behind are told what a caller asking for
    /// something that does not exist is told.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a caller the workspace does not
    /// have or a workspace not governed here;
    /// [`ActionCallError::NotPermitted`] for one whose role or grants do not
    /// reach.
    pub(super) async fn reach(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Theirs, ActionCallError> {
        let asker = asker_of(caller).ok_or(ActionCallError::UnknownAction)?;
        if caller
            .workspace()
            .is_some_and(|confined| WorkspaceId::parse(confined) != Some(workspace_id))
        {
            return Err(ActionCallError::UnknownAction);
        }
        let workspaces = &self.inner.workspaces;
        match authority {
            Authority::Device(certificate) => {
                let reach = workspaces
                    .reaching_allowed(
                        (Some(workspace_id), caller, Some(certificate.as_ref())),
                        store,
                        action,
                        effect,
                    )
                    .await?;
                Ok(Theirs {
                    workspace: reach.workspace.to_string(),
                    person: reach.person,
                    asker,
                    by: member_of(caller, &reach.person),
                })
            }
            Authority::Turn => {
                if caller.roster_created_at().is_some()
                    && !workspaces
                        .agent_may_act(&workspace_id.to_string(), caller, store.app)
                        .await
                {
                    return Err(ActionCallError::NotPermitted);
                }
                // A turn has no device behind it: it is the agent working for
                // the person who asked, or the member acting as themselves.
                let person = caller
                    .turn_person()
                    .or_else(|| caller.user_public_key())
                    .copied()
                    .ok_or(ActionCallError::UnknownAction)?;
                if !workspaces
                    .person_reaches(workspace_id, &person, store, action, effect)
                    .await
                {
                    return Err(ActionCallError::NotPermitted);
                }
                Ok(Theirs {
                    workspace: workspace_id.to_string(),
                    person,
                    asker,
                    by: member_of(caller, &person),
                })
            }
        }
    }

    /// Take one action for `caller`, answered at once.
    #[must_use]
    pub async fn invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
        request: &InvokeRequest,
    ) -> InvocationReceipt {
        let status = match self.answer(workspace_id, caller, authority, request).await {
            Ok(value) => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            Err(error) => ReceiptStatus::Rejected { error },
        };
        InvocationReceipt {
            invocation: request.invocation,
            status,
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "an action is taken whole: a workspace's events stay locked until it is answered"
    )]
    async fn answer(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        let theirs = self
            .reach(
                workspace_id,
                caller,
                authority,
                Store::EVENTS,
                action,
                effect_of(action),
            )
            .await?;
        let mut running = self.inner.running.lock_recover();
        let kept = running.entry(theirs.workspace.clone()).or_default();
        match action {
            workspace_events::LIST => said(&listed(kept, &theirs.person)),
            workspace_events::SET_UP => {
                let wanted: SetUp = argument(
                    request,
                    "an event to set up names itself, what starts it and what to do then",
                )?;
                said(&self.set_up(
                    &theirs,
                    kept,
                    wanted,
                    uuid::Uuid::new_v4().to_string(),
                    None,
                )?)
            }
            workspace_events::CHANGE => {
                let changes: Change = argument(request, "a change names the event it changes")?;
                said(&self.change(&theirs, kept, changes)?)
            }
            action @ (workspace_events::PAUSE | workspace_events::RESUME) => {
                let named: Named = argument(request, "an event is named by its id")?;
                let event = visible_event(kept, &named.event, &theirs.person)?;
                event
                    .kept
                    .pause(action == workspace_events::PAUSE)
                    .map_err(step_error)?;
                said(&view(event))
            }
            workspace_events::REMOVE => {
                let named: Named = argument(request, "an event is named by its id")?;
                let event = visible_event(kept, &named.event, &theirs.person)?;
                let dir = self.dir(&theirs.workspace, &named.event);
                if event.stored.proposal.is_some() {
                    // Its acceptance may still be waiting for our installation receipt.
                    // Keep the owner and stable ID before stopping the native runner.
                    tilo_files::native::atomic::write_private(
                        &dir.with_extension("removed"),
                        event.stored.person.as_bytes(),
                    )
                    .map_err(|error| unavailable(error.to_string()))?;
                }
                drop(kept.remove(&named.event));
                if let Err(error) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(event = %named.event, %error, "a removed event's directory stays");
                }
                said(&listed(kept, &theirs.person))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Install an initiative event under the ordinary event policy. A retry returns
    /// the original ID even if someone has since changed or removed the event.
    pub(super) async fn set_up_once(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        authority: &Authority,
        installation: Installation<'_>,
        wanted: SetUp,
    ) -> Result<String, ActionCallError> {
        let action = workspace_events::SET_UP;
        let theirs = self
            .reach(
                workspace_id,
                caller,
                authority,
                Store::EVENTS,
                action,
                effect_of(action),
            )
            .await?;
        let person = hex::encode(theirs.person);
        let (id, proposal) = match installation {
            Installation::LookRound(id) => (
                uuid::Uuid::parse_str(id).map_err(|_| no_such_event())?,
                None,
            ),
            Installation::Proposal(proposal) => (
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_URL,
                    format!(
                        "tilo.initiative/{}/{person}/proposals/{proposal}/event",
                        theirs.workspace
                    )
                    .as_bytes(),
                ),
                Some(proposal),
            ),
        };
        let id = id.to_string();
        let mut all = self.inner.running.lock_recover();
        let kept = all.entry(theirs.workspace.clone()).or_default();
        if let Some(event) = kept.get(&id) {
            return (event.stored.personal == Some(proposal.is_none())
                && event.stored.proposal.as_deref() == proposal
                && event.stored.person.eq_ignore_ascii_case(&person))
            .then_some(id)
            .ok_or_else(no_such_event);
        }
        let dir = self.dir(&theirs.workspace, &id);
        if proposal.is_some() && removed(&dir, &person).map_err(unavailable)? {
            return Ok(id);
        }
        if dir
            .try_exists()
            .map_err(|error| unavailable(error.to_string()))?
        {
            return Err(unavailable("the saved event could not be resumed".into()));
        }
        self.set_up(&theirs, kept, wanted, id, Some(installation))
            .map(|event| event.id)
    }

    fn set_up(
        &self,
        theirs: &Theirs,
        kept: &mut BTreeMap<String, Running>,
        asked: SetUp,
        id: String,
        installation: Option<Installation<'_>>,
    ) -> Result<EventView, ActionCallError> {
        if kept.len() >= MAX_EVENTS {
            return Err(refused(format!(
                "a workspace keeps at most {MAX_EVENTS} events"
            )));
        }
        let definition = Definition {
            id: id.clone(),
            name: asked.name,
            when: asked.when,
            only_if: asked.only_if,
            every: asked.every,
            instruction: asked.instruction,
            told_as: told_as_of(asked.told_as.as_deref().unwrap_or_default()).map_err(refused)?,
            set_up_at_ms: tilo_clock::unix_now_ms_i64(),
        }
        .checked()
        .map_err(refused)?;
        watched_address(&definition).map_err(refused)?;
        let stored = Stored {
            definition,
            by: theirs.by.clone(),
            person: hex::encode(theirs.person),
            personal: Some(matches!(installation, Some(Installation::LookRound(_)))),
            proposal: match installation {
                Some(Installation::Proposal(proposal)) => Some(proposal.to_owned()),
                _ => None,
            },
        };
        let event = self.begin(&theirs.workspace, stored).map_err(unavailable)?;
        let note = NoteChange {
            instruction: asked.purpose,
            style: asked.style,
            guidance: asked.guidance,
            ..NoteChange::default()
        };
        if note != NoteChange::default()
            && let Err(error) = event.kept.keep(note)
        {
            // An event is set up whole or not at all.
            drop(event);
            let _ = std::fs::remove_dir_all(self.dir(&theirs.workspace, &id));
            return Err(step_error(error));
        }
        let view = view(&event);
        kept.insert(id, event);
        Ok(view)
    }

    fn change(
        &self,
        theirs: &Theirs,
        kept: &mut BTreeMap<String, Running>,
        asked: Change,
    ) -> Result<EventView, ActionCallError> {
        let current = visible_event(kept, &asked.event, &theirs.person)?;
        let mut definition = current.stored.definition.clone();
        if let Some(name) = asked.name {
            definition.name = name;
        }
        if let Some(when) = asked.when {
            definition.when = when;
        }
        if let Some(only_if) = asked.only_if {
            definition.only_if = Some(only_if);
        }
        if let Some(every) = asked.every {
            definition.every = Some(every);
        }
        if let Some(instruction) = asked.instruction {
            definition.instruction = instruction;
        }
        if let Some(said) = asked.told_as {
            definition.told_as = told_as_of(&said).map_err(refused)?;
        }
        let definition = definition.checked().map_err(refused)?;
        watched_address(&definition).map_err(refused)?;
        let note = NoteChange {
            instruction: asked.purpose,
            style: asked.style,
            guidance: asked.guidance,
            accepted: asked.accepted,
            rejected: asked.rejected,
            kept: asked.kept,
        };
        if note != NoteChange::default() {
            current.kept.keep(note).map_err(step_error)?;
        }
        if definition != current.stored.definition {
            // The same event runs another program from now on: its runner
            // stops, and the new one carries on from what the event keeps,
            // its count and its note included. A changed trigger starts its
            // own times. Its author stays who set it up.
            let author = current.stored.clone();
            drop(kept.remove(&asked.event));
            let event = self
                .begin(
                    &theirs.workspace,
                    Stored {
                        definition,
                        ..author
                    },
                )
                .map_err(unavailable)?;
            kept.insert(asked.event.clone(), event);
        }
        kept.get(&asked.event).map(view).ok_or_else(no_such_event)
    }
}

/// The runtime the events work on, where there is one.
fn runtime() -> Option<tokio::runtime::Handle> {
    tokio::runtime::Handle::try_current().ok()
}

fn no_runtime() -> ActionCallError {
    unavailable("this server is not running its workspaces".to_owned())
}

/// Who asks, as the events answer a person and their assistant; none for a
/// bot, an app, or an agent no person's session stands behind.
fn asker_of(caller: &Caller) -> Option<Asker> {
    let acting = caller.acting();
    if acting.app().is_some() {
        None
    } else if acting.is_person() {
        Some(Asker::Person)
    } else if acting.is_agent()
        && (caller.is_session_delegated() || caller.roster_created_at().is_some())
    {
        Some(Asker::Assistant)
    } else {
        None
    }
}

/// Who makes a change now, as authorship keeps it: the agent on a session, or
/// the person themselves.
fn member_of(caller: &Caller, person: &[u8; 32]) -> MemberId {
    if caller.acting().is_agent() {
        let agent = caller
            .agent_name()
            .or_else(|| caller.acting().agent_id())
            .unwrap_or_default();
        MemberId::agent(agent.to_owned())
    } else {
        MemberId::person(hex::encode(person))
    }
}

/// What an action does to the workspace's events, as its policy weighs it.
const fn effect_of(action: &str) -> ActionKind {
    match action.as_bytes() {
        b"list" => ActionKind::Read,
        _ => ActionKind::Undoable,
    }
}

/// The workspace whose store folder `hexed` names, where it names one.
fn workspace_of(hexed: &str) -> Option<String> {
    let bytes = tilo_failure::ignored!(
        "a store folder whose name is not hex is not a workspace's",
        hex::decode(hexed).ok()
    )?;
    tilo_failure::ignored!(
        "a store folder whose name is not text is not a workspace's",
        String::from_utf8(bytes).ok()
    )
}

/// Whether the creator's proposal event was removed after it was installed. The receipt
/// lives beside the event directory, so deleting its instructions and progress keeps it.
fn removed(dir: &Path, person: &str) -> Result<bool, String> {
    match std::fs::read_to_string(dir.with_extension("removed")) {
        Ok(owner) if owner.eq_ignore_ascii_case(person) => Ok(true),
        Ok(_) => Err("the event removal receipt could not be verified".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// What `dir` keeps of an event: none where it does not read.
fn read_stored(dir: &Path) -> Result<Stored, String> {
    let bytes = std::fs::read(dir.join(DESCRIPTION)).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn write_stored(dir: &Path, stored: &Stored) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(stored).map_err(|error| error.to_string())?;
    let path = dir.join(DESCRIPTION);
    if stored.personal == Some(true) {
        tilo_files::native::atomic::write_private(&path, &bytes)
    } else {
        tilo_files::native::atomic::write(&path, &bytes)
    }
    .map_err(|error| error.to_string())
}

/// Who actually set up a saved event, used to assign older look-round data.
pub(super) fn stored_person(workspace_root: &Path, id: &str) -> Option<[u8; 32]> {
    let id = uuid::Uuid::parse_str(id).ok()?.to_string();
    let stored = read_stored(&workspace_root.join(FOLDER).join(&id)).ok()?;
    (stored.definition.id == id).then_some(())?;
    let mut person = [0; 32];
    hex::decode_to_slice(&stored.person, &mut person).ok()?;
    Some(person)
}

/// A member's ordinary installed event, positively identified on the service which runs it.
pub(super) fn member_event(workspace_root: &Path, id: uuid::Uuid, person: &[u8; 32]) -> bool {
    let id = id.to_string();
    read_stored(&workspace_root.join(FOLDER).join(&id)).is_ok_and(|event| {
        event.definition.id == id
            && event.person.eq_ignore_ascii_case(&hex::encode(person))
            && event.personal == Some(false)
    })
}

/// Earlier releases recorded the id of the active look-round beside the events.
pub(super) fn old_look_round(workspace_root: &Path) -> Result<Option<String>, String> {
    #[derive(Deserialize)]
    struct LookRound {
        event: Option<String>,
    }
    match std::fs::read(workspace_root.join("initiative.json")) {
        Ok(bytes) => serde_json::from_slice::<LookRound>(&bytes)
            .map(|look| look.event)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The directories in `at`, by name; none where `at` is no directory.
fn directories(at: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    found.sort();
    found
}

/// `theirs`, in the order they were set up.
fn listed(theirs: &BTreeMap<String, Running>, person: &[u8; 32]) -> Events {
    let mut events: Vec<EventView> = theirs
        .values()
        .filter(|event| event.stored.visible_to(person))
        .map(view)
        .collect();
    events.sort_by(|a, b| (a.set_up_at_ms, &a.id).cmp(&(b.set_up_at_ms, &b.id)));
    Events { events }
}

fn visible_event<'a>(
    events: &'a BTreeMap<String, Running>,
    id: &str,
    person: &[u8; 32],
) -> Result<&'a Running, ActionCallError> {
    events
        .get(id)
        .filter(|event| event.stored.visible_to(person))
        .ok_or_else(no_such_event)
}

/// Why the address `definition` has this server look at on the web is not
/// one it fetches, when it is not: this server's own network, an address
/// that is not public, one with a password in it. Said when the event is
/// described; the fetch refuses the same at every look, a name by what it
/// resolves to then.
pub fn watched_address(definition: &Definition) -> Result<(), String> {
    let Ok(TriggerSpec::SourceChanged { kind, at, .. }) = definition.trigger() else {
        return Ok(());
    };
    if !tilo_automation::source_change::looked_at(&kind) {
        return Ok(());
    }
    let address =
        tilo_net::Url::parse(&at).map_err(|error| format!("not a web address: {error}"))?;
    tilo_web_search::check_url(&address).map_err(|refusal| refusal.to_string())
}

/// The next time `trigger` fires after `now_ms` on this host's clock, ms
/// since the epoch: a schedule's next time, a moment itself. None for a
/// trigger that is not on the clock.
#[must_use]
fn next_fire_ms(trigger: &TriggerSpec, now_ms: i64) -> Option<i64> {
    let now = jiff::Timestamp::from_millisecond(now_ms).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    Timed::of(trigger)?.first_ms(now, &jiff::tz::TimeZone::system())
}

/// `event` as a member reads it now.
fn view(event: &Running) -> EventView {
    let kept = event.kept.read();
    let definition = &event.stored.definition;
    let on = !kept.paused();
    let next_at_ms = definition
        .trigger()
        .ok()
        .filter(|_| on)
        .and_then(|trigger| match trigger {
            // A moment fires once: it has no next time once it ran.
            TriggerSpec::Once { at_ms } => (kept.runs() == 0).then_some(at_ms),
            trigger => next_fire_ms(&trigger, tilo_clock::unix_now_ms_i64()),
        });
    EventView {
        id: definition.id.clone(),
        name: definition.name.clone(),
        sort: if definition.on_a_clock() {
            Sort::Scheduled
        } else {
            Sort::Event
        },
        when: definition.when.clone(),
        next_at_ms,
        only_if: definition.only_if.clone(),
        every: definition.every,
        count: kept.counts().get("").copied().unwrap_or(0),
        instruction: definition.instruction.clone(),
        told_as: definition.told_as,
        note: kept::note_value(kept.note()),
        on,
        runs: kept.runs(),
        last_run: kept.last_run().map(|last| workspace_events::LastRun {
            at_ms: last.at_ms,
            ended: match last.ended {
                kept::Ended::Ran => workspace_events::Ended::Ran,
                kept::Ended::Waits => workspace_events::Ended::Waits,
                kept::Ended::Refused => workspace_events::Ended::Refused,
                kept::Ended::Failed => workspace_events::Ended::Failed,
            },
            why: last.why.clone(),
        }),
        set_up_at_ms: definition.set_up_at_ms,
        by: event.stored.by.clone(),
    }
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

fn unavailable(reason: String) -> ActionCallError {
    ActionCallError::Unavailable { reason }
}

fn no_such_event() -> ActionCallError {
    refused("the workspace keeps no such event".to_owned())
}

fn step_error(error: HostStepError) -> ActionCallError {
    match error {
        HostStepError::Refused(reason) => refused(reason),
        HostStepError::Unavailable(reason) => unavailable(reason),
    }
}

/// What a request's arguments name, or `what` they should have named. The
/// reason never quotes the arguments.
fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let invalid = || ActionCallError::InvalidRequest {
        reason: what.to_owned(),
    };
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(invalid());
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|_| invalid())
}

fn said<T: serde::Serialize>(answer: &T) -> Result<Value, ActionCallError> {
    serde_json::to_value(answer).map_err(|error| refused(error.to_string()))
}

/// One action as the events' rows offer it.
struct Offered {
    action: &'static str,
    label: &'static str,
    description: &'static str,
    params: Vec<ActionParam>,
    kind: ActionKind,
    retry: RetryPolicy,
}

/// The rows the events' actions are offered as.
fn rows() -> Vec<CatalogueRow> {
    offered().into_iter().map(row).collect()
}

/// The parameter that names an event.
fn event_param() -> ActionParam {
    ActionParam::required(
        "event",
        ParamType::Text,
        "The event's id, as `list` answers it.",
    )
}

/// What describes an event: needed to set one up, each left out or given in
/// a change.
fn described(setting_up: bool) -> Vec<ActionParam> {
    let needed = |name: &'static str, description: &'static str| {
        if setting_up {
            ActionParam::required(name, ParamType::Text, description)
        } else {
            ActionParam::optional(name, ParamType::Text, description)
        }
    };
    vec![
        needed("name", "What it is read by, in a few words."),
        needed(
            "when",
            "What starts it, as one line: `once at 2026-11-17T11:32:00Z` (a moment, with its \
             offset from UTC), `every day at 09:00`, `every Monday, Thursday at 17:30` (this \
             host's clock), `every 3600 seconds`, `15 minutes before \"entries.*.start\" in a \
             \"tilo.accounts.calendars\" where categories is \"Travel\"` (a moment before, `at` \
             or after the start or the end, `entries.*.end`, of each entry in the workspace's \
             calendars, every occurrence of one that repeats; each `where` names a field of the \
             entry and the value it holds, and what the assistant is told carries the entry), \
             `when something new is in \"tilo.accounts.mail\"` (a message arrives in a mail \
             account named for its assistant; `at \"ACCOUNT\"` after the kind keeps it to one \
             account, and each `where` names a field of the message and the value it holds, \
             such as `from` for the sender's address; what you are told carries who wrote, \
             what about and the start of the words, as data), `when something new is in \
             \"tilo.web.feed\" at \"https://example.org/feed.xml\"` (a feed on the public web \
             lists an entry it did not list before; you are told its title, link, date and \
             summary, as data), `when something new is in \"tilo.web.page\" at \
             \"https://example.org/prices\"` (the text a person reads on a public page \
             changed; you are told the new lines, as data), or a change in the workspace as \
             a Shortcut writes it. A feed or a page is looked at by the server every hour, \
             with no model, and starts you only when something is new; `looked at every 900 \
             seconds` after the address sets how often, 300 at the least. A page behind a \
             sign-in is not read. When the source stays unreachable you are told once.",
        ),
        needed(
            "instruction",
            "What its author's assistant is told to do when it fires.",
        ),
        ActionParam::optional(
            "onlyIf",
            ParamType::Text,
            "A question the author's own model answers with yes or no about what happened; the \
             event goes on only on a yes. Empty asks none.",
        ),
        ActionParam::optional(
            "every",
            ParamType::Integer,
            "Go on only every so many times: 10 for every tenth. 1 is every time.",
        ),
        ActionParam::optional(
            "toldAs",
            ParamType::Text,
            "How its result is told to its author: `brief` reaches them with a notice every \
             time it fires, as a reminder does; `news` stays in their list and reaches them \
             only where their own rule calls it important; `reminder` is something they asked \
             to be reminded of. Left out, a moment (`once at`) is a reminder and everything \
             else is news. Say `brief` only where your person asked to be told every time. \
             Empty takes the word away.",
        ),
        ActionParam::optional(
            "purpose",
            ParamType::Text,
            "What the event is for, in its author's words.",
        ),
        ActionParam::optional("style", ParamType::Text, "How what it writes should read."),
        ActionParam::optional(
            "guidance",
            ParamType::Text,
            "What its author cares about and how the work is done.",
        ),
    ]
}

/// What a change may name: the event, anything that describes it, an example
/// for its note and what a run keeps for the next.
fn changed() -> Vec<ActionParam> {
    let mut params = vec![event_param()];
    params.extend(described(false));
    params.extend([
        ActionParam::optional(
            "accepted",
            ParamType::Text,
            "An answer the author accepted, to keep as an example.",
        ),
        ActionParam::optional(
            "rejected",
            ParamType::Text,
            "An answer the author rejected, to keep as an example.",
        ),
        ActionParam::optional(
            "kept",
            ParamType::Object,
            "What this run keeps for the next: what it saw last, where it stopped.",
        ),
    ]);
    params
}

/// What every member whose role reaches the workspace's events is offered.
fn offered() -> Vec<Offered> {
    vec![
        Offered {
            action: workspace_events::LIST,
            label: "List events",
            description: "The workspace's scheduled work and events: what starts each, its \
                          filter, count and note, whether it is on, and how its last run ended.",
            params: Vec::new(),
            kind: ActionKind::Read,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: workspace_events::SET_UP,
            label: "Set up an event",
            description: "Have the host do something at a time or when something happens, with \
                          no device on: a reminder, a summary every morning, a look at every \
                          tenth thing that arrives. When it fires its author's assistant is told \
                          the instruction.",
            params: described(true),
            kind: ActionKind::Undoable,
            retry: RetryPolicy::AtMostOnce,
        },
        Offered {
            action: workspace_events::CHANGE,
            label: "Change an event",
            description: "Change what starts an event, its filter, its count, its instruction or \
                          its note. What is left out stays as it is.",
            params: changed(),
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: workspace_events::PAUSE,
            label: "Pause an event",
            description: "Stop an event from firing until it is resumed. What comes while it is \
                          paused is not answered later.",
            params: vec![event_param()],
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: workspace_events::RESUME,
            label: "Resume an event",
            description: "Let a paused event fire again.",
            params: vec![event_param()],
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: workspace_events::REMOVE,
            label: "Remove an event",
            description: "Remove an event for good, with its count and its note.",
            params: vec![event_param()],
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
    ]
}

/// `offered` as a catalogue's row.
fn row(offered: Offered) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: workspace_events::PROVIDER.to_owned(),
        app_id: workspace_events::PROVIDER.to_owned(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: workspace_events::RESOURCE.to_owned(),
            kind: workspace_events::KIND.to_owned(),
            label: "Events".to_owned(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(workspace_events::RESOURCE, offered.action),
            resource_kind: workspace_events::KIND.to_owned(),
            resource_label: "Events".to_owned(),
            action_label: offered.label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(offered.description.to_owned()),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: offered.kind,
                risk: RiskLevel::Low,
                confirmation: ConfirmationPolicy::None,
                retry: offered.retry,
                execution: ExecutionMode::Immediate,
                result: ResultSpec::Json {
                    schema: json!({"type": "object"}),
                },
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: offered.params,
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}
