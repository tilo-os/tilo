//! A person's events on this server: kept, run, and answered as typed
//! actions ([`tilo_actions::remote::person_events`]).
//!
//! An event is a Shortcut the server runs for one person
//! ([`tilo_agent_session::bot::event`]). It is run by the engine that runs a
//! workspace's bots ([`super::installed::BotRunner`]), for that person, and
//! is no member of the workspace: its program's steps are the server's own (a
//! judgement, a count, an instruction handed to the person's assistant), so
//! it changes no record and nothing has to admit it. What it starts is the
//! assistant's own work, under the assistant's own bounds.
//!
//! Each event keeps a directory of its own under the server's state: what was
//! described, what its runner took in, and what it keeps between its runs.
//! A server that starts again runs every event it kept, and a moment that
//! passed while it was down fires once.
//!
//! The actions are answered for the person a caller is, or acts for on a
//! session that person handed it, and only in a workspace that is theirs.
//! Whoever else asks, another person's agent included, is refused, and no
//! action reaches an event of anyone but the asker's own person. Letting an
//! event's work act unasked ([`person_events::PERMIT`]) is the person's
//! alone: their agent is refused it.
//!
//! An event says how its result is told to its person
//! ([`Definition::tells_as`]), and the turn it starts carries that as its
//! cause. It says so only of its own person's event: nothing here names
//! anyone else, and whether a thing then reaches that person, and through
//! their do-not-disturb, stays their own rule's to say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::contract::{
    ActionContract, ActionKind, ConfirmationPolicy, ExecutionMode, ResultSpec, RetryPolicy,
    RiskLevel,
};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::person_events::{self, Change, EventView, Events, Named, SetUp, Sort};
use tilo_actions::remote::person_telling::Kind;
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionMessage,
};
use tilo_actions::{ActionOutput, ExecutionLocus, OperationKind};
use tilo_agent_session::bot::event::{Definition, told_as_of};
use tilo_agent_session::bot::host_steps::{HostStepError, HostSteps, Keeps as _};
use tilo_agent_session::bot::kept::{self, NoteChange};
use tilo_automation::TriggerSpec;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::installed::{self, BotRunner, InstalledBot};
use super::kept::KeptFile;
use crate::workspaces::Workspaces;

/// The most events one person keeps in one workspace.
pub const MAX_EVENTS: usize = 200;
/// What an event's installation says it was installed from.
const ENTRY: &str = "event";
/// The file an event's description is kept in.
const DESCRIPTION: &str = "event.json";
/// The file an event's runner keeps what it took in.
const PROGRESS: &str = "progress.json";

/// Whose events: the workspace, and the person by their root, hex.
type Owner = (String, String);

/// What answers the steps of one event.
type StepsFor = Arc<dyn Fn(&Definition) -> Arc<dyn HostSteps> + Send + Sync>;

/// Whether `message` is for a person's events.
#[must_use]
pub fn addresses(message: &RemoteActionMessage) -> bool {
    match message {
        RemoteActionMessage::CatalogueRequest { query, .. } => {
            query.provider.as_deref() == Some(person_events::PROVIDER)
        }
        RemoteActionMessage::Invoke(request) => request.target.provider == person_events::PROVIDER,
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

/// Who asks for a person's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Asker {
    /// The person themselves, on a device of theirs.
    Person,
    /// An agent on a session the person handed it.
    Assistant,
}

/// One event running here.
struct Running {
    definition: Definition,
    kept: Arc<KeptFile>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Inner {
    workspaces: Workspaces,
    /// Where every event keeps its directory.
    state: PathBuf,
    /// What the server answers an event's steps with.
    steps: StepsFor,
    running: Mutex<BTreeMap<Owner, BTreeMap<String, Running>>>,
}

/// The events of this server's people: every one it keeps runs for as long as
/// this, or a clone of it, is held.
#[derive(Clone)]
pub struct PersonEvents {
    inner: Arc<Inner>,
}

impl PersonEvents {
    /// Run every event kept under `state`, and keep new ones there. Their
    /// steps are answered by this server ([`super::assistant`]): with
    /// `actions`, the server's action service, an instruction an event hands
    /// over starts a turn of its person's assistant. Called on the runtime
    /// the events work on.
    #[must_use]
    pub fn start(
        workspaces: Workspaces,
        state: PathBuf,
        actions: Option<tilo_agent_session::workspace_actions::WorkspaceActions>,
    ) -> Self {
        Self::start_behind(workspaces, state, actions, None)
    }

    /// [`Self::start`], with `before` put in front of what answers each
    /// event's steps: it is handed what would answer and answers in its
    /// place, so a step can be looked at before it is taken.
    #[must_use]
    pub fn start_behind(
        workspaces: Workspaces,
        state: PathBuf,
        actions: Option<tilo_agent_session::workspace_actions::WorkspaceActions>,
        before: Option<super::initiative::Before>,
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
        Self::started(workspaces, state, steps)
    }

    /// [`Self::start`], with what answers the events' steps given.
    #[must_use]
    pub fn start_with(workspaces: Workspaces, state: PathBuf, steps: Arc<dyn HostSteps>) -> Self {
        Self::started(
            workspaces,
            state,
            Arc::new(move |_: &Definition| Arc::clone(&steps)),
        )
    }

    fn started(workspaces: Workspaces, state: PathBuf, steps: StepsFor) -> Self {
        let events = Self {
            inner: Arc::new(Inner {
                workspaces,
                state,
                steps,
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
        for (workspace, at) in directories(&self.inner.state) {
            for (person, at) in directories(&at) {
                let owner = (workspace.clone(), person);
                for (id, dir) in directories(&at) {
                    let started = std::fs::read(dir.join(DESCRIPTION))
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            serde_json::from_slice::<Definition>(&bytes)
                                .map_err(|error| error.to_string())
                        })
                        .and_then(|definition| self.run(&owner, definition, &dir));
                    match started {
                        Ok(event) => kept.push((owner.clone(), id, event)),
                        Err(error) => {
                            tracing::warn!(workspace = %owner.0, event = %id, %error, "a kept event does not run");
                        }
                    }
                }
            }
        }
        let mut running = self.inner.running.lock_recover();
        for (owner, id, event) in kept {
            running.entry(owner).or_default().insert(id, event);
        }
    }

    fn dir(&self, owner: &Owner, id: &str) -> PathBuf {
        self.inner.state.join(&owner.0).join(&owner.1).join(id)
    }

    /// Keep `definition` in its directory and run it.
    fn begin(&self, owner: &Owner, definition: Definition) -> Result<Running, String> {
        let dir = self.dir(owner, &definition.id);
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let bytes = serde_json::to_vec_pretty(&definition).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&dir.join(DESCRIPTION), &bytes)
            .map_err(|error| error.to_string())?;
        self.run(owner, definition, &dir)
    }

    /// Run `definition` for its person from what `dir` keeps: where its
    /// runner had read up to, its counts and its note.
    fn run(&self, owner: &Owner, definition: Definition, dir: &Path) -> Result<Running, String> {
        let bot = InstalledBot {
            installation: definition.id.clone(),
            entry: ENTRY.to_owned(),
            name: definition.name.clone(),
            program: definition.program_text()?,
            inputs: definition.inputs(),
            installed_at_ms: definition.set_up_at_ms,
            release: String::new(),
        };
        // An event changes no record, so nothing is ever signed with this
        // key: it only names the run's caller, anew at each start.
        let key = SigningKey::from_bytes(&rand::random());
        let runner = BotRunner::new(
            self.inner.workspaces.clone(),
            owner.0.clone(),
            self.inner.workspaces.governor().clone(),
            key,
            bot,
            BTreeMap::new(),
            dir.join(PROGRESS),
        )?
        .runs_for(owner.1.clone(), (self.inner.steps)(&definition));
        let kept = runner.kept();
        Ok(Running {
            definition,
            kept,
            task: tokio::spawn(runner.run()),
        })
    }

    /// Answer `message` for `workspace_id`: the actions' rows, or one
    /// action. `None` for a message that needs no reply.
    #[must_use]
    pub fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                Some(Pending::ready(RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result: Ok(self.catalogue(workspace_id, caller, &query)),
                }))
            }
            RemoteActionMessage::Invoke(request) => Some(Pending::ready(
                RemoteActionMessage::Receipt(self.invoke(workspace_id, caller, &request)),
            )),
            _ => None,
        }
    }

    /// The actions `caller` is offered on their person's events in
    /// `workspace_id`: none for a caller they are not answered for.
    #[must_use]
    pub fn catalogue(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> CataloguePage {
        let rows = self
            .asking(workspace_id, caller)
            .map(|(_, asker)| rows(asker))
            .unwrap_or_default();
        let mut page = CataloguePage::from_rows(String::from("0"), rows, query);
        if query.provider.is_none() {
            page.providers = self.provider(workspace_id, caller).into_iter().collect();
        }
        page
    }

    /// The events' provider as a catalogue lists it for `caller`, when
    /// they are answered here.
    #[must_use]
    pub fn provider(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Option<CatalogueProvider> {
        self.asking(workspace_id, caller)
            .ok()
            .map(|_| CatalogueProvider {
                provider: person_events::PROVIDER.to_owned(),
                app_id: person_events::PROVIDER.to_owned(),
                workspace: workspace_id.to_string(),
                document_id: String::new(),
                browsable: true,
            })
    }

    /// Take one action for `caller`, answered at once.
    #[must_use]
    pub fn invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> InvocationReceipt {
        let status = match self.answer(workspace_id, caller, request) {
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

    /// The person `caller` asks for and as what: the person themselves, or
    /// an agent on a session they handed it, in a workspace that is that
    /// person's. A bot, an app, and an agent no person's session stands
    /// behind are told what a caller asking for something that does not
    /// exist is told.
    pub(super) fn asking(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Result<([u8; 32], Asker), ActionCallError> {
        let acting = caller.acting();
        let asker = if acting.app().is_some() {
            None
        } else if acting.is_person() {
            Some(Asker::Person)
        } else if acting.is_agent() && caller.is_session_delegated() {
            Some(Asker::Assistant)
        } else {
            None
        };
        let (Some(asker), Some(person)) = (asker, caller.user_public_key().copied()) else {
            return Err(ActionCallError::UnknownAction);
        };
        if caller
            .workspace()
            .is_some_and(|confined| WorkspaceId::parse(confined) != Some(workspace_id))
        {
            return Err(ActionCallError::UnknownAction);
        }
        let workspaces = &self.inner.workspaces;
        // Where a server keeps workspaces of people's own, a workspace is
        // whose the server says it is. A server that keeps none governs its
        // workspaces for its own person, and they are that person's.
        let theirs = if workspaces.keeps_own_workspaces() {
            workspaces.owns(&person, &workspace_id.to_string())
        } else {
            workspaces.governor().user_public_key == person
        };
        if !theirs {
            return Err(ActionCallError::Refused {
                reason: "events are set up in a workspace of your own".into(),
            });
        }
        Ok((person, asker))
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "an action is taken whole: a person's events stay locked until it is answered"
    )]
    fn answer(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let (person, _asker) = self.asking(workspace_id, caller)?;
        let owner = (workspace_id.to_string(), hex::encode(person));
        let mut running = self.inner.running.lock_recover();
        let theirs = running.entry(owner.clone()).or_default();
        match request.target.address.action.as_str() {
            person_events::LIST => said(&listed(theirs)),
            person_events::SET_UP => {
                let wanted: SetUp = argument(
                    request,
                    "an event to set up names itself, what starts it and what to do then",
                )?;
                said(&self.set_up(&owner, theirs, wanted)?)
            }
            person_events::CHANGE => {
                let changes: Change = argument(request, "a change names the event it changes")?;
                said(&self.change(&owner, theirs, changes)?)
            }
            action @ (person_events::PAUSE | person_events::RESUME) => {
                let named: Named = argument(request, "an event is named by its id")?;
                let event = theirs.get(&named.event).ok_or_else(no_such_event)?;
                event
                    .kept
                    .pause(action == person_events::PAUSE)
                    .map_err(step_error)?;
                said(&view(event))
            }
            person_events::REMOVE => {
                let named: Named = argument(request, "an event is named by its id")?;
                // Dropped, its runner stops; then what it kept goes.
                drop(theirs.remove(&named.event).ok_or_else(no_such_event)?);
                let dir = self.dir(&owner, &named.event);
                if let Err(error) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(event = %named.event, %error, "a removed event's directory stays");
                }
                said(&listed(theirs))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    fn set_up(
        &self,
        owner: &Owner,
        theirs: &mut BTreeMap<String, Running>,
        asked: SetUp,
    ) -> Result<EventView, ActionCallError> {
        if theirs.len() >= MAX_EVENTS {
            return Err(refused(format!(
                "a person keeps at most {MAX_EVENTS} events in a workspace"
            )));
        }
        let definition = Definition {
            id: uuid::Uuid::new_v4().to_string(),
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
        let id = definition.id.clone();
        let event = self.begin(owner, definition).map_err(unavailable)?;
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
            let _ = std::fs::remove_dir_all(self.dir(owner, &id));
            return Err(step_error(error));
        }
        let view = view(&event);
        theirs.insert(id, event);
        Ok(view)
    }

    fn change(
        &self,
        owner: &Owner,
        theirs: &mut BTreeMap<String, Running>,
        asked: Change,
    ) -> Result<EventView, ActionCallError> {
        let current = theirs.get(&asked.event).ok_or_else(no_such_event)?;
        let mut definition = current.definition.clone();
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
        if definition != current.definition {
            // The same event runs another program from now on: its runner
            // stops, and the new one carries on from what the event keeps,
            // its count and its note included. A changed trigger starts its
            // own times.
            drop(theirs.remove(&asked.event));
            let event = self.begin(owner, definition).map_err(unavailable)?;
            theirs.insert(asked.event.clone(), event);
        }
        theirs.get(&asked.event).map(view).ok_or_else(no_such_event)
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
fn listed(theirs: &BTreeMap<String, Running>) -> Events {
    let mut events: Vec<EventView> = theirs.values().map(view).collect();
    events.sort_by(|a, b| (a.set_up_at_ms, &a.id).cmp(&(b.set_up_at_ms, &b.id)));
    Events { events }
}

/// Why the address `definition` has this server look at on the web is not
/// one it fetches, when it is not: this server's own network, an address
/// that is not public, one with a password in it. Said when the event is
/// described; the fetch refuses the same at every look, a name by what it
/// resolves to then.
pub(super) fn watched_address(definition: &Definition) -> Result<(), String> {
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

/// `event` as its person reads it now.
fn view(event: &Running) -> EventView {
    let kept = event.kept.read();
    let definition = &event.definition;
    let on = !kept.paused();
    let next_at_ms = definition
        .trigger()
        .ok()
        .filter(|_| on)
        .and_then(|trigger| match trigger {
            // A moment fires once: it has no next time once it ran.
            TriggerSpec::Once { at_ms } => (kept.runs() == 0).then_some(at_ms),
            trigger => installed::next_fire_ms(&trigger, tilo_clock::unix_now_ms_i64()),
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
        last_run: kept.last_run().map(|last| person_events::LastRun {
            at_ms: last.at_ms,
            ended: match last.ended {
                kept::Ended::Ran => person_events::Ended::Ran,
                kept::Ended::Waits => person_events::Ended::Waits,
                kept::Ended::Refused => person_events::Ended::Refused,
                kept::Ended::Failed => person_events::Ended::Failed,
            },
            why: last.why.clone(),
        }),
        set_up_at_ms: definition.set_up_at_ms,
    }
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

fn unavailable(reason: String) -> ActionCallError {
    ActionCallError::Unavailable { reason }
}

fn no_such_event() -> ActionCallError {
    refused("you keep no such event here".to_owned())
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

/// The rows the events' actions are offered as to `asker`. What only the
/// person themselves may do is not offered to their agent.
fn rows(_asker: Asker) -> Vec<CatalogueRow> {
    for_their_assistant().into_iter().map(row).collect()
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
        needed("name", "What its person reads it by, in a few words."),
        needed(
            "when",
            "What starts it, as one line: `once at 2026-11-17T11:32:00Z` (a moment, with its \
             offset from UTC), `every day at 09:00`, `every Monday, Thursday at 17:30` (this \
             host's clock), `every 3600 seconds`, `15 minutes before \"entries.*.start\" in a \
             \"tilo.accounts.calendars\" where categories is \"Travel\"` (a moment before, `at` \
             or after the start or the end, `entries.*.end`, of each entry in its person's \
             calendars, every occurrence of one that repeats; each `where` names a field of the \
             entry and the value it holds, and what the assistant is told carries the entry), \
             `when something new is in \"tilo.accounts.mail\"` (a message arrives in a mail \
             account its person named you for; `at \"ACCOUNT\"` after the kind keeps it to one \
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
            "What the person's assistant is told to do when it fires.",
        ),
        ActionParam::optional(
            "onlyIf",
            ParamType::Text,
            "A question the person's own model answers with yes or no about what happened; the \
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
            "How its result is told to your person: `brief` reaches them with a notice every \
             time it fires, as a reminder does; `news` stays in their list and reaches them \
             only where their own rule calls it important; `reminder` is something they asked \
             to be reminded of. Left out, a moment (`once at`) is a reminder and everything \
             else is news. Say `brief` only where your person asked to be told every time. \
             Empty takes the word away.",
        ),
        ActionParam::optional(
            "purpose",
            ParamType::Text,
            "What the event is for, in its person's words.",
        ),
        ActionParam::optional("style", ParamType::Text, "How what it writes should read."),
        ActionParam::optional(
            "guidance",
            ParamType::Text,
            "What its person cares about and how the work is done.",
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
            "An answer the person accepted, to keep as an example.",
        ),
        ActionParam::optional(
            "rejected",
            ParamType::Text,
            "An answer the person rejected, to keep as an example.",
        ),
        ActionParam::optional(
            "kept",
            ParamType::Object,
            "What this run keeps for the next: what it saw last, where it stopped.",
        ),
    ]);
    params
}

/// What a person and their assistant are both offered.
fn for_their_assistant() -> Vec<Offered> {
    vec![
        Offered {
            action: person_events::LIST,
            label: "List events",
            description: "Your person's scheduled work and events in this workspace: what starts \
                          each, its filter, count and note, whether it is on, and how its last \
                          run ended.",
            params: Vec::new(),
            kind: ActionKind::Read,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: person_events::SET_UP,
            label: "Set up an event",
            description: "Have the host do something for your person at a time or when something \
                          happens, with no device of theirs on: a reminder, a summary every \
                          morning, a look at every tenth thing that arrives. When it fires you \
                          are told the instruction.",
            params: described(true),
            kind: ActionKind::Undoable,
            retry: RetryPolicy::AtMostOnce,
        },
        Offered {
            action: person_events::CHANGE,
            label: "Change an event",
            description: "Change what starts an event, its filter, its count, its instruction or \
                          its note. What is left out stays as it is.",
            params: changed(),
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: person_events::PAUSE,
            label: "Pause an event",
            description: "Stop an event from firing until it is resumed. What comes while it is \
                          paused is not answered later.",
            params: vec![event_param()],
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: person_events::RESUME,
            label: "Resume an event",
            description: "Let a paused event fire again.",
            params: vec![event_param()],
            kind: ActionKind::Undoable,
            retry: RetryPolicy::Idempotent,
        },
        Offered {
            action: person_events::REMOVE,
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
        provider: person_events::PROVIDER.to_owned(),
        app_id: person_events::PROVIDER.to_owned(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: person_events::RESOURCE.to_owned(),
            kind: person_events::KIND.to_owned(),
            label: "Events".to_owned(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(person_events::RESOURCE, offered.action),
            resource_kind: person_events::KIND.to_owned(),
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
