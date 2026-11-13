//! The runs a person's conversations give this session's agents.
//! An agent works inside a turn: a person's message in a conversation the agent takes part in
//! ([`tilo_action_policy::turn`]). A run is that turn, as the broker's run channel delivers it to
//! the agent: the person's newest message in the conversation that the agent has not answered yet.
//! Nothing here keeps a record of its own: the conversation's own record says what is asked and
//! what was answered. A message summons its conversation's agents, and an agent's answer is the
//! reply with the id its summons and its id derive ([`chat::record::reply_id`]), so a turn is over
//! when its reply exists and a later message is a turn of its own. What the run's ops carry is the
//! turn; what the receiving device looked up about its cause (who wrote it, whether the agent takes
//! part, whether the turn ended) is read from the same record here.
//! Conversations are items of Chat's workspace record and their messages are its parts
//! ([`chat::parts`]), so this reads Chat's record through the same store every app of the workspace
//! opens, as the workspace's settings tabs read the workspace's agents.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tilo_action_policy::WorkspacePolicy;
use tilo_action_policy::turn::{Looked, Turn};
use tilo_app::framework::WorkspaceRecord;
use tilo_broker_core::services::{NativeTaskActionRequirement, TrackerRun, TrackerRuns};
use tilo_clock::wall;
use tilo_resource::{Id, Resource as _};
use ui::permissions::{LivePermissions, PermissionContext, UserId};
use ui::record::RecordId;
use ui::record::workspace::WorkspaceRecords;

/// The app that keeps the conversations: its own record of a workspace.
const CHAT_OWNER: &str = "so.tilo.chat";
/// How the run source names Chat where a record is opened.
const CHAT_APP: (&str, &str) = ("chat", "Chat");
/// Chat's verb that records a turn's work on the reply to its message: the one Chat's window
/// publishes on each message, run here on the same record from the same declaration.
const RECORD_WORK: &str = "record-work";

/// One workspace's conversations as the pass read them, and the runs they give.
#[derive(Clone, Default)]
struct Facts {
    workspace: String,
    /// The agents the workspace admits now, by their bare id.
    agents: Vec<String>,
    /// Its grants: a run's op is admitted by these.
    policy: Option<WorkspacePolicy>,
    /// The turns going in it: one per conversation and agent.
    turns: Vec<Entry>,
}

/// A turn going, as this device read it: the run the broker delivers, and what the same read says
/// about its cause.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    run: TrackerRun,
    looked: Looked,
}

/// What the pass read: each workspace's going turns.
#[derive(Default)]
struct Held {
    workspaces: Vec<Facts>,
}

impl Held {
    fn workspace(&self, workspace: &str) -> Result<&Facts, String> {
        self.workspaces
            .iter()
            .find(|facts| facts.workspace == workspace)
            .ok_or_else(|| format!("this session holds no workspace {workspace}"))
    }

    /// Whether `self` and `other` give different turns to deliver: the same
    /// workspaces, the same agents, and the same runs in each. The policy they
    /// were decided against is not part of it — a grant changing is announced
    /// elsewhere, and delivery is about which turns go, not what they may do.
    fn differs_from(&self, other: &Self) -> bool {
        self.workspaces.len() != other.workspaces.len()
            || self
                .workspaces
                .iter()
                .zip(&other.workspaces)
                .any(|(was, now)| {
                    was.workspace != now.workspace
                        || was.turns != now.turns
                        || was.agents != now.agents
                })
    }
}

/// The turns, as the broker's run channel reads them, and what their agents spent.
pub(crate) struct Runs {
    held: Mutex<Held>,
    /// What the runs' agents spent since the last pass, in millionths of the currency, by workspace
    /// and agent: the pass charges it to their day.
    spent: Mutex<BTreeMap<(String, String), u64>>,
    /// The total each run last reported, by workspace, task and epoch, so a report charges only
    /// what it added.
    reported: Mutex<BTreeMap<(String, String, u64), u64>>,
    /// Wakes the session's loop when a run reported what it spent, so the day it is charged to is
    /// the day the agent then works in.
    wake: Option<tilo_service::Wake>,
    /// The runs the agent door's admitted connections work in, beside the turns: each held for as
    /// long as its connection lasts ([`crate::door_runs`]).
    doors: Arc<crate::door_runs::DoorRuns>,
}

impl Runs {
    /// The turns, woken through `wake` when a run reports what it spent.
    fn new(wake: tilo_service::Wake) -> Self {
        Self {
            held: Mutex::default(),
            spent: Mutex::default(),
            reported: Mutex::default(),
            wake: Some(wake),
            doors: Arc::default(),
        }
    }

    /// The runs the agent door's connections work in.
    pub(crate) fn doors(&self) -> Arc<crate::door_runs::DoorRuns> {
        Arc::clone(&self.doors)
    }

    /// What the runs' agents spent since the last pass, by workspace and agent, in millionths of
    /// the currency, ready to be charged to their day.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        let Ok(mut spent) = self.spent.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *spent)
            .into_iter()
            .map(|((workspace, agent), cost)| (workspace, agent, cost))
            .collect()
    }

    /// Keep only the reports of the turns `held` names: a run that ended is charged for what it
    /// spent, and its total is no longer followed.
    fn prune_reported(&self, held: &Held) {
        let Ok(mut reported) = self.reported.lock() else {
            return;
        };
        reported.retain(|(workspace, issue, epoch), _| {
            held.workspaces
                .iter()
                .find(|facts| &facts.workspace == workspace)
                .is_some_and(|facts| {
                    facts
                        .turns
                        .iter()
                        .any(|entry| &entry.run.issue == issue && entry.run.epoch == *epoch)
                })
        });
    }

    fn workspace(&self, workspace: &str) -> Result<Facts, String> {
        self.held
            .lock()
            .map_err(|_| "this session's conversations are unavailable")?
            .workspace(workspace)
            .cloned()
    }
}

impl TrackerRuns for Runs {
    /// Only a workspace this session holds: its conversations are its own.
    fn requires_workspace(&self, workspace: &str) -> bool {
        self.held.lock().is_ok_and(|held| {
            held.workspaces
                .iter()
                .any(|facts| facts.workspace == workspace)
        })
    }

    /// An agent works only inside its turns, never on a workspace grant alone: what it may do is
    /// its person's grants inside the turn.
    fn confines_agents_to_tasks(&self, workspace: &str) -> bool {
        self.requires_workspace(workspace)
    }

    /// The turns, and the runs the door's connections work in.
    fn runs(&self, workspace: &str) -> Result<Vec<TrackerRun>, String> {
        let mut runs: Vec<TrackerRun> = self
            .workspace(workspace)?
            .turns
            .iter()
            .map(|entry| entry.run.clone())
            .collect();
        runs.extend(self.doors.in_workspace(workspace));
        Ok(runs)
    }

    /// Whether the agent still takes part in a conversation of this workspace: that is where it
    /// works, and an agent a conversation no longer takes in has no turn to answer. A door
    /// connection's agent is admitted while its connection holds its run, under the delegation
    /// that run carries.
    fn check_agent(&self, workspace: &str, run: &TrackerRun) -> Result<(), String> {
        if let Some((door, _)) = self.doors.find(workspace, &run.issue, &run.agent) {
            return if door.delegation == run.delegation {
                Ok(())
            } else {
                Err("the door connection's run is another agent's".into())
            };
        }
        let facts = self.workspace(workspace)?;
        if facts.agents.contains(&run.agent) {
            Ok(())
        } else {
            Err(format!(
                "the agent {} takes part in no conversation here",
                run.agent
            ))
        }
    }

    fn authorize_action(
        &self,
        workspace: &str,
        run: &TrackerRun,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        let facts = self.workspace(workspace)?;
        let policy = facts
            .policy
            .as_ref()
            .ok_or("this workspace's grants are not at hand")?;
        // A door connection's run has no message for a cause: the person admitting it is, and it
        // acts on their live grants capped at an agent's, and on what they answered it "always"
        // for while it goes.
        if let Some((door, answers)) = self.doors.find(workspace, &run.issue, &run.agent) {
            if answers.is_empty() {
                return requirement.check_tracker_policy(policy, workspace, &door, None, wall());
            }
            let mut answered = policy.clone();
            answered.grants.extend(answers);
            return requirement.check_tracker_policy(&answered, workspace, &door, None, wall());
        }
        let entry = facts
            .turns
            .iter()
            .find(|entry| entry.run.issue == run.issue && entry.run.agent == run.agent)
            .ok_or("the turn this op answers is no longer going")?;
        requirement.check_tracker_policy(policy, workspace, &entry.run, Some(&entry.looked), wall())
    }

    /// A door connection's run asks its person on this desk's sheet: no issue holds it. A turn's
    /// questions are its conversation's.
    fn asks_on_host(&self, workspace: &str, run: &TrackerRun) -> bool {
        self.doors.find(workspace, &run.issue, &run.agent).is_some()
    }

    fn keep_question(
        &self,
        workspace: &str,
        issue: &str,
        question: Option<tilo_broker_core::services::TrackerQuestion>,
    ) -> Result<(), String> {
        self.doors.keep_question(workspace, issue, question)
    }

    fn keep_answer(
        &self,
        workspace: &str,
        issue: &str,
        grant: tilo_action_policy::Grant,
    ) -> Result<(), String> {
        self.doors.keep_answer(workspace, issue, grant)
    }

    fn kept_answers(&self, workspace: &str, issue: &str) -> Vec<tilo_action_policy::Grant> {
        self.doors.answers(workspace, issue)
    }

    /// What a run spent, as its agent reported it: charged to the agent's day by the pass, and only
    /// for what it added to the total it reported before.
    fn reported(
        &self,
        workspace: &str,
        issue: &str,
        epoch: u64,
        update: &runtime_collab::TaskRunUpdate,
    ) {
        let runtime_collab::TaskRunUpdate::Spent { cost_micros } = update else {
            return;
        };
        let agent = self.held.lock().ok().and_then(|held| {
            held.workspaces
                .iter()
                .find(|facts| facts.workspace == workspace)
                .and_then(|facts| {
                    facts
                        .turns
                        .iter()
                        .find(|entry| entry.run.issue == issue && entry.run.epoch == epoch)
                })
                .map(|entry| entry.run.agent.clone())
        });
        let Some(agent) = agent else {
            // The turn is over: what it spent was charged when the pass read the report before it.
            return;
        };
        let added = {
            let Ok(mut reported) = self.reported.lock() else {
                return;
            };
            let key = (workspace.to_owned(), issue.to_owned(), epoch);
            let before = reported.insert(key, *cost_micros).unwrap_or_default();
            cost_micros.saturating_sub(before)
        };
        if added == 0 {
            return;
        }
        if let Ok(mut spent) = self.spent.lock() {
            let cost = spent.entry((workspace.to_owned(), agent)).or_default();
            *cost = cost.saturating_add(added);
        }
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

/// One workspace as the pass hands it over: the grants its turns are decided against.
pub(crate) struct WorkspaceFacts {
    pub(crate) workspace: String,
    pub(crate) policy: WorkspacePolicy,
}

/// The conversations this session's agents answer in, read from Chat's record of each workspace it
/// holds.
pub(crate) struct Conversations {
    runs: Arc<Runs>,
    /// The workspaces' records, as the workspace holds them.
    records: crate::engine_records::EngineRecords,
    /// Wakes the session's loop when a change to a record lands.
    wake: ui::WakeScope,
    /// Each workspace's records, opened here and followed change by change.
    open: RefCell<BTreeMap<String, Opened>>,
}

/// A workspace's records as they are open here, and what was read of them.
struct Opened {
    /// The grants they were opened with: a workspace whose grants changed has them opened again, so
    /// its decisions follow.
    policy: WorkspacePolicy,
    records: WorkspaceRecords,
    chat: Option<WorkspaceRecord<chat::Chat>>,
    /// The head of the messages' chain, and the part it holds here.
    part: Option<(u64, WorkspaceRecord<chat::parts::ChatPart>)>,
}

impl Conversations {
    /// The conversations in the workspaces' copy of them (`records`), waking the session's loop through `wake` when a
    /// change lands.
    pub(crate) fn new(
        records: crate::engine_records::EngineRecords,
        wake: tilo_service::Wake,
    ) -> Self {
        let mut scope = ui::WakeScope::new();
        let waking = Arc::clone(&wake);
        if let Err(error) = scope.bind(move |_| waking()) {
            tracing::warn!(%error, "the conversations are not woken");
        }
        Self {
            runs: Arc::new(Runs::new(wake)),
            records,
            wake: scope,
            open: RefCell::default(),
        }
    }

    /// The turns, as the broker reads them.
    pub(crate) fn runs(&self) -> Arc<Runs> {
        Arc::clone(&self.runs)
    }

    /// What the runs' agents spent since the last pass, by workspace and agent, in millionths of
    /// the currency, for their day to be charged.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        self.runs.take_spent()
    }

    /// The person seated here: whose changes what the service records are theirs, and the author
    /// a message or a record of work is written by when the caller names nobody else.
    #[must_use]
    pub fn person(&self) -> String {
        let open = self.open.borrow();
        open.values()
            .next()
            .map(|opened| opened.records.author().0.clone())
            .unwrap_or_default()
    }

    /// The workspace's agents by their id, as the workspace keeps their names: what a
    /// conversation with an agent is named for.
    fn agent_names(opened: &Opened) -> chat::projection::AgentNames {
        Self::workspace_agents(opened)
            .map(|record| record.get().active_names())
            .unwrap_or_default()
    }

    /// The workspace's agents record `opened` holds, opened here where it is not. `None` where the
    /// workspace keeps none — reading an app's record creates none, as [`Self::opened`] reads
    /// Chat's.
    fn workspace_agents(
        opened: &Opened,
    ) -> Option<WorkspaceRecord<tilo_app::tilo_workspace_agent::WorkspaceAgents>> {
        use tilo_app::tilo_workspace_agent::{KIND as AGENTS_KIND, OWNER as AGENTS_OWNER};
        let id = collab_types::workspace_record_id(AGENTS_KIND);
        let held = opened
            .records
            .holds_record(AGENTS_OWNER, &id, AGENTS_KIND)
            .unwrap_or(false);
        if !held {
            return None;
        }
        let standing = tilo_action_policy::Standing::person(
            opened.policy.clone(),
            opened.records.author().0.clone(),
        );
        let permissions = PermissionContext::from_standing(&opened.policy.workspace, standing)
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        WorkspaceRecord::<tilo_app::tilo_workspace_agent::WorkspaceAgents>::open(
            &opened.records,
            AGENTS_OWNER,
            CHAT_APP,
            Some(LivePermissions::fixed(permissions)),
            None,
        )
    }

    /// Record a turn's work through Chat's own `record-work` on the workspace's open Chat record,
    /// as `invoker` (the agent that answered, or the person here): `arguments` are the verb's,
    /// exactly as Chat's window's item takes them, including the `message` whose work it records —
    /// the summons, named as the item Chat's window publishes it names it
    /// (`chat.workspace.messages:message:<id>`) or by the bare id its item carries. Chat's own verb
    /// derives the reply from the summons and the author, so what it records lands on the very
    /// reply the turn's answer is — the address the agent works from, before the reply exists. The
    /// record is the one Chat's window writes and the pass reads turns from: no second writer, and
    /// no second verb.
    ///
    /// # Errors
    /// No open workspace holds the message `arguments` names, or Chat refused the record.
    fn record_work(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::DoorError> {
        let unknown = || {
            tilo_actions::DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource,
                RECORD_WORK,
            ))
        };
        // The message is the agent's argument, as Chat's window's item takes it; the item's own
        // id names it too, for a caller that names nothing.
        let named = arguments
            .get("message")
            .and_then(serde_json::Value::as_str)
            .and_then(message_of)
            .or_else(|| message_of(resource));
        let Some(message) = named else {
            return Err(unknown());
        };
        let work = arguments.get("work").cloned().ok_or_else(|| {
            tilo_actions::DoorError::InvalidArguments(
                "a record of work says how the work is going".to_owned(),
            )
        })?;
        let work: chat::record::WorkUpdate = serde_json::from_value(work).map_err(|error| {
            tilo_actions::DoorError::InvalidArguments(format!("the work does not read: {error}"))
        })?;
        let say = arguments
            .get("say")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let summons = Id::from_raw(message);
        let invoke =
            chat::parts::ChatPart::RECORD_WORK.with(chat::messages::chat_part::RecordWork {
                message: summons.clone(),
                work,
                say,
            });
        let open = self.open.borrow();
        for opened in open.values() {
            let Some((_, part)) = &opened.part else {
                continue;
            };
            if opened.chat.is_none() || part.get().messages.get(&summons).is_none() {
                continue;
            }
            return part
                .invoke_as(invoker, &invoke)
                .map_err(|error| tilo_actions::DoorError::Failed(error.to_string()));
        }
        Err(unknown())
    }

    /// Read each workspace's conversations as `person`, and hand the turns
    /// they give to the run channel.
    ///
    /// A pass that read different turns than the one before announces them
    /// ([`tilo_identity_host::policy_change::announce`]): the broker re-reads
    /// what it delivers when that changes, not on a clock, so a new turn — the
    /// person typing an ask into a conversation Chat had no message in — is
    /// only handed to its agent because this says it exists. Nothing is
    /// announced while the turns are the ones already read, so a quiet desk
    /// costs the broker no re-read.
    pub(crate) fn feed(&self, person: &str, workspaces: Vec<WorkspaceFacts>) {
        let read: Vec<Facts> = workspaces
            .into_iter()
            .map(|facts| {
                let (turns, agents) = self.turns(&facts, person);
                Facts {
                    workspace: facts.workspace,
                    agents,
                    policy: Some(facts.policy),
                    turns,
                }
            })
            .collect();
        let held = Held { workspaces: read };
        self.runs.prune_reported(&held);
        let changed = match self.runs.held.lock() {
            Ok(mut current) => {
                let changed = current.differs_from(&held);
                *current = held;
                changed
            }
            Err(error) => {
                // A poisoned run source keeps the turns it already held: this
                // pass's read was not stored, so announcing it would have the
                // broker re-read turns that are not there. The session's log
                // says the delivery stopped, rather than the ask failing
                // silently.
                tracing::warn!(%error, "the turns' run source is poisoned; this pass delivers nothing");
                false
            }
        };
        // Announced after the read is in place: the broker that wakes re-reads
        // what this just stored, never the pass before it.
        if changed {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// The turns `workspace` 's conversations give, read at the revisions its records are at now,
    /// and the agents those conversations take in.
    fn turns(&self, workspace: &WorkspaceFacts, person: &str) -> (Vec<Entry>, Vec<String>) {
        match self.opened(workspace, person) {
            Some((chat, part)) => {
                // A conversation with an agent is named for it, as the window names it: the
                // workspace's own record of its agents says what it is called.
                let names = self
                    .open
                    .borrow()
                    .get(&workspace.workspace)
                    .map(Self::agent_names)
                    .unwrap_or_default();
                let chat_record = chat.get();
                let part_record = part.get();
                turns(&chat_record, &part_record, person, &names)
            }
            None => (Vec::new(), Vec::new()),
        }
    }

    /// The workspace's chat record and the head of its messages, opened here where they are not.
    /// A workspace whose person never opened Chat holds no record, and the reader creates none.
    fn opened(
        &self,
        workspace: &WorkspaceFacts,
        person: &str,
    ) -> Option<(
        WorkspaceRecord<chat::Chat>,
        WorkspaceRecord<chat::parts::ChatPart>,
    )> {
        let mut open = self.open.borrow_mut();
        let id = workspace.workspace.clone();
        let stale = open
            .get(&id)
            .is_some_and(|opened| opened.policy != workspace.policy);
        if stale {
            open.remove(&id);
        }
        let opened = open.entry(id).or_insert_with(|| Opened {
            policy: workspace.policy.clone(),
            records: self
                .records
                .in_workspace(&workspace.workspace, UserId(person.to_owned())),
            chat: None,
            part: None,
        });
        let permissions = PermissionContext::from_standing(
            &workspace.workspace,
            tilo_action_policy::Standing::person(workspace.policy.clone(), person.to_owned()),
        )
        .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())));
        let chat_id = RecordId::package_primary();
        if opened.chat.is_none() {
            let held = opened
                .records
                .holds_record(CHAT_OWNER, chat_id.as_str(), chat::Chat::KIND)
                .unwrap_or(false);
            if !held {
                return None;
            }
            opened.chat = WorkspaceRecord::<chat::Chat>::open_primary(
                &opened.records,
                CHAT_OWNER,
                CHAT_APP,
                Some(LivePermissions::fixed(permissions.clone())),
                Some(self.wake.handle()),
            );
        }
        let chat = opened.chat.clone()?;
        let head = opened
            .records
            .store_of(CHAT_OWNER)
            .ok()?
            .held_parts(&chat::parts::chain())
            .into_iter()
            .max()?;
        if opened.part.as_ref().map(|(held, _)| *held) != Some(head) {
            let part = WorkspaceRecord::<chat::parts::ChatPart>::open_at(
                &opened.records,
                CHAT_OWNER,
                chat::parts::part(head).as_str(),
                CHAT_APP,
                Some(LivePermissions::fixed(permissions)),
                Some(self.wake.handle()),
            );
            opened.part = part.map(|part| (head, part));
        }
        let part = opened.part.as_ref().map(|(_, part)| part.clone())?;
        Some((chat, part))
    }
}

/// The turns going in the workspaces' conversations as a session service: each turn's summons, as
/// Chat publishes a message's item, carrying Chat's own `record-work`, so the agent answering it has
/// the address its answer goes to whether or not Chat's window is open. The verb runs on the
/// workspace's open Chat record, the same record the pass reads turns from — never a second writer
/// on the store.
pub(crate) struct TurnSummonses {
    conversations: Rc<Conversations>,
    /// The summonses it published last, keyed by each open chat's revision and messages' part:
    /// rebuilt only when a part moved or a workspace opened, so a pass that changed nothing
    /// writes no item again — and reads no part, which would cost the loop every frame.
    summonses: std::cell::RefCell<Option<(SummonsKey, Vec<tilo_actions::ResourceSpec>)>>,
}

/// What the summonses a [session's summonses](TurnSummonses) publishes depend on: each open
/// chat's revision, its messages' head part's revision, and the part's number.
type SummonsKey = (u64, u64, u64);

impl TurnSummonses {
    /// The service over `conversations`, the session's one open of the workspaces' chats.
    #[must_use]
    pub(crate) fn new(conversations: Rc<Conversations>) -> Self {
        Self {
            conversations,
            summonses: std::cell::RefCell::new(None),
        }
    }
}

impl tilo_service::Service for TurnSummonses {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let open = self.conversations.open.borrow();
        let mut specs: Vec<tilo_actions::ResourceSpec> = Vec::new();
        for opened in open.values() {
            let Some(chat_record) = &opened.chat else {
                continue;
            };
            let chat = chat_record.get();
            // Each turn's summons, as Chat publishes a message's item: the message a person
            // asked the agent in, which Chat's own `record-work` writes the answer to. The
            // agent's address *is* that answer's — derived from the summons and the agent
            // (`chat::record::reply_id`) before either exists — so the summons's item carries
            // the verb from the moment the turn does, never only once Chat's window publishes
            // the message. The item is Chat's own, published through `ChatPart`'s declaration,
            // and the verb runs on Chat's record: a verb reimplemented here would be a second
            // chat. It is unbound, as Chat's window's item is: the agent names the message whose
            // work it records, exactly as it does on a message's own item the window publishes.
            let Some((index, part)) = &opened.part else {
                continue;
            };
            let revision = (chat_record.revision(), part.revision(), *index);
            let kept = self
                .summonses
                .borrow()
                .as_ref()
                .and_then(|(at, items)| (*at == revision).then(|| items.clone()));
            let items = kept.unwrap_or_else(|| {
                let built: Vec<tilo_actions::ResourceSpec> = summonses(&chat, &part.get())
                    .into_iter()
                    .map(|summons| summons_item(&summons))
                    .collect();
                *self.summonses.borrow_mut() = Some((revision, built.clone()));
                built
            });
            specs.extend(items);
        }
        specs
    }

    /// A summons has no reading of its own: its message is Chat's to show.
    fn read(&self, _resource: &str) -> Option<serde_json::Value> {
        None
    }

    /// A record of work asked with no invoker is the person's here.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        let invoker = tilo_actions::ActionInvoker::actor(self.conversations.person());
        self.invoke_as(&invoker, resource, action, arguments)
    }

    /// A turn's reply records its work in Chat's own words, as `invoker` (the agent answering the
    /// turn, a script): the item is Chat's own, and the verb is run on Chat's record, written as
    /// the agent that sent it, exactly as Chat's window writes its reply as the agent (`chat`'s
    /// `author(cx)` names the invoker, and the reply is derived from it). Every other verb on a
    /// message is Chat's own, published by Chat, and refused here.
    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        if action != RECORD_WORK {
            return Err(tilo_actions::DoorError::Unknown(
                tilo_actions::ActionAddress::new(resource, action),
            ));
        }
        self.conversations
            .record_work(invoker, resource, arguments)
            .map(|()| tilo_service::Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The message `resource` names when it is a message's item or its id: `chat.workspace/messages/
/// <id>`, the item's bound form `chat.workspace.messages:message:<id>`, the kind-prefixed spelling
/// Chat's window publishes (`chat.messages/messages/<id>`), or the bare `<id>` an agent names it
/// by. Every spelling names the same message.
fn message_of(resource: &str) -> Option<String> {
    for prefix in [
        "chat.workspace/messages/",
        "chat.messages/messages/",
        "chat.workspace.messages:messages:",
        "chat.workspace.messages:message:",
    ] {
        if let Some(id) = resource.strip_prefix(prefix)
            && !id.is_empty()
            && !id.contains('/')
        {
            return Some(id.to_owned());
        }
    }
    // A bare message id, as an agent names the summons in its `message` argument.
    let bare = !resource.is_empty()
        && resource.len() <= 256
        && !resource.contains(['/', ':', '#'])
        && !resource.chars().any(char::is_whitespace);
    bare.then(|| resource.to_owned())
}

/// Every summons whose reply has no item Chat's window publishes yet: the person's messages an
/// agent in the conversation has not answered, and that no held message of the part answers. The
/// reply each names is the one its id and the agent's derive (`chat::record::reply_id`), so this
/// returns the summonses — the item the agent's verb is offered on — never the reply.
/// The summonses whose turn has no reply yet: the person's messages in a conversation an agent
/// takes part in, which no held message answers. The item each is published as carries Chat's
/// `record-work`, its message bound, so the agent's verb is offered on the address it answers from
/// before the reply the verb makes exists — the reply's own item is Chat's window's once it does.
/// The summons whose turn is going: the person's messages in a conversation an agent takes part
/// in, whose reply the agent has not finished — absent yet, or still working (no state that ends
/// it) — or whose reply the person asked again (`chat`'s `retry`, which gives the reply up). The
/// same rule the pass reads turns by, so the item that records the work is offered exactly while
/// the agent answers there: from the moment the turn exists, through every record it makes on the
/// reply it is writing, and again when a failed reply was retried.
fn summonses(chat: &chat::Chat, part: &chat::parts::ChatPart) -> Vec<Id> {
    let mut out: Vec<Id> = Vec::new();
    for (id, message) in part.messages.iter() {
        if message.text.trim().is_empty() || person_of(&message.author.0).is_none() {
            continue;
        }
        if chat
            .channels
            .get(&message.channel)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let agents: Vec<String> = chat
            .conversation_participants(&message.channel)
            .into_iter()
            .filter_map(|participant| match &participant {
                // The reply an agent's answer is derives from the participant key it writes as,
                // which spells the agent `agent:<id>` (`chat::record::reply_id`).
                chat::record::Participant::Agent(_) => Some(participant.key()),
                chat::record::Participant::Person(_) => None,
            })
            .collect();
        for agent in agents {
            // The reply of the attempt the message is in: a retry moved it on, so the reply that
            // failed stands as history and the item offers the next attempt's address.
            let reply = chat::record::reply_id_at(id, &agent, message.attempts);
            let going = part
                .messages
                .get(&reply)
                // A reply without work is a plain answer; one that keeps work is going until
                // its work ends. The same rule `turns` reads the turn by.
                .is_none_or(|reply| {
                    reply.work.as_ref().is_some_and(|work| {
                        let streamed = work
                            .stream
                            .as_ref()
                            .is_some_and(|stream| stream.finish.is_some());
                        !streamed
                            && work
                                .state
                                .as_ref()
                                .is_none_or(|transition| !transition.state.finished())
                    })
                });
            if going && !out.contains(id) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// The summons `id`, as Chat publishes a message's item: its id, its kind, its label, and Chat's
/// own `record-work` — the declaration `ChatPart::RECORD_WORK` publishes, whose action type is
/// Chat's, unbound so the agent names the message it records the work of as it does on Chat's
/// window's item. Built by [`tilo_resource::declaration_spec`], so the action an agent invokes is
/// Chat's verb, not a copy of it.
fn summons_item(id: &Id) -> tilo_actions::ResourceSpec {
    let kind = chat::parts::ChatPart::KIND;
    let mut spec = tilo_actions::ResourceSpec::new(
        format!("{kind}/messages/{id}"),
        "chat.messages",
        id.as_str(),
    )
    .with_type(tilo_actions::ResourceType {
        noun: tilo_actions::Noun::new("message", "messages"),
        app: tilo_actions::Noun::new("Chat", "Chat"),
        root: false,
    });
    spec.app_id = Some("chat".to_owned());
    let declaration = tilo_resource::vocabulary::<chat::parts::ChatPart>()
        .declarations()
        .iter()
        .find(|declaration| declaration.id == RECORD_WORK)
        .cloned();
    if let Some(declaration) = declaration {
        spec = spec.with_action(tilo_resource::declaration_spec(&declaration));
    }
    spec
}

/// The person a message's author names, when it names one: the participant key (`person:<root>`,
/// what a conversation's fields keep) or the bare id a record write attributes its change to
/// ([`ActionCx::author`](tilo_resource::action)). Both spell one person — the second is what Chat
/// writes a message's author as — so a message a person posted summons its conversation's agent
/// however it was written, and the run answers for the person it names.
fn person_of(author: &str) -> Option<String> {
    match chat::record::Participant::parse(author) {
        Some(chat::record::Participant::Person(root)) if !root.is_empty() => return Some(root),
        // A participant key naming an agent, or an empty person.
        Some(chat::record::Participant::Person(_) | chat::record::Participant::Agent(_)) => {
            return None;
        }
        None => {}
    }
    // A bare id: not a participant key at all, and no agent's either.
    (!author.is_empty()
        && !author.contains(':')
        && !author.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX))
    .then(|| author.to_owned())
}

/// The turns `chat` 's conversations give, read from `part` 's messages, and the agents the
/// conversations take in.
/// A conversation the pass has no message for is none: a direct conversation exists only as the id
/// its messages carry ([`chat::Chat::direct_id`]).
fn turns(
    chat: &chat::Chat,
    part: &chat::parts::ChatPart,
    person: &str,
    agent_names: &chat::projection::AgentNames,
) -> (Vec<Entry>, Vec<String>) {
    let mut conversations: Vec<Id> = Vec::new();
    for (_, message) in part.messages.iter() {
        if !conversations.contains(&message.channel) {
            conversations.push(message.channel.clone());
        }
    }
    let mut turns = Vec::new();
    let mut agents = Vec::new();
    for id in &conversations {
        // An archived conversation takes no message, and one the record does not list is a direct
        // conversation the record derives from its id.
        if chat
            .channels
            .get(id)
            .is_some_and(|conversation| conversation.archived)
        {
            continue;
        }
        let messages: Vec<(&Id, &chat::record::Message)> = part
            .messages
            .iter()
            .filter(|(_, message)| message.channel == *id)
            .collect();
        for participant in chat.conversation_participants(id) {
            let key = participant.key();
            let chat::record::Participant::Agent(agent) = participant else {
                continue;
            };
            if !agents.contains(&agent) {
                agents.push(agent.clone());
            }
            // The person's newest message the agent has not answered: the reply its id and the
            // agent's derive, which only the agent's answer to that message carries. An agent's
            // reply that is still working (its work is not finished) leaves the turn going, so a
            // runner that records "taken" before it acts is not read as having answered. A
            // message a person asked again (`chat`'s `retry`) is in a later attempt, whose reply
            // is absent yet: its turn goes again, and the answer is a reply of its own.
            let summons = messages
                .iter()
                .copied()
                .filter(|(message_id, message)| {
                    (!message.text.trim().is_empty() || !message.attachments.is_empty())
                        && person_of(&message.author.0).is_some()
                        && part
                            .messages
                            .get(&chat::record::reply_id_at(
                                message_id,
                                &key,
                                message.attempts,
                            ))
                            // A reply without work is a plain answer; one that keeps work is
                            // going until its work ends.
                            .is_none_or(|reply| {
                                reply.work.as_ref().is_some_and(|work| {
                                    let streamed = work
                                        .stream
                                        .as_ref()
                                        .is_some_and(|stream| stream.finish.is_some());
                                    !streamed
                                        && work
                                            .state
                                            .as_ref()
                                            .is_none_or(|transition| !transition.state.finished())
                                })
                            })
                })
                .max_by_key(|(message_id, message)| (message.ts, message_id.as_str()));
            let Some((message_id, message)) = summons else {
                continue;
            };
            let Some(starter) = person_of(&message.author.0) else {
                continue;
            };
            // The conversation's address in the workspace, as the desk and the apps name it, and
            // the message's own beneath it.
            let address = format!("{}/channels/{id}", chat::Chat::KIND);
            let (instruction, subject) = request(message);
            turns.push(Entry {
                run: TrackerRun {
                    issue: format!("{address}/messages/{message_id}"),
                    title: chat.display_names(id, Some(person), agent_names),
                    instruction,
                    agent: agent.clone(),
                    // The attempt the message is in, as the run's epoch: a person's Retry asks
                    // again for the same message, and that is a run of its own — the turn they
                    // gave up on reads as replaced. Without it the retried turn would read
                    // exactly like the one that failed (same message, same words, same agent),
                    // [`Held::differs_from`] would see no change to deliver, and the broker —
                    // which names a run by its issue and its epoch and hands an agent a run it
                    // named before only once — would give the agent nothing to answer.
                    epoch: u64::from(message.attempts) + 1,
                    starter,
                    active: true,
                    finished_by_agent: false,
                    paused_through: None,
                    budget: None,
                    subject,
                    model: None,
                    directions: Vec::new(),
                    question: None,
                    turn: Some(Turn::new(address, message_id.to_string())),
                    delegation: None,
                    origin: None,
                },
                looked: Looked::by(message.author.0.clone()),
            });
        }
    }
    (turns, agents)
}

/// What a message asks of the agents it summons: its words, and what it carries as the records
/// they name — the first of them the run's subject, which the agent acts on through that record's
/// own actions, and every other one said beside the words. A file or words quoted from nowhere are
/// said as what they are.
fn request(message: &chat::record::Message) -> (String, Option<tilo_actions::SelectionSubject>) {
    let mut subject = None;
    // A reply that answers a run of another message is asked with the words it quotes above its
    // own: the reply's quote is the conversation's record of what it means, so an answer to "yes,
    // that one" names what "that one" is without the agent opening the record it acts in.
    let mut instruction = message
        .quoted
        .as_ref()
        .filter(|quoted| !quoted.text.trim().is_empty())
        .map_or_else(
            || message.text.trim().to_owned(),
            |quoted| format!("> {}\n\n{}", quoted.text.trim(), message.text.trim()),
        );
    for attached in &message.attachments {
        let named = attached.subject();
        if subject.is_none() && named.is_some() {
            subject = named;
            continue;
        }
        let line = match (named, attached) {
            (Some(named), _) => format!(
                "It also carries {:?} ({} `{}`, in `{}`).",
                attached.label(),
                named.resource.kind,
                named.resource.id,
                named.app_id
            ),
            (None, tilo_reference::Attached::Text { text, .. }) => {
                format!("It quotes: {text:?}.")
            }
            (None, _) => format!("It carries the file {:?}.", attached.label()),
        };
        if !instruction.is_empty() {
            instruction.push('\n');
        }
        instruction.push_str(&line);
    }
    (instruction, subject)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Atom;

    /// The agent the tests' conversations take in.
    const AGENT: &str = "tide";
    /// The id its words are written under.
    const AGENT_ACTOR: &str = "agent:tide";
    /// The person sitting here.
    const ME: &str = "person:me";
    /// The conversation the two of them have.
    const CONVERSATION: &str = "c_tide";

    /// A workspace holding one conversation between `ME` and `AGENT` .
    fn workspace() -> chat::Chat {
        let mut chat = chat::Chat::default();
        chat.channels.push(
            Id::from_raw(CONVERSATION),
            chat::record::Conversation {
                name: "Tide".into(),
                participants: vec![Atom(ME.into()), Atom(AGENT_ACTOR.into())],
                ..chat::record::Conversation::default()
            },
        );
        chat
    }

    /// A message `author` wrote in `CONVERSATION` at `ts` .
    fn message(author: &str, text: &str, ts: i64) -> chat::record::Message {
        chat::record::Message {
            channel: Id::from_raw(CONVERSATION),
            author: Atom(author.into()),
            ts,
            text: text.into(),
            ..chat::record::Message::default()
        }
    }

    /// The head part of the messages, holding `messages` .
    fn part(messages: Vec<(&str, chat::record::Message)>) -> chat::parts::ChatPart {
        let mut part = chat::parts::ChatPart::default();
        for (id, message) in messages {
            part.messages.push(Id::from_raw(id), message);
        }
        part
    }

    /// The turns `chat` 's conversations give for the messages of `head` .
    fn read(chat: &chat::Chat, head: &chat::parts::ChatPart) -> Vec<Entry> {
        turns(chat, head, ME, &chat::projection::AgentNames::new()).0
    }

    #[test]
    fn an_unanswered_message_is_a_turn_of_the_conversations_agent() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.agent, AGENT);
        assert_eq!(entry.run.starter, "me");
        assert_eq!(entry.run.instruction, "add the revenue curve");
        assert_eq!(entry.run.title, "Tide");
        assert_eq!(
            entry.run.turn,
            Some(Turn::new("chat.workspace/channels/c_tide", "m1"))
        );
        assert_eq!(entry.looked, Looked::by(ME));
        assert!(entry.run.active);
    }

    /// What a send-to box carried reaches the agent as the record it names: the run's subject is
    /// that record, to act on through its own actions, and a message carrying it with no words is
    /// a turn too.
    #[test]
    fn what_a_message_carries_is_the_subject_of_its_turn() {
        let chat = workspace();
        let issue = tilo_reference::ResourceAt {
            app: "so.tilo.tasks".into(),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                "tasks.issue",
                "tasks.tracker/issues/42",
                "Launch checklist",
            ),
        };
        let mut handed = message(ME, "", 10);
        handed.attachments = vec![
            tilo_reference::Attached::resource(
                issue,
                Some(tilo_reference::Anchor::item("42")),
                "Launch checklist",
            ),
            tilo_reference::Attached::Text {
                text: "before Friday".into(),
                from: None,
            },
        ];
        let entries = read(&chat, &part(vec![("m1", handed)]));
        let [entry] = entries.as_slice() else {
            panic!("a message carrying a task alone is a turn, not {entries:?}");
        };
        let subject = entry.run.subject.as_ref().expect("the task is the subject");
        assert_eq!(subject.app_id, "so.tilo.tasks");
        assert_eq!(subject.resource.id, "tasks.tracker/issues/42");
        assert_eq!(
            subject.anchor.place,
            Some(tilo_actions::SelectionPlace::item("42"))
        );
        assert_eq!(entry.run.instruction, "It quotes: \"before Friday\".");
    }

    /// A message Chat writes names its author by the bare id the record write
    /// gives, not by the `person:<root>` key its conversation keeps: both spell
    /// one person, so a message a person posts in Chat summons its agent as
    /// surely as one written under the key.
    #[test]
    fn a_message_written_by_a_bare_person_id_is_a_turn_too() {
        let bare = "22503e79b0b80274014ec6333d3561ca89cb3209937b901c4bc2e4bedccc2f85";
        let chat = workspace();
        let head = part(vec![("m1", message(bare, "what is on my task list?", 10))]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.agent, AGENT);
        assert_eq!(entry.run.starter, bare, "the run acts for that person");
        assert_eq!(entry.run.instruction, "what is on my task list?");
        assert!(
            entry.run.active,
            "the person's message waits for its answer"
        );
        assert_eq!(
            person_of("agent-task:22222222-2222-4222-8222-222222222222"),
            None,
            "an agent's message is no person's turn"
        );
    }

    #[test]
    fn the_agents_reply_ends_its_turn() {
        let chat = workspace();
        let reply = chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR);
        let mut head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        head.messages.push(reply, message(AGENT_ACTOR, "done", 20));
        assert_eq!(read(&chat, &head), Vec::new());
    }

    /// A reply whose turn failed and that the person asked again no longer answers: the same
    /// message is a turn again, in a later attempt whose reply is its own.
    #[test]
    fn a_retried_reply_gives_its_summons_a_turn_again() {
        let chat = workspace();
        let mut asked = message(ME, "add the revenue curve", 10);
        let mut head = part(vec![("m1", asked.clone())]);
        let mut failed = message(AGENT_ACTOR, "", 20);
        failed.parent = Some(Id::from_raw("m1"));
        failed.work = Some(chat::record::Work {
            state: Some(chat::record::Transition {
                state: chat::model::AgentWorkState::ProviderFinished {
                    outcome: chat::agent_stream::AgentFinish::Failed,
                },
                since: 20,
            }),
            ..chat::record::Work::default()
        });
        head.messages.push(
            chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR),
            failed,
        );
        assert_eq!(
            read(&chat, &head),
            Vec::new(),
            "a failed reply ends the turn until the person asks again"
        );
        // The retry moved the question to its next attempt: its reply is the one derived for
        // that attempt, and nobody has written it.
        asked.attempts = 1;
        head.messages.push(Id::from_raw("m1"), asked.clone());
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("the retried summons is a turn again: {entries:?}");
        };
        assert_eq!(entry.run.instruction, "add the revenue curve");
        assert_eq!(
            entry.run.turn,
            Some(Turn::new("chat.workspace/channels/c_tide", "m1")),
            "the turn is the same message, never a second question"
        );
        assert_eq!(
            summonses(&chat, &head),
            vec![Id::from_raw("m1")],
            "the summons's item offers `record-work` again, so the agent's new answer lands"
        );
        // The answer lands in the attempt's own reply, never the one that failed.
        let next = chat::record::reply_id_at(&Id::from_raw("m1"), AGENT_ACTOR, asked.attempts);
        assert_ne!(
            next,
            chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR),
            "a retried turn is answered in a reply of its own"
        );
    }

    /// A retried turn is a run of its own, not the one that failed read again: its epoch is the
    /// attempt the message is in. That is what makes it deliverable — the pass announces a change
    /// only when the turns differ ([`Held::differs_from`]), and the broker derives the run's own
    /// id from the issue and its epoch, so a turn that read exactly like the failed one would be
    /// handed to nobody.
    #[test]
    fn a_retried_turn_is_a_new_run_of_the_same_message() {
        let chat = workspace();
        let asker = message(ME, "add the revenue curve", 10);
        let mut head = part(vec![("m1", asker.clone())]);
        // The turn going before the answer was tried: the run the agent was handed first.
        let first = read(&chat, &head);
        let [before] = first.as_slice() else {
            panic!("the ask is a turn: {first:?}");
        };
        assert_eq!(before.run.epoch, 1, "the first attempt is the first run");
        // The try failed: the failed reply ends the turn.
        let mut failed = message(AGENT_ACTOR, "", 20);
        failed.parent = Some(Id::from_raw("m1"));
        failed.work = Some(chat::record::Work {
            state: Some(chat::record::Transition {
                state: chat::model::AgentWorkState::ProviderFinished {
                    outcome: chat::agent_stream::AgentFinish::Failed,
                },
                since: 20,
            }),
            ..chat::record::Work::default()
        });
        head.messages.push(
            chat::record::reply_id(&Id::from_raw("m1"), AGENT_ACTOR),
            failed,
        );
        assert_eq!(read(&chat, &head), Vec::new());
        // The person pressed Retry: the same message is a turn again, and it is a new run.
        let mut retried = asker;
        retried.attempts = 1;
        head.messages.push(Id::from_raw("m1"), retried);
        let again = read(&chat, &head);
        let [after] = again.as_slice() else {
            panic!("the retried summons is a turn again: {again:?}");
        };
        assert_eq!(
            after.run.issue, before.run.issue,
            "the retried run is the same message's"
        );
        assert_eq!(
            after.run.epoch, 2,
            "the retried run is the attempt the message moved to"
        );
        assert_ne!(
            *after, *before,
            "the retried turn differs from the one that failed, so the pass announces it"
        );
        // And the pass reads that as a change to deliver, exactly as it does a new turn.
        let facts = |turns: Vec<Entry>| Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents: vec![AGENT.to_owned()],
                policy: None,
                turns,
            }],
        };
        assert!(
            facts(again).differs_from(&facts(first)),
            "the retry is a change the broker is woken for"
        );
    }

    #[test]
    fn the_newest_unanswered_message_is_the_turn() {
        let chat = workspace();
        let head = part(vec![
            ("m1", message(ME, "first", 10)),
            ("m2", message(ME, "second", 20)),
        ]);
        let entries = read(&chat, &head);
        let [entry] = entries.as_slice() else {
            panic!("one turn, not {entries:?}");
        };
        assert_eq!(entry.run.instruction, "second");
    }

    #[test]
    fn an_agents_own_words_are_no_summons() {
        let chat = workspace();
        let head = part(vec![("m1", message(AGENT_ACTOR, "on it", 10))]);
        assert_eq!(read(&chat, &head), Vec::new());
    }

    #[test]
    fn a_channel_of_the_whole_workspace_summons_no_agent() {
        let mut chat = workspace();
        chat.channels.push(
            Id::from_raw("c_general"),
            chat::record::Conversation {
                name: "general".into(),
                ..chat::record::Conversation::default()
            },
        );
        let head = part(vec![(
            "m1",
            chat::record::Message {
                channel: Id::from_raw("c_general"),
                ..message(ME, "hello all", 10)
            },
        )]);
        assert_eq!(read(&chat, &head), Vec::new());
    }

    #[test]
    fn a_runs_spend_is_charged_to_its_agent_for_what_each_report_added() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME, &chat::projection::AgentNames::new());
        let issue = entries[0].run.issue.clone();
        let epoch = entries[0].run.epoch;
        let agent = entries[0].run.agent.clone();
        let runs = Runs::new(std::sync::Arc::new(|| {}));
        *runs.held.lock().unwrap() = Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents,
                policy: None,
                turns: entries,
            }],
        };
        let spent = |cost_micros| runtime_collab::TaskRunUpdate::Spent { cost_micros };

        runs.reported("w1", &issue, epoch, &spent(420_000));
        assert_eq!(
            runs.take_spent(),
            vec![("w1".to_owned(), agent.clone(), 420_000)]
        );
        assert_eq!(runs.take_spent(), Vec::new(), "a charge is taken once");

        runs.reported("w1", &issue, epoch, &spent(420_000));
        assert_eq!(
            runs.take_spent(),
            Vec::new(),
            "a repeated total adds nothing"
        );

        runs.reported("w1", &issue, epoch, &spent(900_000));
        assert_eq!(
            runs.take_spent(),
            vec![("w1".to_owned(), agent, 480_000)],
            "the run's total, not the last call's, is what its agent spent"
        );

        runs.reported("w2", &issue, epoch, &spent(100));
        assert_eq!(
            runs.take_spent(),
            Vec::new(),
            "a turn this session does not hold is not charged"
        );
    }

    #[test]
    fn the_run_channel_knows_only_the_workspaces_it_read() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME, &chat::projection::AgentNames::new());
        let runs = Runs::new(std::sync::Arc::new(|| {}));
        *runs.held.lock().unwrap() = Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents,
                policy: None,
                turns: entries,
            }],
        };
        assert!(runs.requires_workspace("w1"));
        assert!(!runs.requires_workspace("w2"));
        assert_eq!(runs.runs("w1").unwrap().len(), 1);
        assert!(runs.runs("w2").is_err());
        let mut run = runs.runs("w1").unwrap().remove(0);
        assert!(runs.check_agent("w1", &run).is_ok());
        run.agent = "other".into();
        assert!(runs.check_agent("w1", &run).is_err());
    }

    /// A pass that read different turns than the one before is what wakes the
    /// broker to deliver a new one: the person typing an ask into a
    /// conversation Chat held no message in is only handed to its agent
    /// because [`Held::differs_from`] says the turns changed.
    #[test]
    fn a_new_turn_is_a_change_and_the_same_turns_are_not() {
        let chat = workspace();
        let asked = part(vec![("m1", message(ME, "what is on my task list?", 10))]);
        let (entries, agents) = turns(&chat, &asked, ME, &chat::projection::AgentNames::new());
        let facts = |turns: Vec<Entry>| Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents: agents.clone(),
                policy: None,
                turns,
            }],
        };
        let nothing = Held::default();
        let waiting = facts(entries.clone());
        assert!(
            nothing.differs_from(&waiting),
            "a turn that was not there is a change to deliver"
        );
        assert!(
            !waiting.differs_from(&facts(entries)),
            "the same turns delivered again are no change"
        );
        assert!(
            facts(Vec::new()).differs_from(&waiting),
            "a turn that ended is a change too: its run stops"
        );
    }

    /// An agent the person admitted at the door works in a run of its own beside the turns: the
    /// broker reads it with them, admits its agent under the delegation it carries and no other,
    /// and it ends with its connection.
    #[test]
    fn a_door_connections_run_goes_beside_the_turns_while_it_is_held() {
        let chat = workspace();
        let head = part(vec![("m1", message(ME, "add the revenue curve", 10))]);
        let (entries, agents) = turns(&chat, &head, ME, &chat::projection::AgentNames::new());
        let runs = Runs::new(std::sync::Arc::new(|| {}));
        *runs.held.lock().unwrap() = Held {
            workspaces: vec![Facts {
                workspace: "w1".into(),
                agents,
                policy: None,
                turns: entries,
            }],
        };
        let door = TrackerRun {
            issue: format!("{}a1", crate::door_runs::DOOR_RUNS),
            agent: "task-0123456789ab".into(),
            turn: None,
            delegation: Some(Ok(vec![1, 2, 3])),
            ..runs.runs("w1").unwrap().remove(0)
        };
        let going = runs.doors().open("w1", door.clone());
        let listed = runs.runs("w1").unwrap();
        assert_eq!(listed.len(), 2, "the turn and the door's run: {listed:?}");
        assert!(listed.contains(&door));
        assert!(
            runs.check_agent("w1", &door).is_ok(),
            "its agent is admitted"
        );
        let forged = TrackerRun {
            delegation: Some(Ok(vec![9])),
            ..door.clone()
        };
        assert!(
            runs.check_agent("w1", &forged).is_err(),
            "under its own delegation alone"
        );
        drop(going);
        assert_eq!(
            runs.runs("w1").unwrap().len(),
            1,
            "it ends with its connection"
        );
        assert!(runs.check_agent("w1", &door).is_err());
    }
}
