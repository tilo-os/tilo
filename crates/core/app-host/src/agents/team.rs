//! What a turn on this server does with its own team: the sub-agents its agent started.
//!
//! A person's own agent starts sub-agents, reads how they stand and steers them while they
//! work, and a sub-agent does the same for a team of its own, as deep as its person allows
//! ([`tilo_workspace_agent::sub`]). The workspace's list of its agents holds all of it and
//! guards it: who may start one, how deep, how many at once, and who steers which. That list
//! is Assistant's, and a sub-agent's turn reads nothing Chat or the list keeps, so a turn does
//! not reach its team through either. [`TurnTeam`] is this server's own answer for the one turn
//! it serves, a provider of the turn's own ([`TurnProvider`]):
//!
//! - **Three verbs**, the list's own by name: `start-sub-agent`, `note-sub-agent` and
//!   `stop-sub-agent`. Each is applied on the list as the turn's agent acting for the person
//!   who keeps it, the way this server writes how a sub-agent stands. So the list's rules
//!   decide, and what it refuses is what the turn's tools are told. Nothing more is needed to
//!   start one: the turn tells the sub-agent its job at once, and each note later, in a message
//!   that mentions it beside the turn's reply, which its trigger fires on
//!   ([`super::sub_agents::tell`]).
//! - **One read**, the team: the sub-agents the turn's agent started and theirs, each with
//!   how it stands, since when, its line and what it made ([`WorkspaceAgents::team_of`]).
//!   Nothing of whoever started the turn's agent is in the answer, and nothing of a
//!   sub-agent beside it, so the one-way rule stays whole: a lead reads down, never up.
//!
//! Whom it answers: a turn of the agent this server carries out, whichever member asked it
//! ([`Workspaces::asked_turn`]). Anything that is no turn is answered what an unknown action
//! is. That check is never the whole rule. The list's verbs are Assistant's, so each is also
//! decided by the rules the turn carries, at the place the list's own verb of that name is
//! decided at: what the agent's entry bounds it to holds here, and a sub-agent keeps exactly
//! the verbs of its own team ([`tilo_workspace_agent::sub::one_way`]). Reading a team is part
//! of steering it and is decided as a note is. And the list is the workspace's own document,
//! which a member changes as an editor: a member who is less than that here starts and steers
//! nothing through an agent they ask, and what the list records is written as the agent acting
//! for them.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Context, Grantee, Party, Role, TreePath};
use tilo_actions::remote::ActionCallError as NotTaken;
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest,
    Pending, RemoteActionTarget,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    InvocationUpdate, ParamType,
};
use tilo_resource::{Id, Invoke, NonBlank};
use tilo_workspace_agent::workspace_agents::{NoteSubAgent, StartSubAgent, StopSubAgent};
use tilo_workspace_agent::{Agent, KIND, NewSubAgent, OWNER, WorkspaceAgents, sub};

use super::turn_provider::{Ending, Rows};
use super::{Turn, TurnProvider, TurnScope, WorkspaceId};
use tilo_workspace_service::workspaces::Workspaces;

/// The provider a turn's tools find its team under.
const PROVIDER: &str = "tilo.sub-agents";
/// The one resource it holds: the team of the turn's agent.
const TEAM: &str = "team";
/// The read of the team.
const READ: &str = "read";
/// The list's own verbs, by the names the list gives them.
const START: &str = "start-sub-agent";
const NOTE: &str = "note-sub-agent";
const STOP: &str = "stop-sub-agent";
/// The code of a call the list refused: its words say why.
const REFUSED: &str = "refused";
/// The most of a job the read shows, in characters: the whole of it is what its sub-agent was
/// asked first.
const JOB_CHARS: usize = 160;

/// What an agent is told about its team where its turn is offered one, after everything else
/// it is told.
const TOLD: &str = "You can hand parts of your work to sub-agents. Each works on the one job \
     you give it, on its own, and cannot read your conversation: its job says everything it \
     needs. `sub-agents/team#start-sub-agent` starts one (a name, the job and, if you like, a \
     role and the kind of work that picks its model), and you are asked again with its answer \
     when its work ends. Read `sub-agents/team` for how each one stands and what it made; \
     `sub-agents/team#note-sub-agent` tells one something more while it works, and \
     `sub-agents/team#stop-sub-agent` stops one. Do small things yourself.";

/// What `turn`'s agent is told about a team of its own: said where it may start one, so to a
/// workspace's agent whose entry lets it, and to a sub-agent that does not stand as deep as
/// sub-agents go under that agent.
pub(super) fn told(turn: &Turn) -> String {
    let deepest = |root: &Agent| root.sub_depth.unwrap_or(sub::DEFAULT_DEPTH);
    let may_start = match (&turn.agent.sub, &turn.line) {
        (None, _) => deepest(&turn.agent) > 0,
        (Some(own), Some(line)) => own.depth < deepest(&line.root),
        (Some(_), None) => false,
    };
    if may_start {
        format!("\n\n{TOLD}")
    } else {
        String::new()
    }
}

/// Make `scope`'s service answer as well for the team of `turn`'s agent.
pub(super) async fn join(workspaces: &Workspaces, turn: &Turn, scope: &mut TurnScope) {
    let team = TurnTeam::for_turn(workspaces, turn, scope).await;
    let service = &mut scope.provider;
    *service = service.clone().answering(team);
}

/// What one turn does with the team of its agent.
struct TurnTeam {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The turn's agent, as the list held it when the turn began.
    agent: Agent,
    /// Where the turn tells its team, for a turn asked in a conversation: what it answers there,
    /// and as whom. A sub-agent it starts is told its job there, and each note.
    telling: Option<(
        super::WorkspaceActionProvider,
        Caller,
        tilo_workspace_service::work::Where,
    )>,
    /// What the turn is offered: nothing for a caller that is no turn of this agent.
    offered: Vec<CatalogueRow>,
}

/// The sub-agent a note or a stop names.
#[derive(Deserialize)]
struct Named {
    agent: String,
}

/// What a note says.
#[derive(Deserialize)]
struct Noted {
    text: String,
}

impl TurnTeam {
    /// What `turn` does with its agent's team, where `scope` holds whom its tools act for.
    async fn for_turn(workspaces: &Workspaces, turn: &Turn, scope: &TurnScope) -> Arc<Self> {
        let mut team = Self {
            workspaces: workspaces.clone(),
            workspace: scope.workspace.clone(),
            workspace_id: scope.workspace_id,
            agent: turn.agent.clone(),
            telling: None,
            offered: Vec::new(),
        };
        let reply = scope.reply(turn);
        team.telling = turn
            .ask
            .message()
            .map(|place| (reply.provider.clone(), reply.caller.clone(), place.clone()));
        let caller = reply.caller;
        if team.on_turn(&caller).await.is_ok() {
            team.offered = rows()
                .into_iter()
                .filter(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| team.lets(&caller, &action.address.action, None))
                })
                .collect();
        }
        Arc::new(team)
    }

    /// That `caller` is on a turn of this agent, whichever member asked it: the first check of
    /// everything here. Every other caller is answered what an unknown action is.
    async fn on_turn(&self, caller: &Caller) -> Result<(), NotTaken> {
        let asked = self
            .workspaces
            .asked_turn_of(self.workspace_id, caller)
            .await?;
        if asked.agent == self.agent.member() {
            Ok(())
        } else {
            Err(NotTaken::UnknownAction)
        }
    }

    /// Whether the rules `caller`'s turn carries let it take `verb`, on the sub-agent `on`
    /// where the verb names one. Decided where the list's own verb of that name is decided,
    /// so what the agent's entry bounds it to holds here, and a sub-agent keeps the verbs of
    /// its own team and no other. Reading a team is part of steering it: it is decided as a
    /// note is.
    fn lets(&self, caller: &Caller, verb: &str, on: Option<&str>) -> bool {
        let (Some((agent, own)), Some(person)) = (
            caller
                .acting()
                .member_id()
                .and_then(tilo_action_policy::MemberId::as_agent),
            caller.acting().acted_for_person(),
        ) else {
            return false;
        };
        let list = TreePath::workspace(self.workspace.as_str()).app(OWNER);
        let node = if verb == START {
            list.kind(KIND).resource(KIND).action(START)
        } else {
            // One sub-agent of the list: the one named, or for what is offered before any is
            // named, the turn's own agent.
            let item = on.unwrap_or(self.agent.id.0.as_str());
            list.kind(format!("{KIND}.agents"))
                .resource(format!("{KIND}/agents/{item}"))
                .action(if verb == READ { NOTE } else { verb })
        };
        // Only a turn this server carries out reaches here ([`Self::asker`]): its agent works
        // for the turn's person as one handed to them, held to what the list keeps for it.
        let grants = tilo_action_policy::Acting::agent(agent, own.clone())
            .handed()
            .acting_for(tilo_action_policy::Acting::member(
                tilo_action_policy::MemberId::person(person),
            ))
            .handed_preset(&self.workspace);
        // Its grants hold until they are taken back: no time decides them.
        let decided = Party::new(vec![Grantee::agent(agent, own.clone())], &grants).decide(
            &node,
            tilo_action_policy::ActionKind::Undoable,
            &Context::at(0),
        );
        decided.answer == Answer::Allow
    }

    /// Whether every person `caller`'s turn is decided for changes the workspace's own
    /// documents as a member: an editor or more, admitted now. That is the person who asked
    /// and, where the agent belongs to someone else, that person too: the turn does no more
    /// than both may.
    async fn people_edit(&self, caller: &Caller) -> bool {
        let Ok((_, kept)) = self.workspaces.kept_membership(&self.workspace).await else {
            return false;
        };
        let people = caller.acting().people();
        !people.is_empty()
            && people.iter().all(|person| {
                kept.get(*person).is_some_and(|mark| {
                    !mark.removed
                        && !mark.banned
                        && Role::parse(&mark.role).is_some_and(|role| role.includes(Role::Editor))
                })
            })
    }

    /// The workspace's list of its agents, as the workspace keeps it now.
    async fn list(&self) -> Result<WorkspaceAgents, String> {
        let document = collab_types::workspace_document_id(KIND);
        let bytes = self
            .workspaces
            .resource_document(&self.workspace, tilo_workspace_agent::OWNER, &document)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("the workspace keeps no agents")?;
        let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        crate::agents::opened::<WorkspaceAgents>(&document, &mut doc)
    }

    /// Apply `invoke` on the list as `caller`'s turn acts: its agent, for the person who asked,
    /// bounded by the person the agent belongs to where that is someone else. The list's own
    /// rules decide, and what it refuses is the error.
    async fn on_list(
        &self,
        invoke: Invoke<WorkspaceAgents>,
        caller: &Caller,
    ) -> Result<(), String> {
        let acting = caller.acting().clone();
        self.workspaces
            .author_agents(&self.workspace, |doc| {
                let mut list =
                    tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                        .as_acting(acting)
                        .in_app(tilo_workspace_agent::OWNER, "Workspace");
                // A replica stamps what it writes with a fixed day of its own unless told the time.
                list.set_now_ms(tilo_clock::unix_now_ms_i64());
                let applied = list
                    .apply(invoke)
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, list.doc_mut());
                applied
            })
            .await
    }

    /// What the turn's agent reads of the sub-agent `id` of its team, as the list holds it
    /// now; nothing for one that is not of its team.
    async fn one(&self, id: &str) -> Result<Value, String> {
        let list = self.list().await?;
        Ok(list
            .team_of(&self.agent)
            .into_iter()
            .find(|member| member.id.0 == id)
            .and_then(|member| self.shown(member))
            .unwrap_or_else(|| json!({"agent": id})))
    }

    /// What the turn's agent reads of `member`, one of its team.
    fn shown(&self, member: &Agent) -> Option<Value> {
        let of = member.sub.as_ref()?;
        let above = self.agent.sub.as_ref().map_or(0, |own| own.depth);
        let job: String = of
            .job
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(JOB_CHARS)
            .collect();
        let made: Vec<Value> = of
            .produced
            .iter()
            .map(|made| {
                json!({
                    "kind": made.kind,
                    "label": made.label,
                    "url": made.url,
                    "at": made.at,
                })
            })
            .collect();
        Some(json!({
            "agent": member.id.0,
            "name": member.name,
            "role": of.role,
            "job": job,
            "started_by": of.started_by,
            // How far below the reader it stands: 1 for one the reader started itself.
            "depth": of.depth.saturating_sub(above),
            "stands": of.stands,
            "since": of.since,
            "line": of.line,
            "produced": made,
        }))
    }

    /// The team of the turn's agent: the sub-agents it started and theirs.
    async fn team(&self) -> Result<Value, String> {
        let list = self.list().await?;
        let team: Vec<Value> = list
            .team_of(&self.agent)
            .into_iter()
            .filter_map(|member| self.shown(member))
            .collect();
        Ok(json!({"team": team}))
    }

    /// Start the sub-agent `given` asks for.
    async fn start(
        &self,
        given: &Map<String, Value>,
        caller: &Caller,
    ) -> Result<Result<Value, String>, NotTaken> {
        let mut new: NewSubAgent = read_as(given)?;
        // Named here where the starter gave no id, so it is told the id at once.
        let id = match new.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => id.to_owned(),
            None => match self.list().await {
                Ok(list) => list.free_id(&new.name),
                Err(why) => return Ok(Err(why)),
            },
        };
        new.id = Some(id.clone());
        let started = self
            .on_list(
                WorkspaceAgents::START_SUB_AGENT.with(StartSubAgent { sub_agent: new }),
                caller,
            )
            .await;
        if let Err(why) = started {
            return Ok(Err(why));
        }
        // Its job is told at once, in a message that mentions it: its trigger starts its turn.
        let name = given
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned();
        let job = given
            .get("job")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Err(error) = self.tell(&id, &name, &job).await {
            tracing::warn!(workspace = %self.workspace, agent = %id, %error, "a sub-agent was not told its job");
        }
        Ok(self.one(&id).await)
    }

    /// Tell the sub-agent `id` (called `name`) `text` beside the turn's reply, in a message that
    /// mentions it. A turn asked outside a conversation has nowhere to tell it.
    ///
    /// # Errors
    /// Chat refused the message, or the turn answers in no conversation.
    async fn tell(&self, id: &str, name: &str, text: &str) -> Result<(), String> {
        let Some((provider, caller, place)) = &self.telling else {
            return Err("only a turn asked in a conversation tells a sub-agent".to_owned());
        };
        let member = self
            .list()
            .await?
            .get(&Id::from_raw(id.to_owned()))
            .map(Agent::member)
            .ok_or("the workspace keeps no such sub-agent")?;
        let rich_text = self
            .workspaces
            .tilolang()
            .map(|tilolang| tilolang.rich_text)
            .ok_or("this daemon runs no tilolang runtime: it writes no rich text")?;
        super::sub_agents::tell(
            (provider, caller),
            &*rich_text,
            &self.workspace,
            place,
            (&member, name),
            text,
        )
        .await
    }

    /// Take the call `request` asks for as `caller`: how it ended, or why this server did
    /// not take it.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, NotTaken> {
        let invocation = request.invocation;
        self.on_turn(caller).await?;
        let address = &request.target.address;
        let verb = address.action.as_str();
        if address.resource != TEAM || ![READ, START, NOTE, STOP].contains(&verb) {
            return Err(NotTaken::UnknownAction);
        }
        let none = Map::new();
        let given = match &request.argument {
            InvokeArgument::Json(given) => given,
            _ => &none,
        };
        let done = if verb == READ {
            if !self.lets(caller, READ, None) {
                return Err(NotTaken::NotPermitted);
            }
            self.team().await
        } else {
            let named = if verb == START {
                None
            } else {
                Some(read_as::<Named>(given)?.agent)
            };
            if !self.lets(caller, verb, named.as_deref()) || !self.people_edit(caller).await {
                return Err(NotTaken::NotPermitted);
            }
            match (verb, named) {
                (START, _) => self.start(given, caller).await?,
                (NOTE, Some(agent)) => match NonBlank::parse(&read_as::<Noted>(given)?.text) {
                    Some(text) => {
                        let said = text.to_string();
                        let noted = WorkspaceAgents::NOTE_SUB_AGENT.with(NoteSubAgent {
                            agent: Id::from_raw(agent.clone()),
                            text,
                        });
                        match self.on_list(noted, caller).await {
                            Ok(()) => {
                                // The note reaches the sub-agent as a message that mentions it.
                                let name = self
                                    .list()
                                    .await
                                    .ok()
                                    .and_then(|list| {
                                        list.get(&Id::from_raw(agent.clone()))
                                            .map(|its| its.name.clone())
                                    })
                                    .unwrap_or_else(|| agent.clone());
                                if let Err(error) = self.tell(&agent, &name, &said).await {
                                    tracing::warn!(workspace = %self.workspace, %agent, %error, "a sub-agent was not told its note");
                                }
                                self.one(&agent).await
                            }
                            Err(why) => Err(why),
                        }
                    }
                    None => Err("a note says something".to_owned()),
                },
                (_, Some(agent)) => {
                    let stopped = WorkspaceAgents::STOP_SUB_AGENT.with(StopSubAgent {
                        agent: Id::from_raw(agent.clone()),
                    });
                    match self.on_list(stopped, caller).await {
                        Ok(()) => self.one(&agent).await,
                        Err(why) => Err(why),
                    }
                }
                (_, None) => return Err(NotTaken::UnknownAction),
            }
        };
        Ok(match done {
            Ok(value) => InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            Err(why) => InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: REFUSED.to_owned(),
                    message: why,
                    details: None,
                    retryable: false,
                },
            },
        })
    }
}

impl TurnProvider for TurnTeam {
    /// The team's provider, as a listing of everything names it beside the workspace's
    /// resources: to a turn that is offered none of it, not at all.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if self.offered.is_empty() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: PROVIDER.to_owned(),
            app_id: PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The rows `query` asks for, when it asks the team's provider.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// Everything asked of the team's provider is this one's to answer, also what it refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
    }

    /// A turn's caller changes when its person writes again while it works, so a call is
    /// taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// `given` read as what a verb takes.
fn read_as<T: serde::de::DeserializeOwned>(given: &Map<String, Value>) -> Result<T, NotTaken> {
    serde_json::from_value(Value::Object(given.clone())).map_err(|error| NotTaken::InvalidRequest {
        reason: error.to_string(),
    })
}

/// The team's rows, as a catalogue offers them to an agent: its read, and the three verbs.
fn rows() -> Vec<CatalogueRow> {
    let words = |name: &'static str, about: &'static str| {
        ActionParam::optional(name, ParamType::Text, about)
    };
    let named = || {
        ActionParam::required(
            "agent",
            ParamType::Text,
            "The sub-agent, by its id: one of your team.",
        )
    };
    vec![
        row(
            (READ, "Read team", ActionKind::Read),
            "Your team: the sub-agents you started and theirs, each with how it stands \
             (working, needs_person, done, failed or stopped), since when, one line of its \
             last answer and what it made. Nobody above or beside you is in it.",
            Vec::new(),
        ),
        row(
            (START, "Start sub-agent", ActionKind::Undoable),
            "Start a sub-agent for a part of your work. It works on the one job you give it, \
             on its own, for the person you work for. It cannot read your conversation, so \
             the job says everything it needs. You are asked again with its answer when its \
             work ends. Refused past how deep sub-agents go and how many work at once, which \
             your person sets.",
            vec![
                ActionParam::required(
                    "name",
                    ParamType::Text,
                    "What people call it: a few words naming its job.",
                ),
                ActionParam::required(
                    "job",
                    ParamType::Text,
                    "The job, in words: the first thing it is told.",
                ),
                words("role", "Its role in a few words, as Researcher."),
                words("instruction", "How it is to work, in words."),
                words(
                    "kind",
                    "The kind of work that picks its model: subtask (the default), task, \
                     request, classifier or one_shot.",
                ),
                words(
                    "model",
                    "The model it works with, as provider/model. Leave it out for your \
                     person's choice for the kind.",
                ),
                words("task", "The issue in Tasks it works for, by its address."),
                words(
                    "id",
                    "Its id: letters, digits and dashes. Leave it out and its name makes one.",
                ),
            ],
        ),
        row(
            (NOTE, "Note sub-agent", ActionKind::Undoable),
            "Tell a sub-agent of your team something more. It is asked it once what it works \
             on now is done, and works again on it if it had ended.",
            vec![
                named(),
                ActionParam::required("text", ParamType::Text, "What it is told."),
            ],
        ),
        row(
            (STOP, "Stop sub-agent", ActionKind::Undoable),
            "Stop a sub-agent of your team: it ends what it is doing. A later note makes it \
             work again.",
            vec![named()],
        ),
    ]
}

/// One row of the team's: `action` on the team, as a catalogue offers it.
fn row(
    (action, label, kind): (&str, &str, ActionKind),
    description: &str,
    params: Vec<ActionParam>,
) -> CatalogueRow {
    let mut contract = ActionContract {
        kind,
        ..ActionContract::default()
    };
    // A turn's own: no person picks it from a list.
    contract.set_audience(tilo_actions::contract::Audience::Agents);
    CatalogueRow::for_action(
        PROVIDER,
        PROVIDER,
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(TEAM, action),
            resource_kind: TEAM.to_owned(),
            resource_label: "Sub-agents".to_owned(),
            action_label: label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description.to_owned()),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract,
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The team's rows are its read and the list's three verbs, each for an agent and none
    /// for a person's listing, and nothing in them reaches outside the workspace.
    #[test]
    fn the_team_offers_its_read_and_the_lists_three_verbs_to_agents_alone() {
        let offered = rows();
        let named: Vec<&str> = offered
            .iter()
            .filter_map(|row| row.action.as_ref())
            .map(|action| action.address.action.as_str())
            .collect();
        assert_eq!(named, [READ, START, NOTE, STOP]);
        for row in &offered {
            let action = row.action.as_ref().unwrap();
            assert_eq!(
                (row.provider.as_str(), action.address.resource.as_str()),
                (PROVIDER, TEAM)
            );
            assert!(action.contract.reaches_agents() && !action.contract.reaches_people());
            assert_ne!(
                action.contract.kind,
                ActionKind::Outside,
                "{}",
                action.address.action
            );
        }
        assert_eq!(
            offered[0].action.as_ref().unwrap().contract.kind,
            ActionKind::Read
        );
    }
}
