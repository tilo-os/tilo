//! The asks of a workspace's tilolang records that the agents on this server answer, and the
//! replies they write.
//!
//! An ask is the agent package's resource (`so.tilo.lang.agent`), read and written through
//! [`tilo_workspace_service::asks`]: a mention of a hosted agent in a Chat message asks it, and
//! its answer is the ask's reply, which Chat shows under the message. This server is one of the
//! workspace's devices: it claims an ask's turn before it answers, and answers only where
//! [`tilo_workspace_service::work::takes_turn`] says the turn is its own.
//!
//! What a turn is told of the ask is what sits around what it is about
//! ([`tilo_workspace_service::asks::around`]): the conversation a message is in, read by
//! structure, never by Chat's own fields.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_workspace_agent::Agent;
use tilo_workspace_service::asks::{self, AskState, Asked};
use tilo_workspace_service::tl_records::Packages;
use tilo_workspace_service::work::{self, Taking};

use super::{Ask, Turn};
use crate::workspaces::Workspaces;

/// Whether `app`'s record is one asks are read in: tilolang Chat's, which uses the agent
/// package.
pub(super) fn keeps_asks(app: &str) -> bool {
    packages().is_ok_and(|packages| packages.app() == app)
}

/// How many items around what an ask is about a turn is told.
const AROUND: usize = 30;

/// The packages Chat's record is read through, compiled once for the server.
fn packages() -> Result<Packages, String> {
    static COMPILED: OnceLock<Result<Packages, String>> = OnceLock::new();
    COMPILED.get_or_init(asks::chat_packages).clone()
}

/// The member a record is opened as on this server: none, so nothing a mention in it asks is
/// made here for anyone.
fn viewer() -> asks::Actor {
    asks::Actor::App(tilo_app_id::AppId::new(asks::AGENT_APP))
}

/// This server's key, as it claims an ask's turn.
pub(super) fn device(workspaces: &Workspaces) -> String {
    hex::encode(workspaces.governor().device_public_key)
}

/// The turns `workspace`'s asks owe `agents` here, and the turns their askers stopped while this
/// server worked on them.
///
/// # Errors
/// A record could not be read.
pub(super) async fn turns(
    workspaces: &Workspaces,
    workspace: &str,
    agents: &std::collections::BTreeMap<String, Agent>,
) -> Result<(Vec<Turn>, Vec<Turn>), String> {
    let packages = packages()?;
    let me = device(workspaces);
    let now = tilo_clock::unix_now_ms_i64();
    // Each agent by its id, with its keeper where it has one.
    let wanted: std::collections::BTreeMap<String, Option<tilo_action_policy::MemberId>> = agents
        .iter()
        .map(|(id, agent)| {
            let keeper = super::keeper_of(agent).map(tilo_action_policy::MemberId::person);
            (id.clone(), keeper)
        })
        .collect();
    let read = workspaces
        .with_tl_record(workspace, &packages, viewer(), move |opened| {
            let mut read = Vec::new();
            for asked in asks::asks(opened)? {
                let Some(agent) = asked.agent.agent_id().map(str::to_owned) else {
                    continue;
                };
                let Some(keeper) = wanted.get(&agent) else {
                    continue;
                };
                let around = asked
                    .from
                    .as_ref()
                    .map(|from| asks::around(opened, from, AROUND));
                let private = keeper
                    .as_ref()
                    .is_some_and(|keeper| asks::private_to(opened, &asked, keeper, &asked.agent));
                read.push((agent, asked, around, private));
            }
            Ok(read)
        })
        .await?;
    let mut owed = Vec::new();
    let mut stopped = Vec::new();
    for (agent, asked, around, private) in read {
        let Some(agent) = agents.get(&agent) else {
            continue;
        };
        let taking = work::takes_turn(&asked.claims, &me, now);
        let ours_working = taking == Taking::Mine
            && asked
                .latest()
                .is_some_and(|reply| reply.state == AskState::Working);
        let turn = || turn_of(packages.app(), agent, &asked, around.as_ref(), private);
        match asked.state {
            AskState::Stopped if ours_working => stopped.extend(turn()),
            AskState::Waiting if !matches!(taking, Taking::Theirs(_)) => owed.extend(turn()),
            AskState::Working if taking == Taking::Mine => owed.extend(turn()),
            _ => {}
        }
    }
    // A turn that is its keeper's own, about what nobody else reads, is told what its agent
    // learned about them ([`super::learned`]).
    for turn in &mut owed {
        if let Some(known) = super::learned::learned_for(workspaces, workspace, turn).await {
            turn.request.push_str(&super::learned::told(&known));
        }
    }
    Ok((owed, stopped))
}

/// The turn `asked` gives `agent`, kept in `app`'s record; none for an ask nobody we know made.
fn turn_of(
    app: &str,
    agent: &Agent,
    asked: &Asked,
    around: Option<&asks::Around>,
    private: bool,
) -> Option<Turn> {
    let person = match &asked.who {
        Some(asks::Actor::Member(member)) => member.person_id()?.to_owned(),
        _ => return None,
    };
    let said = asked.said.clone().unwrap_or_default();
    let mut request = String::new();
    if let Some(around) = around {
        if !around.title.trim().is_empty() {
            let _ = writeln!(request, "In \"{}\":", around.title.trim());
        }
        for item in &around.items {
            if item.asked_about {
                continue;
            }
            let who = match &item.who {
                Some(asks::Actor::Member(member)) if member.agent_id() == Some(&agent.id.0) => {
                    "you".to_owned()
                }
                Some(asks::Actor::Member(member)) if member.person_id() == Some(&person) => {
                    "the person asking".to_owned()
                }
                Some(asks::Actor::Member(member)) if member.agent_id().is_some() => {
                    "another agent".to_owned()
                }
                _ => "another member".to_owned(),
            };
            let _ = writeln!(request, "{who}: {}", item.text.trim());
        }
        if !request.is_empty() {
            request.push('\n');
        }
    }
    if asked.said.is_none() {
        request.push_str(
            "You were mentioned where you may not read what was written. Say that you cannot \
             read it, and nothing else.",
        );
    } else {
        request.push_str(&said);
    }
    let attempt = asked
        .replies
        .len()
        .try_into()
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let conversation = around
        .and_then(|around| around.holder.as_ref())
        .or(asked.from.as_ref())
        .map_or_else(|| asked.at.id_string(), asks::ResourceRef::id_string);
    Some(Turn {
        agent: agent.clone(),
        person,
        conversation,
        cause: asked.at.id_string(),
        request,
        history: Vec::new(),
        asked: said,
        typed: true,
        line: None,
        ask: Ask::Asked {
            app: app.to_owned(),
            ask: asked.at.id_string(),
            ty: asked.at.ty.clone(),
            attempt,
            private,
        },
    })
}

/// Where a turn made from an ask writes its reply: the ask, and the reply once the turn took
/// it. Every [`super::Reply`] of one turn shares it.
#[derive(Clone)]
pub(super) struct Replying {
    workspaces: Workspaces,
    workspace: String,
    agent: tilo_action_policy::MemberId,
    ask: asks::ResourceRef,
    reply: Arc<std::sync::Mutex<Option<asks::ResourceRef>>>,
}

impl Replying {
    /// Where `turn`'s reply is written, for a turn made from an ask; `None` for any other.
    pub(super) fn of(workspaces: &Workspaces, workspace: &str, turn: &Turn) -> Option<Self> {
        let Ask::Asked { ask, ty, .. } = &turn.ask else {
            return None;
        };
        Some(Self {
            workspaces: workspaces.clone(),
            workspace: workspace.to_owned(),
            agent: tilo_action_policy::MemberId::agent(turn.agent.id.0.clone()),
            ask: asks::ResourceRef::parse_id(ask, ty.clone())?,
            reply: Arc::default(),
        })
    }

    /// Run `write` on the ask's record and write what it changed as this server's.
    async fn on_record<R: Send + 'static>(
        &self,
        write: impl FnOnce(&mut tilo_workspace_service::tl_records::Opened) -> Result<R, String>
        + Send
        + 'static,
    ) -> Result<R, String> {
        let packages = packages()?;
        self.workspaces
            .with_tl_record(&self.workspace, &packages, viewer(), write)
            .await
    }

    /// Claim the ask's turn for this server, and start its reply where the turn is this
    /// server's: whether it is.
    ///
    /// # Errors
    /// The record could not be read or written.
    pub(super) async fn take(&self) -> Result<bool, String> {
        let me = device(&self.workspaces);
        let (agent, ask) = (self.agent.clone(), self.ask.clone());
        let reply = self
            .on_record(move |opened| {
                let now = tilo_clock::unix_now_ms_i64();
                asks::claim(opened, &agent, &ask, (&me, now))?;
                let asked = asks::asks(opened)?
                    .into_iter()
                    .find(|asked| asked.at.same(&ask))
                    .ok_or("the ask is gone")?;
                if work::takes_turn(&asked.claims, &me, now) != Taking::Mine {
                    return Ok(None);
                }
                asks::answer(opened, &agent, &ask).map(Some)
            })
            .await?;
        let mine = reply.is_some();
        *self.reply.lock_recover() = reply;
        Ok(mine)
    }

    /// Whether the ask's newest reply is still working from an earlier run of this server: it
    /// took the turn and stopped before it ended, and nothing says what its steps did.
    pub(super) async fn left_begun(&self) -> bool {
        let ask = self.ask.clone();
        let me = device(&self.workspaces);
        self.on_record(move |opened| {
            Ok(asks::asks(opened)?.into_iter().any(|asked| {
                asked.at.same(&ask)
                    && work::takes_turn(&asked.claims, &me, tilo_clock::unix_now_ms_i64())
                        == Taking::Mine
                    && asked
                        .latest()
                        .is_some_and(|reply| reply.state == AskState::Working)
            }))
        })
        .await
        .unwrap_or(false)
    }

    /// The reply: the one this run started, else the ask's newest, which an earlier run left.
    async fn reply(&self) -> Result<asks::ResourceRef, String> {
        let started = self.reply.lock_recover().clone();
        if let Some(reply) = started {
            return Ok(reply);
        }
        let ask = self.ask.clone();
        self.on_record(move |opened| {
            asks::asks(opened)?
                .into_iter()
                .find(|asked| asked.at.same(&ask))
                .and_then(|asked| asked.latest().map(|reply| reply.at.clone()))
                .ok_or_else(|| "the ask has no reply".to_owned())
        })
        .await
    }

    /// Record a step the turn took: an action it called, and what it answered.
    pub(super) async fn step(&self, called: &str, answered: &str, failed: bool) {
        let Ok(reply) = self.reply().await else {
            return;
        };
        let agent = self.agent.clone();
        let (called, answered) = (called.to_owned(), answered.to_owned());
        if let Err(error) = self
            .on_record(move |opened| {
                asks::step(opened, &agent, &reply, (&called, &answered, failed))
            })
            .await
        {
            tracing::warn!(workspace = %self.workspace, %error, "a reply did not take its step");
        }
    }

    /// Put `more` at the end of the reply's body: a piece of the answer as its model writes it
    /// ([`super::written`]).
    ///
    /// # Errors
    /// The reply did not take it: it ended, or the record could not be written.
    pub(super) async fn say(&self, more: &str) -> Result<(), String> {
        let reply = self.reply().await?;
        let agent = self.agent.clone();
        let more = more.to_owned();
        self.on_record(move |opened| asks::say(opened, &agent, &reply, &more))
            .await
    }

    /// Put `more` at the end of the reply's thinking: what its model thought, as its provider
    /// shows it ([`super::written`]).
    ///
    /// # Errors
    /// The reply did not take it: it ended, or the record could not be written.
    pub(super) async fn think(&self, more: &str) -> Result<(), String> {
        let reply = self.reply().await?;
        let agent = self.agent.clone();
        let more = more.to_owned();
        self.on_record(move |opened| asks::think(opened, &agent, &reply, &more))
            .await
    }

    /// End the turn as `state`: `words` go onto the end of the reply first, where there are
    /// any ([`super::written::Written::ending`]).
    ///
    /// # Errors
    /// The reply did not take it.
    pub(super) async fn finish(&self, words: &str, state: AskState) -> Result<(), String> {
        let reply = self.reply().await?;
        let agent = self.agent.clone();
        let words = words.to_owned();
        self.on_record(move |opened| {
            if !words.trim().is_empty() {
                asks::say(opened, &agent, &reply, &words)?;
            }
            asks::finish(opened, &agent, &reply, state)
        })
        .await
    }
}
