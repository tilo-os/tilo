//! What a turn on this server does with its keeper's skills: how kinds of work are done for
//! them ([`tilo_workspace_agent::skills`]).
//!
//! A person's skills are that person's own, kept by this server in their own store
//! ([`crate::workspaces::person_skills`]). One rule decides everything here, the rule that
//! decides what a turn knows of what was learned about its keeper ([`learned::keeps`]): a turn
//! is told skills, and changes them, only where it is its agent's keeper's own, in the
//! keeper's own conversation with the agent. For such a turn:
//!
//! - **It is told the skills that fit its request** ([`tell`]): every skill by its name and
//!   what it is for, and in full the few that fit the request by its words
//!   ([`tilo_workspace_agent::skills::Skill::fits`]). So a kind of work its person corrected
//!   once is done the corrected way by a turn that never saw the correction.
//! - **It writes and corrects them** through a provider of the turn's own ([`TurnSkills`]):
//!   the list's verbs by name, each applied to the keeper's list as the turn's agent acting
//!   for them, and one read. The list's own rules decide, and what it refuses is what the
//!   turn's tool is told.
//! - **It hands one to a sub-agent it starts** ([`for_sub_agent`]): the sub-agent takes the
//!   model the skill names, and works by that skill. Which model its turns are then asked of
//!   is decided where every turn's is: by what the list of the workspace's agents keeps for
//!   it.
//!
//! **Nothing a skill says is written where a workspace's other members read.** The list of
//! the workspace's agents and its messages are documents every member's device holds, so a
//! handed skill stands in neither. Which skill a sub-agent was handed is kept in its keeper's
//! own store on this server, beside the skills
//! ([`crate::workspaces::person_skills::HandedSkill`]), by the skill's id. What the skill says
//! is read there for each turn of that sub-agent which its keeper's own words or the job
//! started in the sub-agent's own conversation ([`works_by`]), as the skill is then, and goes
//! with the turn's request to the model and into nothing that is kept. A coding program is
//! handed none: what it is asked is kept in the workspace's machine.
//!
//! A turn anybody else started with the same agent, the keeper's own turn in a conversation
//! another person takes part in, and every turn of a sub-agent are told nothing of the list,
//! are offered none of it, and are answered what an unknown action is. A sub-agent knows the
//! one skill it was started with and no other, and only in a turn that is its keeper's. That
//! one it hands on to a sub-agent of its own ([`handed_on`]): which skill it is, is read from
//! its keeper's store on this server and never from what the workspace's list keeps of it, so
//! no text in an entry gives a sub-agent a skill. So a skill of one person never reaches a
//! turn another person started, or an agent another person keeps.
//!
//! What its keeper bounds an agent to holds here too ([`lets`]). Each of the list's verbs,
//! the read among them, is decided where Chat's own verb of that name on the person's list
//! is decided, by the rules the turn's agent carries. An agent bounded off the list is told
//! none of its keeper's skills, is offered none of the verbs its bound refuses, has a call
//! of one refused, and hands no skill to a sub-agent. An agent bounded off one skill is not
//! told that skill, does not find it in the list it reads, neither changes it nor writes one
//! of its name again, and does not hand it on. A bound only ever narrows this. What a
//! sub-agent was handed before its starter was bounded stays with that sub-agent.
//!
//! A skill says how work is done. It decides nothing about what a turn may do: every call
//! the turn then makes is decided where it always was. Nothing a skill says is written to a
//! log.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Context, Grantee, Party, TreePath};
use tilo_actions::remote::ActionCallError as NotTaken;
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest,
    Pending, RemoteActionTarget,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    InvocationUpdate, ParamType,
};
use tilo_workspace_agent::skills::{KIND, Skill, Skills};
use tilo_workspace_agent::{Agent, NewSubAgent};

use super::turn_provider::{Ending, Rows};
use super::{CHAT, Turn, TurnProvider, TurnScope, WorkspaceId, learned};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::person_skills::{self, HandedSkill, NotChanged, SkillView};

/// The provider a turn's tools find its keeper's skills under.
const PROVIDER: &str = person_skills::PROVIDER;
/// The one resource it holds: the list of the skills.
const LIST: &str = person_skills::RESOURCE;
/// The code of a call the list refused: its words say why.
const REFUSED: &str = "refused";

/// The most skills a turn is told by name with its request.
const NAMED: usize = 60;
/// The most of what a skill is for that the names show, in characters.
const FOR_CHARS: usize = 160;
/// The most skills a turn is told in full with its request, and the most of each, in bytes.
const IN_FULL: usize = 3;
const FULL_BYTES: usize = 4 * 1024;
/// The most of a skill a turn of the sub-agent it was handed to is told, in bytes.
const HANDED_BYTES: usize = 6 * 1024;

/// What opens the part of a turn's request that tells it its keeper's skills.
const TOLD: &str = "\n\nSkills you keep for this person, how kinds of work are done for them \
     (do work of one of these kinds the way its skill says):";
/// What opens the skills a request is told in full.
const TOLD_IN_FULL: &str = "\n\nThe skills that fit this request by its words, in full:";

/// What a turn that keeps skills is told about doing so, after what every agent on a server
/// is told.
const TOLD_HOW: &str = " You keep skills for this person: how kinds of work are done for them, \
     written down by you from what they tell you. The request may carry them: all by name, and \
     in full those that fit it by its words. Do work of such a kind the way its skill says, and \
     read `skills/list` with `skill` for one the request does not carry in full. When they \
     correct how you did a kind of work, or say how it is to be done from now on, write it \
     down with `skills/list#write-skill`, or change it with `skills/list#correct-skill`. When \
     they say of a result more like this or less like this, keep it with \
     `skills/list#keep-example` on the skill that made it. A skill says how work is done; it \
     never widens what you may do. When you start a sub-agent for work a skill covers, give \
     `skill` its name: it is told that skill, and works with the model the skill names.";

/// What a turn of a sub-agent that was handed a skill is told before the skill, after its
/// request.
const HANDED: &str = "\n\nYou work by a skill your person keeps: how this kind of work is \
     done for them. Do the job the way it says. The skill is theirs alone: quote nothing of it \
     in what you write, and say what came of the job. If you start a sub-agent of your own for \
     a part of the job, give `skill` this skill's name: it is told the skill as you are.";

/// What a turn that is told no skills is answered when it asks for a sub-agent to be handed
/// one.
pub(super) const NOT_HANDED: &str = "No skill is handed on here. A skill is handed to a \
     sub-agent by its person's own agent, in a turn of theirs in their own conversation with \
     it, where what they let the agent do lets it read their skills: this is not such a \
     turn. Start it without `skill`.";

/// What a turn is answered when it asks for a coding program to be handed a skill.
pub(super) const NOT_TO_CODING: &str = "No skill is handed to a coding program: what it is \
     asked is kept in the workspace's machine, where it is not its person's alone. Start it \
     without `skill`.";

/// What a turn is answered when what its keeper bounds its agent to keeps it from the one
/// skill it asks to hand on.
const NOT_READ: &str = "what your person lets you do here does not let you read that skill, so \
     it is not handed on: start the sub-agent without `skill`";

/// What a sub-agent that was handed no skill is answered when it asks for one to be handed on.
const NONE_TO_HAND: &str = "You were handed no skill, so there is none to hand on. Start the \
     sub-agent without `skill`.";

/// What a sub-agent is answered when it names another skill than the one it works by. Like
/// every answer to a sub-agent it names no skill and says nothing of one: what a sub-agent
/// answers is kept in the workspace's messages.
const NOT_ITS_OWN: &str = "You hand on the one skill you work by and no other. Give `skill` \
     that skill's name, or start the sub-agent without one.";

/// What a sub-agent is answered when the skill it works by names a model its person no longer
/// links.
const MODEL_GONE: &str = "The skill you work by names a model your person no longer links, so \
     it is not handed on. Start the sub-agent without `skill`.";

/// The keeper whose skills `turn` is told, and hands to a sub-agent it starts, in
/// `workspace`: its agent's keeper, where the turn is the keeper's own in their own
/// conversation with the agent ([`learned::keeps`]) and what the keeper bounds the agent to
/// lets it read the list ([`lets`]). `None` for every other turn.
pub(super) fn keeps(turn: &Turn, workspace: &str) -> Option<[u8; 32]> {
    learned::keeps(turn).filter(|_| lets(&turn.agent, workspace, person_skills::READ, None))
}

/// Whether what its keeper bounds `agent` to lets a turn of it take `verb` of the list in
/// `workspace`, on the skill `on` where the verb names one the list keeps. Decided as the
/// agent, with the rules every turn of it carries ([`Agent::turn_rules`]), where Chat's own
/// verb of that name on the person's list is decided: at the list, and for a verb on one
/// skill at that skill as well. So a rule on Chat, on the list's kind, on one of its verbs
/// or on one skill holds here, and an agent nothing narrows takes every verb. It never
/// decides whose turn a turn is: that is asked first ([`learned::keeps`]).
fn lets(agent: &Agent, workspace: &str, verb: &str, on: Option<&str>) -> bool {
    let kind = if verb == person_skills::READ {
        tilo_action_policy::ActionKind::Read
    } else {
        tilo_action_policy::ActionKind::Undoable
    };
    let list = TreePath::workspace(workspace).app(CHAT);
    let mut nodes = vec![list.clone().kind(KIND).resource(KIND).action(verb)];
    if let Some(skill) = on {
        nodes.push(
            list.kind(format!("{KIND}.skills"))
                .resource(format!("{KIND}/skills/{skill}"))
                .action(verb),
        );
    }
    let mut grants =
        tilo_action_policy::Acting::handed_agent(agent.id.0.as_str(), agent.keeper.as_str())
            .handed_preset(workspace);
    grants.extend(tilo_action_policy::roles::agent_bound(
        &agent.id.0,
        &agent.keeper,
        &agent.turn_rules(workspace),
    ));
    let party = Party::new(vec![Grantee::agent(agent.id.0.clone())], &grants);
    // Its grants hold until they are taken back: no time decides them.
    nodes
        .iter()
        .all(|node| party.decide(node, kind, &Context::at(0)).answer == Answer::Allow)
}

/// What a turn that keeps skills in `workspace` is told about doing so; nothing for any
/// other turn.
pub(super) fn told_how(turn: &Turn, workspace: &str) -> &'static str {
    if keeps(turn, workspace).is_some() {
        TOLD_HOW
    } else {
        ""
    }
}

/// `turn` as this server works it: its own copy, told its keeper's skills with its request
/// where the turn is the keeper's own ([`tell`]), as what was learned about them is told, and
/// told the one skill its agent was handed where it is a sub-agent's ([`tell_handed`]). The
/// copy is this server's own while the turn goes: what it is told here is asked of the model
/// and is written into no message and no document.
pub(super) async fn told_turn(workspaces: &Workspaces, workspace: &str, turn: &Turn) -> Turn {
    let mut turn = turn.clone();
    tell(workspaces, workspace, &mut turn).await;
    tell_handed(workspaces, workspace, &mut turn).await;
    turn
}

/// The keeper whose store says which skill `turn`'s agent works by: the keeper of a sub-agent
/// the server's model works, where the turn was asked for that keeper in the sub-agent's own
/// conversation, which is the keeper's alone with it. `None` for every other turn: one
/// another member started, one anywhere but in that conversation, and one a coding program in
/// the workspace's machine works.
pub(super) fn works_by(turn: &Turn) -> Option<[u8; 32]> {
    let sub = turn.agent.sub.as_ref()?;
    if sub.coding.is_some() {
        return None;
    }
    let keeper = learned::keeper_asking(&turn.agent, &turn.person)?;
    let own =
        chat::summons::conversation_address(&tilo_resource::Id::from_raw(sub.conversation.clone()));
    (turn.conversation == own).then_some(keeper)
}

/// Tell `turn` the skill its agent was handed, where it is a sub-agent's turn that is its
/// keeper's ([`works_by`]): the skill as its keeper's list holds it now, after the turn's
/// request. Nothing is told where the sub-agent was handed none, where what is kept was
/// handed to an earlier sub-agent of that id, and where the skill was forgotten since.
async fn tell_handed(workspaces: &Workspaces, workspace: &str, turn: &mut Turn) {
    let Some(keeper) = works_by(turn) else {
        return;
    };
    let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
    let (agent, made) = (turn.agent.id.0.clone(), turn.agent.created_at);
    // Both are files of the keeper's store: read off the runtime's workers.
    let told = tokio::task::spawn_blocking(move || {
        let handed = workspaces
            .skill_handed(&keeper, &workspace, &agent)
            .filter(|handed| handed.is_for(made))?;
        let kept = workspaces.skills_of(&keeper, &workspace)?;
        let (_, skill) = kept
            .skills
            .iter()
            .find(|(id, _)| id.as_str() == handed.skill)?;
        Some(format!("{HANDED}\n{}", skill.told(HANDED_BYTES)))
    })
    .await
    .ok()
    .flatten();
    if let Some(told) = told {
        turn.request.push_str(&told);
    }
}

/// Tell `turn` its keeper's skills with its request, where the turn is the keeper's own and
/// its agent may read them ([`keeps`]): all by name, and in full those that fit what it is
/// asked. Nothing is told to any other turn, and nothing while the keeper keeps no skill.
async fn tell(workspaces: &Workspaces, workspace: &str, turn: &mut Turn) {
    let Some(keeper) = keeps(turn, workspace) else {
        return;
    };
    let (workspaces, kept_in) = (workspaces.clone(), workspace.to_owned());
    let kept = tokio::task::spawn_blocking(move || workspaces.skills_of(&keeper, &kept_in))
        .await
        .ok()
        .flatten();
    if let Some(kept) = kept {
        // A skill its keeper bounds the agent off is told neither by name nor in full.
        let reads = |skill: &str| lets(&turn.agent, workspace, person_skills::READ, Some(skill));
        let told = told(&kept, &turn.request, reads);
        turn.request.push_str(&told);
    }
}

/// What a turn asked with `request` is told of the skills `kept` that its agent `reads`: each
/// by its name and what it is for, the one changed last first, then in full those that fit
/// the request's own words.
fn told(kept: &Skills, request: &str, reads: impl Fn(&str) -> bool) -> String {
    let mut skills: Vec<&Skill> = kept
        .skills
        .iter()
        .filter(|(id, _)| reads(id.as_str()))
        .map(|(_, skill)| skill)
        .filter(|skill| !skill.name.trim().is_empty())
        .collect();
    if skills.is_empty() {
        return String::new();
    }
    skills.sort_by_key(|skill| std::cmp::Reverse(skill.at));
    let mut said = TOLD.to_owned();
    for skill in skills.iter().take(NAMED) {
        let work: String = skill.work.trim().chars().take(FOR_CHARS).collect();
        let _ = write!(said, "\n- {}: {work}", skill.name.trim());
    }
    let more = skills.len().saturating_sub(NAMED);
    if more > 0 {
        let _ = write!(said, "\n({more} more: read `skills/list`)");
    }
    let job = own_words(request);
    let mut fitting = skills.iter().filter(|skill| skill.fits(job)).take(IN_FULL);
    if let Some(first) = fitting.next() {
        let _ = write!(said, "{TOLD_IN_FULL}\n\n{}", first.told(FULL_BYTES));
        for skill in fitting {
            let _ = write!(said, "\n\n{}", skill.told(FULL_BYTES));
        }
    }
    said
}

/// What a turn was asked in its person's own words: `request` up to what this server tells
/// the turn after them, which is no part of the job a skill is held against.
fn own_words(request: &str) -> &str {
    [
        runtime_collab::INSTRUCTION_CONTEXT,
        runtime_collab::INSTRUCTION_LEARNED,
    ]
    .iter()
    .filter_map(|told| request.find(told))
    .min()
    .and_then(|end| request.get(..end))
    .unwrap_or(request)
}

/// What a sub-agent started for work a skill covers is handed: which skill of its keeper's it
/// works by, and the model that skill names. Nothing the skill says: that is told to its turns
/// ([`tell_handed`]).
pub(super) struct Handed {
    keeper: [u8; 32],
    /// The skill, by its id in the keeper's list.
    skill: String,
    model: Option<String>,
}

impl Handed {
    /// Give `new`, a sub-agent about to be started, the skill's model unless its starter
    /// named one. Nothing else of the skill goes into what the workspace's list keeps of it.
    pub(super) fn to(&self, new: &mut NewSubAgent) {
        if new
            .model
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
        {
            new.model.clone_from(&self.model);
        }
    }

    /// Keep, in the keeper's own store on this server, that the sub-agent `id` of `workspace`
    /// works by this skill: before the workspace's list holds the sub-agent with no `made_ms`,
    /// and once it does with when the list made it.
    ///
    /// # Errors
    /// The store cannot be written.
    pub(super) async fn kept_for(
        &self,
        workspaces: &Workspaces,
        workspace: &str,
        (id, made_ms): (&str, Option<i64>),
    ) -> Result<(), String> {
        let handed = HandedSkill {
            skill: self.skill.clone(),
            at_ms: tilo_clock::unix_now_ms_i64(),
            made_ms,
        };
        keep_handed(workspaces, workspace, (self.keeper, id), Some(handed)).await
    }

    /// Take away again what was kept for the sub-agent `id`: the list refused its start.
    pub(super) async fn taken_from(&self, workspaces: &Workspaces, workspace: &str, id: &str) {
        // Where this fails, what stays names a sub-agent the list never made, and reaches none.
        let _ = keep_handed(workspaces, workspace, (self.keeper, id), None).await;
    }
}

/// Write which skill `keeper`'s sub-agent `id` was `handed` into their store, or take it away:
/// a file, written off the runtime's workers.
async fn keep_handed(
    workspaces: &Workspaces,
    workspace: &str,
    (keeper, id): ([u8; 32], &str),
    handed: Option<HandedSkill>,
) -> Result<(), String> {
    let (workspaces, workspace, id) = (workspaces.clone(), workspace.to_owned(), id.to_owned());
    tokio::task::spawn_blocking(move || workspaces.hand_skill(&keeper, &workspace, &id, handed))
        .await
        .map_err(|error| error.to_string())?
}

/// What a sub-agent `agent` starts for its keeper is handed when it is started in `workspace`
/// with the skill `named`: which skill, and its model. Whoever calls this has decided that
/// the turn is told the keeper's skills ([`keeps`]).
///
/// # Errors
/// The keeper keeps no skill under that id or name, what they bound the agent to keeps it
/// from that skill, or the skill names a model they no longer link on this server: in words
/// the turn that asked is told.
pub(super) async fn for_sub_agent(
    workspaces: &Workspaces,
    workspace: &str,
    (agent, keeper): (&Agent, &[u8; 32]),
    named: &str,
) -> Result<Handed, String> {
    let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
    let (agent, keeper, named) = (agent.clone(), *keeper, named.to_owned());
    tokio::task::spawn_blocking(move || {
        let kept = workspaces
            .skills_of(&keeper, &workspace)
            .unwrap_or_default();
        let (id, skill) = kept.named(&named).ok_or_else(|| {
            format!(
                "no skill is kept under the id or the name {named:?}: read `skills/list` for \
                 the skills there are, or start the sub-agent without one"
            )
        })?;
        // Handing it on is reading it: a rule on this one skill holds as well.
        if !lets(&agent, &workspace, person_skills::READ, Some(id.as_str())) {
            return Err(NOT_READ.to_owned());
        }
        if let Some(model) = &skill.model {
            workspaces
                .person_links_model(&keeper, model)
                .map_err(|why| {
                    format!(
                        "the skill {:?} names a model that cannot be asked: {why} Correct the \
                         skill's model, or start the sub-agent without the skill.",
                        skill.name
                    )
                })?;
        }
        Ok(Handed {
            keeper,
            skill: id.to_string(),
            model: skill.model.clone(),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Whether `named` names the skill `kept` holds as `own`: by that id, or by what the skill is
/// called.
fn is_named(kept: &Skills, named: &str, own: &str) -> bool {
    kept.named(named).is_some_and(|(id, _)| id.as_str() == own)
}

/// What a sub-agent `lead` starts is handed where `lead` names the skill `named` for it: the
/// one skill `lead` works by itself, as this server kept it in its keeper's own store when
/// `lead` was started ([`Workspaces::skill_handed`]), and as that skill is now. Nothing the
/// list of the workspace's agents keeps of `lead` decides it, so no text in an entry gives a
/// sub-agent a skill, and `lead` learns nothing here of a skill it was not handed. `above` is
/// the person's own agent its line of starters ends at: what its keeper bounds that agent to
/// holds for the skill handed on, as it held when `lead` was handed it.
///
/// # Errors
/// `lead` was handed no skill or the skill was forgotten since, `named` names another, what
/// its keeper bounds `above` to keeps it from the skill, or the skill names a model its keeper
/// no longer links: in words the turn that asked is told, which name no skill.
pub(super) async fn handed_on(
    workspaces: &Workspaces,
    workspace: &str,
    (lead, above, keeper): (&Agent, &Agent, &[u8; 32]),
    named: &str,
) -> Result<Handed, String> {
    let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
    let (lead, above) = (lead.clone(), above.clone());
    let (keeper, named) = (*keeper, named.to_owned());
    tokio::task::spawn_blocking(move || {
        let own = workspaces
            .skill_handed(&keeper, &workspace, &lead.id.0)
            .filter(|handed| handed.is_for(lead.created_at))
            .ok_or_else(|| NONE_TO_HAND.to_owned())?;
        let kept = workspaces
            .skills_of(&keeper, &workspace)
            .unwrap_or_default();
        let (id, skill) = kept
            .skills
            .iter()
            .find(|(id, _)| id.as_str() == own.skill)
            .ok_or_else(|| NONE_TO_HAND.to_owned())?;
        if !is_named(&kept, &named, id.as_str()) {
            return Err(NOT_ITS_OWN.to_owned());
        }
        if !lets(&above, &workspace, person_skills::READ, Some(id.as_str())) {
            return Err(NOT_READ.to_owned());
        }
        if let Some(model) = &skill.model
            && workspaces.person_links_model(&keeper, model).is_err()
        {
            return Err(MODEL_GONE.to_owned());
        }
        Ok(Handed {
            keeper,
            skill: id.to_string(),
            model: skill.model.clone(),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Make `scope`'s service answer as well for the skills of the keeper of `turn`'s agent.
pub(super) fn join(workspaces: &Workspaces, turn: &Turn, scope: &mut TurnScope) {
    let keeper = learned::keeps(turn);
    // What the turn is offered: of the keeper's own turn, the verbs its agent's bound lets.
    let offered = if keeper.is_some() {
        rows()
            .into_iter()
            .filter(|row| {
                row.action.as_ref().is_some_and(|action| {
                    lets(&turn.agent, &scope.workspace, &action.address.action, None)
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let skills = Arc::new(TurnSkills {
        workspaces: workspaces.clone(),
        workspace: scope.workspace.clone(),
        workspace_id: scope.workspace_id,
        agent: turn.agent.clone(),
        keeper,
        offered,
    });
    let service = &mut scope.provider;
    *service = service.clone().answering(skills);
}

/// What one turn does with its keeper's skills.
struct TurnSkills {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The turn's agent, as the list of the workspace's agents held it when the turn began.
    agent: Agent,
    /// The keeper whose skills the turn is told and changes: none for a turn that is not
    /// its keeper's own in their own conversation with the agent, which is offered nothing.
    keeper: Option<[u8; 32]>,
    /// What the turn is offered: the read and the list's verbs, less what its keeper bounds
    /// the agent off ([`lets`]); nothing for a turn that is not its keeper's own.
    offered: Vec<CatalogueRow>,
}

impl TurnSkills {
    /// The keeper whose skills `caller` reads and changes: the turn's, where the turn is told
    /// them and `caller` is on a turn of this agent that is its keeper's own. The first check
    /// of everything here: every other caller is answered what an unknown action is.
    async fn keepers(&self, caller: &Caller) -> Result<[u8; 32], NotTaken> {
        let keeper = self.keeper.ok_or(NotTaken::UnknownAction)?;
        let turn = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: None,
        };
        let kept = self.workspaces.keepers_turn(&turn).await?;
        if kept.agent == self.agent.id.0 && kept.person == keeper {
            Ok(keeper)
        } else {
            Err(NotTaken::UnknownAction)
        }
    }

    /// Take the call `request` asks for as `caller`: how it ended, or why this server did not
    /// take it.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, NotTaken> {
        let invocation = request.invocation;
        let keeper = self.keepers(caller).await?;
        let address = &request.target.address;
        if address.resource != LIST {
            return Err(NotTaken::UnknownAction);
        }
        let given = match &request.argument {
            InvokeArgument::Json(given) => given.clone(),
            _ => Map::new(),
        };
        let action = address.action.clone();
        let (workspaces, workspace) = (self.workspaces.clone(), self.workspace.clone());
        let agent = self.agent.clone();
        // The list is a file of the keeper's store: read and written off the runtime's workers.
        let done = tokio::task::spawn_blocking(move || {
            carried_out(
                &workspaces,
                (&keeper, &workspace, &agent),
                (&action, &given),
            )
        })
        .await
        .map_err(|error| NotTaken::Refused {
            reason: error.to_string(),
        })??;
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

/// Carry out `action` on the skills this server keeps for `keeper` in `workspace`, as `agent`
/// acting for them: what the turn's tool is told, the list's refusal in its own words, or why
/// the call was not taken at all.
fn carried_out(
    workspaces: &Workspaces,
    (keeper, workspace, agent): (&[u8; 32], &str, &Agent),
    (action, given): (&str, &Map<String, Value>),
) -> Result<Result<Value, String>, NotTaken> {
    if action == person_skills::READ {
        return read(workspaces, (keeper, workspace, agent), given);
    }
    let by = Some(agent.id.0.as_str());
    let permits = |id: Option<&tilo_resource::Id>| {
        lets(agent, workspace, action, id.map(tilo_resource::Id::as_str))
    };
    match workspaces.skills_action_with_access(keeper, workspace, by, (action, given), permits) {
        Ok(Some(id)) => {
            if !lets(agent, workspace, person_skills::READ, Some(id.as_str())) {
                return Ok(Ok(json!({"changed": id})));
            }
            let skill = workspaces
                .skills_of(keeper, workspace)
                .and_then(|kept| kept.skills.get(&id).map(|skill| SkillView::of(&id, skill)));
            Ok(Ok(skill.map_or_else(
                || json!({"changed": id}),
                |skill| json!({"skill": skill}),
            )))
        }
        Ok(None) => Ok(Ok(json!({"forgotten": true}))),
        Err(NotChanged::Unknown) => Err(NotTaken::UnknownAction),
        Err(NotChanged::Unreadable(reason)) => Err(NotTaken::InvalidRequest { reason }),
        Err(NotChanged::Denied) => Err(NotTaken::NotPermitted),
        Err(NotChanged::Refused(why)) => Ok(Err(why)),
    }
}

/// What a turn of `agent` reads of the skills this server keeps for `keeper` in `workspace`:
/// the one `given` names by `skill` in full, or all of them by name, the one changed last
/// first. A bound on an individual skill also applies to an explicit read.
fn read(
    workspaces: &Workspaces,
    (keeper, workspace, agent): (&[u8; 32], &str, &Agent),
    given: &Map<String, Value>,
) -> Result<Result<Value, String>, NotTaken> {
    if !lets(agent, workspace, person_skills::READ, None) {
        return Err(NotTaken::NotPermitted);
    }
    let kept = workspaces.skills_of(keeper, workspace).unwrap_or_default();
    let reads = |skill: &str| lets(agent, workspace, person_skills::READ, Some(skill));
    let named = given
        .get("skill")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|named| !named.is_empty());
    if let Some(named) = named {
        let Some((id, skill)) = kept.named(named) else {
            return Ok(Err(format!(
                "no skill is kept under the id or the name {named:?}: read `skills/list` \
                 without `skill` for the skills there are"
            )));
        };
        if !reads(id.as_str()) {
            return Err(NotTaken::NotPermitted);
        }
        return Ok(Ok(json!({"skill": SkillView::of(&id, skill)})));
    }
    let mut skills: Vec<_> = kept
        .skills
        .iter()
        .filter(|(id, _)| reads(id.as_str()))
        .collect();
    skills.sort_by_key(|(_, skill)| std::cmp::Reverse(skill.at));
    let skills: Vec<Value> = skills
        .into_iter()
        .map(|(id, skill)| {
            json!({
                "id": id.to_string(),
                "name": skill.name,
                "work": skill.work,
                "model": skill.model,
                "examples": skill.examples.len(),
            })
        })
        .collect();
    Ok(Ok(json!({"skills": skills})))
}

impl TurnProvider for TurnSkills {
    /// The skills' provider, as a listing of everything names it beside the workspace's
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

    /// The rows `query` asks for, when it asks the skills' provider.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// Everything asked of the skills' provider on a turn is this one's to answer, also what
    /// it refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
    }

    /// A turn's caller changes when its person writes again while it works, so a call is
    /// taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// What a turn is told about the model a skill names.
const MODEL: &str = "The model the work is done with, as provider/model, the provider by the \
     name your person gave it: one they linked. Leave it out for their choice for the kind of \
     work.";

/// A parameter in words that may be left out.
fn words(name: &'static str, about: &'static str) -> ActionParam {
    ActionParam::optional(name, ParamType::Text, about)
}

/// A parameter in words that a verb needs.
fn needed(name: &'static str, about: &'static str) -> ActionParam {
    ActionParam::required(name, ParamType::Text, about)
}

/// The skill a verb is about.
fn named() -> ActionParam {
    needed("skill", "The skill, by its name or its id.")
}

/// The skills' rows, as a catalogue offers them to an agent: the read, and the list's verbs.
fn rows() -> Vec<CatalogueRow> {
    let mut rows = whole_skill_rows();
    rows.extend(example_rows());
    rows.push(row(
        (person_skills::FORGET, "Forget skill", ActionKind::Undoable),
        "Forget a skill, when the person asks you to: no later turn is told it.",
        vec![named()],
    ));
    rows
}

/// The read, and the verbs that write a skill down and correct it.
fn whole_skill_rows() -> Vec<CatalogueRow> {
    vec![
        row(
            (person_skills::READ, "Read skills", ActionKind::Read),
            "The skills you keep for the person you work for: each by its name, what it is \
             for and its model. With `skill`, that one in full: how its work is done, the \
             form of the result, and the results they accepted and rejected.",
            vec![words("skill", "One skill, by its name or its id.")],
        ),
        row(
            (person_skills::WRITE, "Write skill", ActionKind::Undoable),
            "Write down how a kind of work is done for the person you work for: after they \
             corrected how you did it, or said how it is to be done from now on. A skill of \
             that name is written again, not twice, and keeps its examples.",
            vec![
                needed("name", "What it is called, in a few words."),
                needed(
                    "work",
                    "The kind of work it is for, in a sentence or two: when it applies.",
                ),
                needed(
                    "how",
                    "How that work is done here, step by step where that helps.",
                ),
                words("form", "The form the result takes."),
                words("model", MODEL),
            ],
        ),
        row(
            (
                person_skills::CORRECT,
                "Correct skill",
                ActionKind::Undoable,
            ),
            "Correct a skill: only what you name changes, and an empty form or model takes \
             it away. Its examples stay.",
            vec![
                named(),
                words("name", "What it is called now."),
                words("work", "The kind of work it is for now."),
                words("how", "How the work is done now."),
                words("form", "The form the result takes now."),
                words("model", MODEL),
            ],
        ),
    ]
}

/// The verbs that keep a result as a skill's example and take one out.
fn example_rows() -> Vec<CatalogueRow> {
    vec![
        row(
            (
                person_skills::KEEP_EXAMPLE,
                "Keep example",
                ActionKind::Undoable,
            ),
            "Keep a result as an example of the skill that made it, when the person says more \
             like this or less like this. The newest examples stay.",
            vec![
                named(),
                needed("text", "The result, or the part of it that is meant."),
                ActionParam::required(
                    "accepted",
                    ParamType::Boolean,
                    "True for more like this, false for less like this.",
                ),
                words("note", "What they said about it."),
            ],
        ),
        row(
            (
                person_skills::DROP_EXAMPLE,
                "Take example out",
                ActionKind::Undoable,
            ),
            "Take an example out of a skill.",
            vec![
                named(),
                needed("example", "The example, by its id: read the skill for it."),
            ],
        ),
    ]
}

/// One row of the skills': `action` on the list, as a catalogue offers it.
fn row(
    (action, label, kind): (&str, &str, ActionKind),
    description: &str,
    params: Vec<ActionParam>,
) -> CatalogueRow {
    let mut contract = ActionContract {
        kind,
        ..ActionContract::default()
    };
    // A turn's own: no person picks it from a list. A person changes their skills through
    // the provider of the same name their own device asks.
    contract.set_audience(tilo_actions::contract::Audience::Agents);
    CatalogueRow::for_action(
        PROVIDER,
        PROVIDER,
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(LIST, action),
            resource_kind: LIST.to_owned(),
            resource_label: "Skills".to_owned(),
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
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use tilo_resource::{NonBlank, Replica};
    use tilo_workspace_agent::skills::skills;

    use super::*;

    /// The skills a person keeps: a weekly report, and a market analysis written after it.
    fn kept() -> Skills {
        let mut resource = Replica::<Skills>::new().as_author("ada");
        for (at, name, work, how) in [
            (
                1_000,
                "Weekly report",
                "A request for the weekly report",
                "Say what moved first.",
            ),
            (
                2_000,
                "Market analysis",
                "Analysing a market for the person",
                "One row per company, each with its source.",
            ),
        ] {
            resource.set_now_ms(at);
            resource
                .apply(Skills::WRITE_SKILL.with(skills::WriteSkill {
                    name: NonBlank::parse(name).unwrap(),
                    work: NonBlank::parse(work).unwrap(),
                    how: how.to_owned(),
                    form: None,
                    model: None,
                }))
                .unwrap();
        }
        resource.state().unwrap()
    }

    /// A turn is told every skill by its name and what it is for, the one changed last first,
    /// and in full only those that fit what its person asked in their own words: what the
    /// server tells the turn after those words makes no skill fit.
    #[test]
    fn a_turn_is_told_all_skills_by_name_and_in_full_those_that_fit_its_persons_words() {
        let kept = kept();
        let names = "\n- Market analysis: Analysing a market for the person\
                     \n- Weekly report: A request for the weekly report";
        let told_for = |request: &str| told(&kept, request, |_| true);

        let analysis = told_for("Please analyse the market for cargo bikes.");
        assert_eq!(
            analysis,
            format!(
                "{TOLD}{names}{TOLD_IN_FULL}\n\nSkill: Market analysis\nFor: Analysing a \
                 market for the person\nHow it is done here:\nOne row per company, each with \
                 its source."
            )
        );
        // No skill is of this kind of work: the names, and nothing in full.
        assert_eq!(told_for("Book a table for two."), format!("{TOLD}{names}"));
        // What came before in the conversation is no part of the job: it is not in the request
        // at all, and what was learned after the words is cut.
        let recalled =
            format!("Book a table for two.{INSTRUCTION_LEARNED}\n- Prefers the window seat");
        assert_eq!(told_for(&recalled), format!("{TOLD}{names}"));
        assert_eq!(own_words(&recalled), "Book a table for two.");
        // Nothing is told while nothing is kept.
        assert_eq!(told(&Skills::default(), "Anything.", |_| true), "");
    }

    /// A skill its agent may not read is told to its turn neither by name nor in full, and the
    /// others are told as before. Where it may read none, nothing is told.
    #[test]
    fn a_skill_its_agent_may_not_read_is_not_told() {
        let kept = kept();
        let (off, _) = kept.named("Market analysis").unwrap();
        let job = "Please analyse the market for cargo bikes.";
        assert_eq!(
            told(&kept, job, |skill| skill != off.as_str()),
            format!("{TOLD}\n- Weekly report: A request for the weekly report")
        );
        assert_eq!(told(&kept, job, |_| false), "");
    }

    /// What a sub-agent is handed gives the workspace's list its skill's model, unless its
    /// starter named one, and nothing else: not the skill's id, and no word of how it is to
    /// work beside what its starter said.
    #[test]
    fn a_handed_skill_gives_the_list_its_model_and_nothing_the_skill_says() {
        let handed = Handed {
            keeper: [7; 32],
            skill: "market-analysis".to_owned(),
            model: Some("Anthropic/claude-research".to_owned()),
        };
        let mut plain = NewSubAgent::default();
        handed.to(&mut plain);
        assert_eq!(
            plain,
            NewSubAgent {
                model: Some("Anthropic/claude-research".to_owned()),
                ..NewSubAgent::default()
            },
            "the model, and nothing else"
        );
        let mut told = NewSubAgent {
            instruction: Some("  Be brief. ".to_owned()),
            model: Some("Google/gemini-main".to_owned()),
            ..NewSubAgent::default()
        };
        let before = told.clone();
        handed.to(&mut told);
        assert_eq!(told, before, "its starter's own words and model stay");
    }

    /// A turn of a sub-agent is told the skill it was handed only where the turn is its
    /// keeper's, in the sub-agent's own conversation: not a turn another member started, not
    /// one in another conversation, not a coding program's, and no turn of an agent a person
    /// made.
    #[test]
    fn only_a_sub_agents_own_turn_for_its_keeper_works_by_what_it_was_handed() {
        const ADA: &str = "0707070707070707070707070707070707070707070707070707070707070707";
        const BO: &str = "0909090909090909090909090909090909090909090909090909090909090909";
        let direct = |person: &str, agent: &str| {
            chat::Chat::direct_id([
                tilo_resource::MemberId::person(person),
                tilo_resource::MemberId::agent(agent),
            ])
        };
        let own = direct(ADA, "bike-market");
        let turn = |person: &str, conversation: &tilo_resource::Id| Turn {
            agent: Agent {
                id: tilo_resource::Atom("bike-market".to_owned()),
                keeper: ADA.to_owned(),
                sub: Some(tilo_workspace_agent::Sub {
                    conversation: own.as_str().to_owned(),
                    ..tilo_workspace_agent::Sub::default()
                }),
                ..Agent::default()
            },
            person: person.to_owned(),
            ask: super::super::Ask::Message {
                part: "part".to_owned(),
                message: "m1".to_owned(),
                attempt: 0,
            },
            conversation: chat::summons::conversation_address(conversation),
            cause: "m1".to_owned(),
            request: "Do the job.".to_owned(),
            history: Vec::new(),
            asked: "Do the job.".to_owned(),
            fold: None,
            typed: false,
            line: None,
        };
        assert_eq!(works_by(&turn(ADA, &own)), Some([7; 32]));
        assert_eq!(works_by(&turn(BO, &own)), None, "another member asked");
        assert_eq!(works_by(&turn("", &own)), None, "nobody asked");
        assert_eq!(
            works_by(&turn(BO, &direct(BO, "bike-market"))),
            None,
            "another member's conversation with it"
        );
        assert_eq!(
            works_by(&turn(ADA, &direct(ADA, "scout"))),
            None,
            "her conversation with another agent"
        );
        let mut coding = turn(ADA, &own);
        if let Some(sub) = coding.agent.sub.as_mut() {
            sub.coding = Some(tilo_workspace_agent::coding::Coding::default());
        }
        assert_eq!(works_by(&coding), None, "a coding program's turn");
        let mut made = turn(ADA, &own);
        made.agent.sub = None;
        assert_eq!(works_by(&made), None, "an agent a person made");
    }

    /// The skills' rows are the read and the list's five verbs, each for an agent and none
    /// for a person's listing, and nothing in them reaches outside the workspace.
    #[test]
    fn the_skills_offer_their_read_and_the_lists_verbs_to_agents_alone() {
        let offered = rows();
        let named: Vec<&str> = offered
            .iter()
            .filter_map(|row| row.action.as_ref())
            .map(|action| action.address.action.as_str())
            .collect();
        assert_eq!(
            named,
            [
                person_skills::READ,
                person_skills::WRITE,
                person_skills::CORRECT,
                person_skills::KEEP_EXAMPLE,
                person_skills::DROP_EXAMPLE,
                person_skills::FORGET
            ]
        );
        for row in &offered {
            let action = row.action.as_ref().unwrap();
            assert_eq!(
                (row.provider.as_str(), action.address.resource.as_str()),
                (PROVIDER, LIST)
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

    /// A sub-agent names the skill it works by with that skill's id or with what it is called,
    /// whatever the case; another skill's name, a name nothing is kept under and no name at
    /// all name it not.
    #[test]
    fn a_skill_is_named_by_its_id_or_its_name_and_by_no_other() {
        let kept = kept();
        let (own, _) = kept.named("Market analysis").unwrap();
        assert!(is_named(&kept, " market ANALYSIS ", own.as_str()));
        assert!(is_named(&kept, own.as_str(), own.as_str()));
        assert!(!is_named(&kept, "Weekly report", own.as_str()));
        assert!(!is_named(&kept, "No such skill", own.as_str()));
        assert!(!is_named(&kept, "", own.as_str()));
    }

    /// The agent `scout` one person keeps, bounded to `rules`.
    fn bounded(rules: &[tilo_action_policy::Rule]) -> Agent {
        let mut agent = Agent {
            keeper: "ada".to_owned(),
            bound: rules.iter().map(tilo_workspace_agent::Bound::of).collect(),
            ..Agent::default()
        };
        agent.id.0 = "scout".to_owned();
        agent
    }

    /// What its keeper bounds an agent to decides which of the list's verbs a turn of it takes.
    /// A rule on Chat or on the list's kind holds for every verb, on a skill as well; one for
    /// changes alone leaves the read; one on a verb by name takes that verb; one on a skill
    /// holds for that skill and no other. A sub-agent takes none, whatever its entry says.
    #[test]
    fn a_bound_decides_which_of_the_lists_verbs_a_turn_of_its_agent_takes() {
        use tilo_action_policy::{KindSet, Rule};
        const VERBS: [&str; 6] = [
            person_skills::READ,
            person_skills::WRITE,
            person_skills::CORRECT,
            person_skills::KEEP_EXAMPLE,
            person_skills::DROP_EXAMPLE,
            person_skills::FORGET,
        ];
        let chat = || TreePath::workspace("w").app(CHAT);
        let all = KindSet::READ.union(KindSet::UNDOABLE);
        let taken = |agent: &Agent, on: Option<&str>| -> Vec<&str> {
            VERBS
                .into_iter()
                .filter(|verb| lets(agent, "w", verb, on))
                .collect()
        };

        // Nothing narrows it: every verb, on the list and on one skill.
        let free = bounded(&[]);
        assert_eq!(taken(&free, None), VERBS);
        assert_eq!(taken(&free, Some("s1")), VERBS);

        // Bounded off Chat, or off the kind the list is: none of them, on a skill neither.
        for off in [
            Rule::new(chat(), all, Answer::Deny),
            Rule::new(chat().kind(KIND), all, Answer::Deny),
        ] {
            let agent = bounded(&[off]);
            assert!(taken(&agent, None).is_empty());
            assert!(taken(&agent, Some("s1")).is_empty());
        }

        // Kept from changing the list, it still reads it.
        let reads = bounded(&[Rule::new(
            chat().kind(KIND),
            KindSet::UNDOABLE,
            Answer::Deny,
        )]);
        assert_eq!(taken(&reads, None), [person_skills::READ]);
        assert_eq!(taken(&reads, Some("s1")), [person_skills::READ]);

        // One verb by its name.
        let forgets_none = bounded(&[Rule::new(
            chat()
                .kind(KIND)
                .resource(KIND)
                .action(person_skills::FORGET),
            KindSet::UNDOABLE,
            Answer::Deny,
        )]);
        assert_eq!(
            taken(&forgets_none, None),
            [
                person_skills::READ,
                person_skills::WRITE,
                person_skills::CORRECT,
                person_skills::KEEP_EXAMPLE,
                person_skills::DROP_EXAMPLE,
            ]
        );

        // One skill: that one is neither read nor changed, every other is.
        let one = bounded(&[Rule::new(
            chat()
                .kind(format!("{KIND}.skills"))
                .resource(format!("{KIND}/skills/s1")),
            all,
            Answer::Deny,
        )]);
        assert!(taken(&one, Some("s1")).is_empty());
        assert_eq!(taken(&one, Some("s2")), VERBS);
        assert_eq!(taken(&one, None), VERBS);

        // A sub-agent reads nothing Chat keeps and calls none of its verbs but its own team's.
        let mut sub_agent = bounded(&[]);
        sub_agent.sub = Some(tilo_workspace_agent::sub::Sub::default());
        assert!(taken(&sub_agent, None).is_empty());
        assert!(taken(&sub_agent, Some("s1")).is_empty());
    }
}
