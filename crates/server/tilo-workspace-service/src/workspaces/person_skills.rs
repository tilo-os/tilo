//! How kinds of work are done for a person, their skills, kept where the server reads and
//! writes them with every device of theirs off.
//!
//! A person's skills are that person's own resource
//! ([`tilo_workspace_agent::skills::Skills`]): what a kind of work is, how it is done here, the
//! form of its result, the results they accepted and rejected, and the model the work is done
//! with. An agent on a workspace's server writes one down while no device of its keeper is on,
//! so the server keeps them in the person's own store beside their vault, their model providers
//! and what its agents learned about them: one document for each workspace the agent works in,
//! changed through the resource's own verbs, one change at a time. It is one person's apart
//! from every other person's, and readable on disk by the server's own user alone.
//!
//! Two callers reach it. Code linked into the server, which answers the turns of a person's
//! agents and decides first whose turn it is ([`Workspaces::skills_of`],
//! [`Workspaces::skills_action`]). And the person themselves on a device of theirs, through
//! the provider [`PROVIDER`]: they read the list, write a skill down, correct one, keep or
//! take out an example, or forget one. Nobody else is answered, the person's own agents
//! included: unknown and forbidden are the same answer.
//!
//! Which of them a sub-agent was handed when the person's own agent started it is kept in the
//! same store, beside the skills of that workspace ([`HandedSkill`]): the skill by its id and
//! not a word of it. The workspace's own documents, which every member's device holds, say
//! nothing of a skill; what a handed skill says is read here when that sub-agent's turn is
//! told it.
//!
//! A skill names a model only where its person linked that model on this server
//! ([`Workspaces::person_links_model`]): a skill never asks a model somebody else pays for.
//! Nothing a skill says is written to a log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::ActionCallError::{self, UnknownAction};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Id, NonBlank, Replica};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::skills::{Skill, Skills, skills};

use super::Workspaces;
use super::person_settings::answer;

/// The provider a person reads and changes their skills on this server by.
pub const PROVIDER: &str = super::actions::SKILLS;
/// Its one resource: the skills, in the workspace the request comes through.
pub const RESOURCE: &str = "list";
/// Read the skills: answers [`Kept`].
pub const READ: &str = "read";
/// Write a skill down, `{ name, work, how, form?, model? }`.
pub const WRITE: &str = "write-skill";
/// Correct a skill, `{ skill, name?, work?, how?, form?, model? }`: only what is named changes.
pub const CORRECT: &str = "correct-skill";
/// Keep a result as an example of a skill, `{ skill, text, accepted, note? }`.
pub const KEEP_EXAMPLE: &str = "keep-example";
/// Take an example out of a skill, `{ skill, example }`.
pub const DROP_EXAMPLE: &str = "drop-example";
/// Forget a skill, `{ skill }`.
pub const FORGET: &str = "forget-skill";

/// The app whose resource this is, as every change of it names it.
const CHAT: (&str, &str) = ("so.tilo.chat", "Chat");

/// The folder of a person's store their skills are kept in, one file a workspace.
const FOLDER: &str = "skills";

/// What ends the name of the file that keeps, beside a workspace's skills, which of them each
/// sub-agent of the person's agents was handed.
const HANDED_FILE: &str = "handed.json";

/// The most sub-agents one such file names: past it, those handed longest ago go.
const MOST_HANDED: usize = 1024;

/// How long after a skill was handed the workspace's list may still make the sub-agent it was
/// handed to, in milliseconds, while this server has not kept when the list made it.
const HANDED_OPEN_MS: i64 = 60_000;

/// The most bytes one kept resource holds. Every skill at its longest with all its examples
/// and their history stays below it, and a file past it is neither read nor written.
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// One change of a kept resource at a time on this server: a change reads the resource, applies
/// itself and writes the resource back whole.
static CHANGING: Mutex<()> = Mutex::new(());

/// The provider's `action`, as a caller addresses it.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress {
            resource: RESOURCE.to_owned(),
            action: action.to_owned(),
        },
    }
}

/// What the provider answers: the skills this server keeps for the person in one workspace,
/// the one changed last first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    /// The workspace they are kept in.
    pub workspace: String,
    /// What the resource is at: the same until a skill is written, changed or forgotten, and
    /// empty while none is kept. A reader that holds a list reads again when it differs.
    pub revision: String,
    /// The skills, the one changed last first.
    pub skills: Vec<SkillView>,
}

/// One skill, as its person reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    /// Its id: what a change names it by, as its name does.
    pub id: String,
    /// What it is called.
    pub name: String,
    /// The kind of work it is for.
    pub work: String,
    /// How that work is done here.
    pub how: String,
    /// The form the result takes; empty when it has no fixed form.
    pub form: String,
    /// The model the work is done with, as provider/model; absent for the person's choice for
    /// the kind of work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The results the person accepted and rejected, oldest first.
    pub examples: Vec<ExampleView>,
    /// When it was written or last changed, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The agent that changed it last, by its id; absent where the person did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// One example of a skill, as its person reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExampleView {
    /// Its id: what `drop-example` names it by.
    pub id: String,
    /// Whether the person accepted it (more like this) or rejected it (less like this).
    pub accepted: bool,
    /// The result, or the part of it that is meant.
    pub text: String,
    /// What the person said about it; empty when they said nothing more.
    pub note: String,
    /// When it was kept, in milliseconds since the Unix epoch.
    pub at_ms: i64,
}

impl SkillView {
    /// `skill`, kept under `id`, as its person reads it.
    #[must_use]
    pub fn of(id: &Id, skill: &Skill) -> Self {
        Self {
            id: id.to_string(),
            name: skill.name.clone(),
            work: skill.work.clone(),
            how: skill.how.clone(),
            form: skill.form.clone(),
            model: skill.model.clone(),
            examples: skill
                .examples
                .iter()
                .map(|example| ExampleView {
                    id: example.id.clone(),
                    accepted: example.accepted,
                    text: example.text.clone(),
                    note: example.note.clone(),
                    at_ms: example.at,
                })
                .collect(),
            at_ms: skill.at,
            by: skill.by.clone(),
        }
    }
}

/// Which skill a sub-agent was handed when its person's own agent started it. It names the
/// skill and holds nothing the skill says: that is read from the list when a turn of the
/// sub-agent is told it, so the turn is told the skill as it is then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandedSkill {
    /// The skill, by its id in the person's list.
    pub skill: String,
    /// When it was handed, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// When the workspace's list made the sub-agent, as its entry there says; none until this
    /// server kept that the list took the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_ms: Option<i64>,
}

impl HandedSkill {
    /// Whether it was handed to the sub-agent the workspace's list made at `made_ms`: a later
    /// sub-agent that takes the id of one that is gone was handed nothing.
    #[must_use]
    pub fn is_for(&self, made_ms: i64) -> bool {
        match self.made_ms {
            Some(made) => made == made_ms,
            None => (0..=HANDED_OPEN_MS).contains(&made_ms.saturating_sub(self.at_ms)),
        }
    }
}

/// Why a call of one of the list's verbs changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotChanged {
    /// The list has no verb of that name.
    Unknown,
    /// What the verb was given does not read as what it takes.
    Unreadable(String),
    /// The caller is not permitted to act on the resolved target.
    Denied,
    /// The list refused, in its own words.
    Refused(String),
}

impl From<String> for NotChanged {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    name: String,
    work: String,
    how: String,
    #[serde(default)]
    form: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Correct {
    skill: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    work: Option<String>,
    #[serde(default)]
    how: Option<String>,
    #[serde(default)]
    form: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepExample {
    skill: String,
    text: String,
    accepted: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DropExample {
    skill: String,
    example: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forget {
    skill: String,
}

impl Workspaces {
    /// Where `person`'s skills of `workspace` are kept.
    fn skills_at(&self, person: &[u8; 32], workspace: &str) -> Result<PathBuf, String> {
        // The id as the workspace spells it, never the caller's text: the name of a file.
        let workspace = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| "not a workspace id".to_owned())?;
        Ok(self
            .person_settings_root(person)
            .join(FOLDER)
            .join(format!("{workspace}.automerge")))
    }

    /// The skills this server keeps for `person` in `workspace` and what the resource is at;
    /// none where none was written yet, and none where the resource does not read.
    fn skills_kept(&self, person: &[u8; 32], workspace: &str) -> Option<(Skills, String)> {
        let bytes = match self
            .skills_at(person, workspace)
            .and_then(|path| read(&path))
        {
            Ok(bytes) => bytes?,
            Err(error) => {
                tracing::warn!(%workspace, %error, "a person's skills are not read");
                return None;
            }
        };
        let revision = blake3::hash(&bytes)
            .to_hex()
            .get(..16)
            .unwrap_or_default()
            .to_owned();
        let read = opened(Some(bytes))
            .and_then(|resource| state(&resource))
            .ok();
        if read.is_none() {
            // Why is left unsaid: a resource's own error may quote what it holds.
            tracing::warn!(%workspace, "a person's skills do not read as their resource");
        }
        read.map(|skills| (skills, revision))
    }

    /// The skills this server keeps for `person` in `workspace`: none where none was written
    /// yet. For the code that answers the turns of the person's agents; it decides whose turn
    /// is told them.
    #[must_use]
    pub fn skills_of(&self, person: &[u8; 32], workspace: &str) -> Option<Skills> {
        self.skills_kept(person, workspace)
            .map(|(skills, _)| skills)
    }

    /// Change the skills this server keeps for `person` in `workspace` through the resource's
    /// own verbs: `change` applies them, as the agent `by` acting for the person, or as the
    /// person themselves where `by` is none, stamped with the time now. One change is made at a
    /// time, the first makes the resource, and one that fails keeps the resource as it was.
    ///
    /// # Errors
    /// `workspace` is no workspace id, the resource cannot be read or written, it would outgrow
    /// what one resource holds, or `change` refused.
    pub(super) fn change_skills<R>(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        change: impl FnOnce(&mut Replica<Skills>) -> Result<R, NotChanged>,
    ) -> Result<R, NotChanged> {
        let path = self.skills_at(person, workspace)?;
        let keeper = hex::encode(person);
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let mut resource = match by {
            Some(agent) => resource.as_agent_for(agent, &keeper),
            None => resource.as_author(&keeper),
        }
        .in_app(CHAT.0, CHAT.1);
        resource.set_now_ms(tilo_clock::unix_now_ms_i64());
        let answer = change(&mut resource)?;
        let bytes = resource.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err(NotChanged::Refused("the list of skills is full".to_owned()));
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        drop(one_at_a_time);
        Ok(answer)
    }

    /// The skills this server keeps for `person` in `workspace`, as the provider answers them.
    #[must_use]
    pub fn skills_listed(&self, person: &[u8; 32], workspace: &str) -> Kept {
        let (kept, revision) = self.skills_kept(person, workspace).unwrap_or_default();
        let mut skills: Vec<SkillView> = kept
            .skills
            .iter()
            .map(|(id, skill)| SkillView::of(id, skill))
            .collect();
        skills.sort_by_key(|skill| std::cmp::Reverse(skill.at_ms));
        Kept {
            workspace: workspace.to_owned(),
            revision,
            skills,
        }
    }

    /// Carry out `action`, one of the list's verbs by its name, with what it was `given`, on
    /// the skills this server keeps for `person` in `workspace`: as the agent `by` acting for
    /// the person, or as the person themselves where `by` is none. A skill is named by its id
    /// or by what it is called. Answers the id of the skill it wrote or changed, none for one
    /// it forgot. Whoever calls this has decided whose skills the caller may change.
    ///
    /// # Errors
    /// The list has no such verb, what it was given does not read, or the list refused: a
    /// skill it does not keep, a bound, or a model the person has not linked on this server.
    pub fn skills_action(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        action: (&str, &Map<String, Value>),
    ) -> Result<Option<String>, NotChanged> {
        self.skills_action_with_access(person, workspace, by, action, |_| true)
            .map(|id| id.map(|id| id.to_string()))
    }

    /// Apply a verb after checking the collection and the exact item it will affect.
    /// The item is resolved and checked under the same lock as the change.
    ///
    /// # Errors
    /// As in [`Self::skills_action`], or the caller is denied by `permits`.
    pub fn skills_action_with_access(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        (action, given): (&str, &Map<String, Value>),
        permits: impl Fn(Option<&Id>) -> bool,
    ) -> Result<Option<Id>, NotChanged> {
        if !permits(None) {
            return Err(NotChanged::Denied);
        }
        match action {
            WRITE => {
                let Write {
                    name,
                    work,
                    how,
                    form,
                    model,
                } = read_as(given)?;
                self.names_linked_model(person, model.as_deref())?;
                let write = skills::WriteSkill {
                    name: said(&name, "a skill has a name")?,
                    work: said(&work, "a skill says what kind of work it is for")?,
                    how,
                    form,
                    model,
                };
                self.change_skills(person, workspace, by, |resource| {
                    let existing = state(resource)?.named_by_name(&name).map(|(id, _)| id);
                    if existing.as_ref().is_some_and(|id| !permits(Some(id))) {
                        return Err(NotChanged::Denied);
                    }
                    applied(resource.apply(Skills::WRITE_SKILL.with(write)))?;
                    Ok(state(resource)?.named_by_name(&name).map(|(id, _)| id))
                })
            }
            CORRECT => {
                let Correct {
                    skill,
                    name,
                    work,
                    how,
                    form,
                    model,
                } = read_as(given)?;
                self.names_linked_model(person, model.as_deref())?;
                self.change_skills(person, workspace, by, |resource| {
                    let id = permitted_skill(resource, &skill, &permits)?;
                    let correct = skills::CorrectSkill {
                        skill: id.clone(),
                        name,
                        work,
                        how,
                        form,
                        model,
                    };
                    applied(resource.apply(Skills::CORRECT_SKILL.with(correct)))?;
                    Ok(Some(id))
                })
            }
            KEEP_EXAMPLE => {
                let KeepExample {
                    skill,
                    text,
                    accepted,
                    note,
                } = read_as(given)?;
                self.change_skills(person, workspace, by, |resource| {
                    let id = permitted_skill(resource, &skill, &permits)?;
                    let keep = skills::KeepExample {
                        skill: id.clone(),
                        text,
                        accepted,
                        note,
                    };
                    applied(resource.apply(Skills::KEEP_EXAMPLE.with(keep)))?;
                    Ok(Some(id))
                })
            }
            DROP_EXAMPLE => {
                let DropExample { skill, example } = read_as(given)?;
                self.change_skills(person, workspace, by, |resource| {
                    let id = permitted_skill(resource, &skill, &permits)?;
                    let dropped = skills::DropExample {
                        skill: id.clone(),
                        example,
                    };
                    applied(resource.apply(Skills::DROP_EXAMPLE.with(dropped)))?;
                    Ok(Some(id))
                })
            }
            FORGET => {
                let Forget { skill } = read_as(given)?;
                self.change_skills(person, workspace, by, |resource| {
                    let id = permitted_skill(resource, &skill, &permits)?;
                    applied(
                        resource
                            .apply(Skills::FORGET_SKILL.with(skills::ForgetSkill { skill: id })),
                    )?;
                    Ok(None)
                })
            }
            _ => Err(NotChanged::Unknown),
        }
    }

    /// Where it is kept which skill each of `person`'s sub-agents in `workspace` was handed:
    /// in the person's own store, beside their skills of that workspace.
    fn handed_at(&self, person: &[u8; 32], workspace: &str) -> Result<PathBuf, String> {
        // The id as the workspace spells it, never the caller's text: the name of a file.
        let workspace = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| "not a workspace id".to_owned())?;
        Ok(self
            .person_settings_root(person)
            .join(FOLDER)
            .join(format!("{workspace}.{HANDED_FILE}")))
    }

    /// The skill `person`'s sub-agent `sub_agent` in `workspace` was handed: none where it was
    /// handed none. For the code that answers the turns of the person's agents; it decides
    /// which turn is told the skill.
    #[must_use]
    pub fn skill_handed(
        &self,
        person: &[u8; 32],
        workspace: &str,
        sub_agent: &str,
    ) -> Option<HandedSkill> {
        let path = self.handed_at(person, workspace).ok()?;
        handed_kept(&path).remove(sub_agent)
    }

    /// Keep that `person`'s sub-agent `sub_agent` in `workspace` was handed `handed`, or with
    /// none take away what is kept for it. Kept in the person's own store on this server and
    /// nowhere else, readable on disk by the server's own user alone.
    ///
    /// # Errors
    /// `workspace` is no workspace id, or the file cannot be written.
    pub fn hand_skill(
        &self,
        person: &[u8; 32],
        workspace: &str,
        sub_agent: &str,
        handed: Option<HandedSkill>,
    ) -> Result<(), String> {
        let path = self.handed_at(person, workspace)?;
        let one_at_a_time = CHANGING.lock_recover();
        let mut kept = handed_kept(&path);
        match handed {
            Some(handed) => {
                kept.insert(sub_agent.to_owned(), handed);
            }
            None => {
                if kept.remove(sub_agent).is_none() {
                    return Ok(());
                }
            }
        }
        while kept.len() > MOST_HANDED {
            let oldest = kept
                .iter()
                .min_by_key(|(_, handed)| handed.at_ms)
                .map(|(id, _)| id.clone());
            let Some(oldest) = oldest else {
                break;
            };
            kept.remove(&oldest);
        }
        let bytes = serde_json::to_vec(&kept).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        drop(one_at_a_time);
        Ok(())
    }

    /// Refuses a `model` a skill would name that `person` has not linked on this server; a
    /// skill that names none, or takes its model away, names nothing to refuse.
    fn names_linked_model(&self, person: &[u8; 32], model: Option<&str>) -> Result<(), NotChanged> {
        match model.map(str::trim) {
            None | Some("") => Ok(()),
            Some(model) => self
                .person_links_model(person, model)
                .map_err(NotChanged::Refused),
        }
    }

    /// Answer a request about what this server keeps for a person's agents and the person
    /// alone reads and changes on a device of theirs: their skills ([`PROVIDER`]), or what
    /// the agents learned about them ([`super::person_learned`]).
    pub(super) fn person_kept(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.provider == PROVIDER {
            self.person_skills_invoke(workspace_id, caller, certificate, request)
        } else {
            self.person_learned_invoke(workspace_id, caller, certificate, request)
        }
    }

    /// Answer a [`PROVIDER`] request: the person's own skills in the workspace the request
    /// comes through, read or changed by them on a device of theirs and by nobody else.
    fn person_skills_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let workspace = workspace_id.ok_or(UnknownAction)?.to_string();
        if request.target.address.resource != RESOURCE {
            return Err(UnknownAction);
        }
        let action = request.target.address.action.as_str();
        if action != READ {
            let none = Map::new();
            let given = match &request.argument {
                InvokeArgument::Json(given) => given,
                _ => &none,
            };
            self.skills_action(&person, &workspace, None, (action, given))
                .map_err(|not| match not {
                    NotChanged::Unknown | NotChanged::Denied => UnknownAction,
                    NotChanged::Unreadable(reason) => ActionCallError::InvalidRequest { reason },
                    NotChanged::Refused(reason) => ActionCallError::Refused { reason },
                })?;
        }
        answer(&Ok::<_, String>(self.skills_listed(&person, &workspace)))
    }
}

/// Which skill each sub-agent was handed, as the file at `path` keeps it: nothing where none
/// was handed yet, and nothing where the file does not read.
fn handed_kept(path: &Path) -> BTreeMap<String, HandedSkill> {
    let bytes = match read(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(%error, "which skill a person's sub-agents were handed is not read");
            return BTreeMap::new();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(kept) => kept,
        Err(error) => {
            // Where it fails is said and not why: the error's own words may quote the file.
            let (line, column) = (error.line(), error.column());
            tracing::warn!(
                line,
                column,
                "which skill a person's sub-agents were handed does not read as it was kept"
            );
            BTreeMap::new()
        }
    }
}

/// `given` read as what a verb takes.
fn read_as<T: serde::de::DeserializeOwned>(given: &Map<String, Value>) -> Result<T, NotChanged> {
    serde_json::from_value(Value::Object(given.clone()))
        .map_err(|error| NotChanged::Unreadable(error.to_string()))
}

/// `words` as a verb takes them, or `why` they are none.
fn said(words: &str, why: &str) -> Result<NonBlank, NotChanged> {
    NonBlank::parse(words).ok_or_else(|| NotChanged::Refused(why.to_owned()))
}

fn permitted_skill(
    resource: &Replica<Skills>,
    named: &str,
    permits: &impl Fn(Option<&Id>) -> bool,
) -> Result<Id, NotChanged> {
    let id = named_skill(&state(resource)?, named)?;
    permits(Some(&id)).then_some(id).ok_or(NotChanged::Denied)
}

/// The skill `named` names in `kept`: by its id, or by what it is called.
fn named_skill(kept: &Skills, named: &str) -> Result<Id, String> {
    kept.named(named).map(|(id, _)| id).ok_or_else(|| {
        format!("no skill is kept under the id or the name {named:?}: read the list for them")
    })
}

/// How a verb of the resource ended, in words when it refused.
fn applied<T>(ended: Result<T, tilo_actions::ApiError>) -> Result<(), String> {
    ended.map(drop).map_err(|error| error.to_string())
}

/// What `resource` holds now.
fn state(resource: &Replica<Skills>) -> Result<Skills, String> {
    resource.state().map_err(|error| error.to_string())
}

/// The resource `bytes` hold, or a new one where no skill was written yet.
fn opened(bytes: Option<Vec<u8>>) -> Result<Replica<Skills>, String> {
    let Some(bytes) = bytes else {
        return Ok(Replica::new());
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<Skills>(&mut doc).map_err(|error| error.to_string())?;
    Ok(Replica::from_doc(doc))
}

/// The resource kept at `path`, or none where there is none yet.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(kept) if kept.len() > MAX_BYTES => {
            return Err("the list of skills is larger than one is kept".to_owned());
        }
        Ok(_) => {}
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|error| error.to_string())
}
