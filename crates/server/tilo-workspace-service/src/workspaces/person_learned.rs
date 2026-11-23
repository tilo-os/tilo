//! What the agents on this server learned in a workspace, kept where the
//! server reads and writes it with every device off.
//!
//! A workspace server keeps what its agents learn in the workspace's store
//! beside its vault and model providers ([`Workspaces::workspace_root`]). The
//! records use the same [`tilo_workspace_agent::learned::Learned`] resource
//! as devices do, one document per workspace, and each change names who made
//! it and for whom. Each fact names the person it was learned about, and is
//! read, corrected and forgotten for them alone
//! ([`tilo_workspace_agent::learned::Fact::is_about`]). The store is readable on disk by the server's own user.
//! Writes are serialized.
//!
//! Two callers reach it. Code linked into the server, which answers the
//! turns of the workspace's agents and decides first whose turn it is
//! ([`Workspaces::learned_about`], [`Workspaces::change_learned`]). And
//! whoever the workspace's policy lets, through the provider [`PROVIDER`]
//! ([`Store::LEARNED`]): they read their own facts, correct one, forget one,
//! or write one down.
//!
//! Nothing a fact says is written to a log.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::ActionCallError::{self, UnknownAction};
use tilo_actions::remote::{Caller, InvokeRequest, RemoteActionTarget};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Id, NonBlank, Replica};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::learned::{Learned, learned};

use super::Workspaces;
use super::person_settings::{answer, argument};
use super::workspace_stores::Store;

/// The provider a workspace's members read and change what its agents learned by.
pub const PROVIDER: &str = super::actions::LEARNED;
/// Its one resource: the facts, in the workspace the request comes through.
pub const RESOURCE: &str = "facts";
/// Read the facts: answers [`Facts`].
pub const LIST: &str = "list";
/// Write a fact down oneself, `{ text, source }`: answers [`Facts`].
pub const LEARN: &str = "learn";
/// Correct a fact, `{ fact, text, source? }`: answers [`Facts`].
pub const CORRECT: &str = "correct";
/// Forget a fact, `{ fact }`: answers [`Facts`].
pub const FORGET: &str = "forget";

/// The app whose resource this is, as every change of it names it.
const CHAT: (&str, &str) = ("so.tilo.chat", "Chat");

/// The folder of a workspace's store its learned resource is kept in.
const FOLDER: &str = "learned";

/// The most bytes one kept resource holds. A few hundred short facts with their history stay far
/// below it, and a file past it is neither read nor written.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

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

/// What the provider answers: the facts this server keeps in one workspace,
/// newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    /// The workspace they were learned in.
    pub workspace: String,
    /// What the resource stands at: the same until a fact is kept, corrected or forgotten, and
    /// empty while nothing was learned. A reader that holds a list reads again when it differs.
    pub revision: String,
    /// The facts, newest first.
    pub facts: Vec<FactView>,
}

/// One fact, as a member reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactView {
    /// Its id: what `correct` and `forget` name it by.
    pub id: String,
    /// What was learned.
    pub text: String,
    /// Where it came from: the agent's words, closed by the message of the turn that kept it
    /// where an agent on this server did.
    pub source: String,
    /// When it was learned or last corrected, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The agent that wrote it down, by its id; absent where a person wrote it themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Learn {
    text: String,
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Correct {
    fact: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forget {
    fact: String,
}

impl Workspaces {
    /// Where `workspace`'s learned resource is kept.
    fn learned_at(&self, workspace: &str) -> Result<PathBuf, String> {
        // The id as the workspace spells it, never the caller's text: the name of a file.
        let workspace = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| "not a workspace id".to_owned())?;
        Ok(self
            .workspace_root(&workspace.to_string())
            .join(FOLDER)
            .join(format!("{workspace}.automerge")))
    }

    /// The resource this server keeps in `workspace` and what it stands at; none where nothing
    /// was learned yet, and none where it does not read.
    fn learned_kept(&self, workspace: &str) -> Option<(Learned, String)> {
        let bytes = match self.learned_at(workspace).and_then(|path| read(&path)) {
            Ok(bytes) => bytes?,
            Err(error) => {
                tracing::warn!(%workspace, %error, "what was learned in a workspace is not read");
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
            tracing::warn!(%workspace, "what was learned in a workspace does not read as its resource");
        }
        read.map(|learned| (learned, revision))
    }

    /// What this server's agents learned in `workspace`: none where nothing was learned yet. For
    /// the code that answers the turns of the workspace's agents; it decides whose turn is told
    /// it.
    #[must_use]
    pub fn learned_about(&self, workspace: &str) -> Option<Learned> {
        self.learned_kept(workspace).map(|(learned, _)| learned)
    }

    /// Change what this server keeps in `workspace` through the resource's own verbs: `change`
    /// applies them, as the agent `by` acting for `person`, or as `person` themselves where `by`
    /// is none, stamped with the time now. One change is made at a time,
    /// the first makes the resource, and one that fails keeps the resource as it was.
    ///
    /// # Errors
    /// `workspace` is no workspace id, the resource cannot be read or written, it would outgrow
    /// what one resource holds, or `change` refused.
    pub fn change_learned<R>(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        change: impl FnOnce(&mut Replica<Learned>) -> Result<R, String>,
    ) -> Result<R, String> {
        let path = self.learned_at(workspace)?;
        let acts_for = hex::encode(person);
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let mut resource = match by {
            Some(agent) => resource.as_agent_for(agent, &acts_for),
            None => resource.as_author(&acts_for),
        }
        .in_app(CHAT.0, CHAT.1);
        resource.set_now_ms(tilo_clock::unix_now_ms_i64());
        let answer = change(&mut resource)?;
        let bytes = resource.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err("the list of what was learned is full".to_owned());
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        drop(one_at_a_time);
        Ok(answer)
    }

    /// Keep `text` as a fact for `person` in `workspace`, with where it came from in words,
    /// as server code that read it from the workspace's resources: `by` is the id it writes under,
    /// one no agent has. A model's words never come this way: a turn keeps what it learns
    /// through its own tools, which decide first whose turn it is.
    ///
    /// # Errors
    /// The fact or its source is not one the resource keeps, or as [`Self::change_learned`].
    pub fn keep_learned_fact(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: &str,
        (text, source): (&str, &str),
    ) -> Result<(), String> {
        let learn = learned::Learn {
            text: said(text)?,
            source: said(source)?,
        };
        self.change_learned(person, workspace, Some(by), |resource| {
            applied(resource.apply(Learned::LEARN.with(learn)))
        })
    }

    /// The facts this server keeps in `workspace` about `person`, as the provider answers them.
    fn learned_facts(&self, workspace: &str, person: &[u8; 32]) -> Facts {
        let (learned, revision) = self.learned_kept(workspace).unwrap_or_default();
        let person = hex::encode(person);
        let mut facts: Vec<FactView> = learned
            .facts
            .iter()
            .filter(|(_, fact)| fact.is_about(&person))
            .map(|(id, fact)| FactView {
                id: id.to_string(),
                text: fact.text.clone(),
                source: fact.source.clone(),
                at_ms: fact.at,
                by: fact.by.clone(),
            })
            .collect();
        facts.sort_by_key(|fact| std::cmp::Reverse(fact.at_ms));
        Facts {
            workspace: workspace.to_owned(),
            revision,
            facts,
        }
    }

    /// Answer a [`PROVIDER`] request: the list of the workspace the request comes through, read
    /// or changed as the workspace's policy lets the caller.
    pub(super) async fn person_learned_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.address.resource != RESOURCE {
            return Err(UnknownAction);
        }
        let action = request.target.address.action.as_str();
        let effect = if action == LIST {
            tilo_action_policy::ActionKind::Read
        } else {
            tilo_action_policy::ActionKind::Undoable
        };
        let reach = self
            .reaching_allowed(
                (workspace_id, caller, certificate),
                Store::LEARNED,
                action,
                effect,
                request,
            )
            .await?;
        let workspace = reach.workspace.to_string();
        let person = reach.person;
        let by = reach.agent.as_deref();
        let changed = match request.target.address.action.as_str() {
            LIST => Ok(()),
            LEARN => {
                let Learn { text, source } = argument(request, "a fact with where it came from")?;
                self.change_learned(&person, &workspace, by, |resource| {
                    let learn = learned::Learn {
                        text: said(&text)?,
                        source: said(&source)?,
                    };
                    applied(resource.apply(Learned::LEARN.with(learn)))
                })
            }
            CORRECT => {
                let Correct { fact, text, source } =
                    argument(request, "the fact to correct and what it says now")?;
                self.change_learned(&person, &workspace, by, |resource| {
                    let correct = learned::Correct {
                        fact: Id::from_raw(fact),
                        text: said(&text)?,
                        source,
                    };
                    applied(resource.apply(Learned::CORRECT.with(correct)))
                })
            }
            FORGET => {
                let Forget { fact } = argument(request, "the fact to forget")?;
                self.change_learned(&person, &workspace, by, |resource| {
                    let forget = learned::Forget {
                        fact: Id::from_raw(fact),
                    };
                    applied(resource.apply(Learned::FORGET.with(forget)))
                })
            }
            _ => return Err(UnknownAction),
        };
        answer(&changed.map(|()| self.learned_facts(&workspace, &person)))
    }
}

/// `words` as a verb takes them: something, on one line.
fn said(words: &str) -> Result<NonBlank, String> {
    NonBlank::parse(words).ok_or_else(|| "a fact says something and where it came from".to_owned())
}

/// How a verb of the resource ended, in words when it refused.
fn applied<T>(ended: Result<T, tilo_actions::ApiError>) -> Result<(), String> {
    ended.map(drop).map_err(|error| error.to_string())
}

/// What `resource` holds now.
fn state(resource: &Replica<Learned>) -> Result<Learned, String> {
    resource.state().map_err(|error| error.to_string())
}

/// The resource `bytes` hold, or a new one where nothing was learned yet.
fn opened(bytes: Option<Vec<u8>>) -> Result<Replica<Learned>, String> {
    let Some(bytes) = bytes else {
        return Ok(Replica::new());
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<Learned>(&mut doc).map_err(|error| error.to_string())?;
    Ok(Replica::from_doc(doc))
}

/// The resource kept at `path`, or none where there is none yet.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(kept) if kept.len() > MAX_BYTES => {
            return Err("the list of what was learned is larger than one is kept".to_owned());
        }
        Ok(_) => {}
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|error| error.to_string())
}
