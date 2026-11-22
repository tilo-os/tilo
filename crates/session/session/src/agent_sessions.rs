//! The agent sessions served here: one per connection the person admitted at the session's MCP
//! server, each its own presence, history and Stop; and one per conversation an agent is asked a
//! turn in, for as long as that turn goes.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilo_action_policy::AgentSessionId;
use tilo_broker_core::services::AgentTurn;
use tilo_lock::MutexRecover as _;
use tilo_mcp::session_api::Consent;

/// The agent sessions served here, from the moment a client connects until its connection ends.
pub(crate) struct AgentSessions {
    inner: std::sync::Mutex<Inner>,
    /// Wakes the session's loop when a turn reported what it spent, so the day it is charged to is
    /// the day the agent then works in.
    wake: std::sync::OnceLock<tilo_service::Wake>,
}

#[derive(Default)]
struct Inner {
    /// Bumped on every change, so a reading made before it is not mistaken for the shell now.
    revision: u64,
    sessions: BTreeMap<AgentSessionId, Connection>,
    /// The turns the conversations of each workspace held here give, as the last pass read them.
    turns: BTreeMap<String, Vec<AgentTurn>>,
    /// The sessions the turns are answered in, by id.
    answering: BTreeMap<String, Answering>,
    /// What the turns' agents spent since the last pass, in millionths of the currency, by
    /// workspace and agent: the pass charges it to their day.
    spent: BTreeMap<(String, String), u64>,
    /// The turns whose answer ended without one, by session: the pass records each on the message
    /// it answered.
    failed: BTreeMap<String, (tilo_action_policy::turn::Turn, u32, String)>,
}

/// The session an agent answers a conversation's turns in, while one of them goes.
struct Answering {
    workspace: String,
    agent: String,
    /// What the person allowed it for every later step like one it asked about.
    answers: Vec<tilo_action_policy::Grant>,
    /// The total the turn last reported spending, so a report charges only what it added.
    reported: u64,
}

#[derive(Default)]
struct Connection {
    /// The agent member it is a session of: empty until the person admits it.
    agent: String,
    /// The program the person's answer was remembered for (`claude-code`), which taking that
    /// answer back names.
    program: String,
    /// What the shell calls it (`Claude Code`).
    name: String,
    workspace_name: String,
    workspace: String,
    doing: Option<Doing>,
    /// The environment's server it came in by: none for the session's own. It ends with that
    /// environment.
    environment: Option<Entered>,
    /// What the person allowed it for every later step like one it asked about.
    answers: Vec<tilo_action_policy::Grant>,
    /// Whether it works through the broker as its own agent session: what taking its work back
    /// names it by. `false` where it acts on the workspace's grants as any app does.
    certified: bool,
    /// The records it changed, each as the app that keeps it and the record: what taking back its
    /// work undoes.
    changed: Vec<(String, String)>,
    /// The connection itself, shut when the person stops the session.
    stream: Option<UnixStream>,
}

impl AgentSessions {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(Inner::default()),
            wake: std::sync::OnceLock::new(),
        })
    }

    /// Wake the session's loop through `wake` when a turn reports what it spent.
    pub(crate) fn wake_on_spend(&self, wake: tilo_service::Wake) {
        let _ = self.wake.set(wake);
    }

    /// The turns the conversations of each workspace held here give now. Whether they differ from
    /// the ones read before: a session whose turns all ended is gone, with what the person allowed
    /// it.
    pub(crate) fn asked(&self, workspaces: Vec<(String, Vec<AgentTurn>)>) -> bool {
        let mut inner = self.inner.lock_recover();
        let mut turns = BTreeMap::new();
        let mut answering = BTreeMap::new();
        for (workspace, read) in workspaces {
            let mut held = Vec::with_capacity(read.len());
            for turn in read {
                if !answering.contains_key(&turn.session) {
                    let kept = inner.answering.remove(&turn.session);
                    let (answers, reported) = kept
                        .map(|kept| (kept.answers, kept.reported))
                        .unwrap_or_default();
                    answering.insert(
                        turn.session.clone(),
                        Answering {
                            workspace: workspace.clone(),
                            agent: turn.agent.clone(),
                            answers,
                            reported,
                        },
                    );
                }
                held.push(turn);
            }
            turns.insert(workspace, held);
        }
        inner.answering = answering;
        let changed = inner.turns != turns;
        inner.turns = turns;
        changed
    }

    /// What the turns' agents spent since the last pass, by workspace and agent, in millionths of
    /// the currency, ready to be charged to their day.
    pub(crate) fn take_spent(&self) -> Vec<(String, String, u64)> {
        std::mem::take(&mut self.inner.lock_recover().spent)
            .into_iter()
            .map(|((workspace, agent), cost)| (workspace, agent, cost))
            .collect()
    }

    /// The turns whose answer ended without one since the last pass: the agent that answered, the
    /// turn, the time it was asked and why its answer ended.
    pub(crate) fn take_failed(&self) -> Vec<(String, tilo_action_policy::turn::Turn, u32, String)> {
        let mut inner = self.inner.lock_recover();
        std::mem::take(&mut inner.failed)
            .into_iter()
            .filter_map(|(session, (turn, attempt, reason))| {
                inner
                    .answering
                    .get(&session)
                    .map(|answering| (answering.agent.clone(), turn, attempt, reason))
            })
            .collect()
    }

    /// A new connection, by the environment's server `environment` (none for the session's own),
    /// before the person answers for it: nothing of it shows until it is admitted.
    pub(crate) fn joining(&self, environment: Option<Entered>) -> AgentSessionId {
        let session = new_session();
        self.inner.lock_recover().sessions.insert(
            session.clone(),
            Connection {
                environment,
                ..Connection::default()
            },
        );
        session
    }

    /// The person's answer for `session`: admitted, it is a session of the agent member `consent`
    /// names, of the program `program`, working in the workspace named `workspace_name`.
    pub(crate) fn admit(
        &self,
        session: &AgentSessionId,
        consent: &Consent,
        program: &str,
        workspace_name: &str,
    ) {
        let Consent::Admit {
            workspace,
            agent,
            name,
            ..
        } = consent
        else {
            return;
        };
        let mut inner = self.inner.lock_recover();
        let Some(connection) = inner.sessions.get_mut(session) else {
            // It ended while the person was answering: nothing is served.
            return;
        };
        agent.clone_into(&mut connection.agent);
        program.clone_into(&mut connection.program);
        name.clone_into(&mut connection.name);
        workspace.clone_into(&mut connection.workspace);
        workspace_name.clone_into(&mut connection.workspace_name);
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// `session` takes the step `doing`. Whether that is news.
    pub(crate) fn acting(&self, session: &AgentSessionId, doing: Doing) -> bool {
        let mut inner = self.inner.lock_recover();
        let Some(connection) = inner.sessions.get_mut(session) else {
            return false;
        };
        let doing = Some(doing);
        if connection.doing == doing {
            return false;
        }
        connection.doing = doing;
        inner.revision = inner.revision.wrapping_add(1);
        true
    }

    /// The admitted sessions and the workspace agents running here, each once: a workspace's
    /// agent is at work while a turn of it goes.
    pub(crate) fn here(&self) -> Vec<Present> {
        let inner = self.inner.lock_recover();
        let here = inner
            .sessions
            .iter()
            .filter(|(_, connection)| !connection.agent.is_empty())
            .map(|(session, connection)| Present {
                session: session.clone(),
                agent: connection.agent.clone(),
                name: connection.name.clone(),
                workspace: connection.workspace.clone(),
                workspace_name: connection.workspace_name.clone(),
                doing: connection.doing.clone(),
                working: connection.doing.is_some(),
                environment: connection.environment.clone(),
            })
            .collect();
        drop(inner);
        here
    }

    /// The session that `speaker` (this device) speaks for on the presence board as `member`, by
    /// its key in hex.
    pub(crate) fn spoken_as(&self, speaker: [u8; 32], member: &str) -> Option<AgentSessionId> {
        self.inner
            .lock_recover()
            .sessions
            .keys()
            .find(|session| {
                hex::encode(collab_types::presence::stand_in(speaker, session.as_str())) == member
            })
            .cloned()
    }

    /// `session` works through the broker as its own agent session.
    pub(crate) fn certified(&self, session: &AgentSessionId) {
        if let Some(connection) = self.inner.lock_recover().sessions.get_mut(session) {
            connection.certified = true;
        }
    }

    /// `session` changed `record` of `app`: one of the records taking its work back undoes.
    pub(crate) fn changed(&self, session: &AgentSessionId, app: &str, record: &str) {
        let step = (app.to_owned(), record.to_owned());
        let mut inner = self.inner.lock_recover();
        if let Some(connection) = inner.sessions.get_mut(session)
            && !connection.changed.contains(&step)
        {
            connection.changed.push(step);
        }
    }

    /// What `session` works in. `None` for a session not served here.
    pub(crate) fn working(&self, session: &AgentSessionId) -> Option<Working> {
        self.inner
            .lock_recover()
            .sessions
            .get(session)
            .map(|connection| Working {
                workspace: connection.workspace.clone(),
                program: connection.program.clone(),
                session: connection.certified.then(|| session.clone()),
                changed: connection.changed.clone(),
                app: connection
                    .doing
                    .as_ref()
                    .and_then(|doing| doing.app.clone()),
            })
    }

    /// What the shell calls `session`, once admitted.
    pub(crate) fn name(&self, session: &AgentSessionId) -> Option<String> {
        self.inner
            .lock_recover()
            .sessions
            .get(session)
            .map(|connection| connection.name.clone())
            .filter(|name| !name.is_empty())
    }

    /// The connection `session` is served on, which stopping it closes.
    pub(crate) fn attached(&self, session: &AgentSessionId, stream: &UnixStream) {
        let copy = match stream.try_clone() {
            Ok(copy) => Some(copy),
            Err(error) => {
                // Without its own handle Stop cannot end this connection; the client keeps it
                // until it leaves by itself.
                tracing::warn!(%error, "an agent session's connection could not be duplicated");
                None
            }
        };
        if let Some(connection) = self.inner.lock_recover().sessions.get_mut(session) {
            connection.stream = copy;
        }
    }

    /// The connection of `session` ended.
    pub(crate) fn left(&self, session: &AgentSessionId) {
        let mut inner = self.inner.lock_recover();
        if inner.sessions.remove(session).is_none() {
            return;
        }
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// End `session`, as the person's Stop does. Whether it was served here.
    pub(crate) fn stop(&self, session: &AgentSessionId) -> bool {
        self.end(|id, _| id == session)
    }

    /// End every session of the program `program`, as taking back the person's answer does.
    pub(crate) fn stop_program(&self, program: &str) -> bool {
        self.end(|_, connection| connection.program == program)
    }

    /// End every session that came in by the environment's server `environment`, as that
    /// environment ends.
    pub(crate) fn stop_environment(&self, environment: u64) -> bool {
        self.end(|_, connection| {
            connection
                .environment
                .as_ref()
                .is_some_and(|entered| entered.socket == environment)
        })
    }

    /// End every session `ending` picks. Whether there was one.
    fn end(&self, ending: impl Fn(&AgentSessionId, &Connection) -> bool) -> bool {
        let ended: Vec<Connection> = {
            let mut inner = self.inner.lock_recover();
            let picked: Vec<AgentSessionId> = inner
                .sessions
                .iter()
                .filter(|(id, connection)| ending(id, connection))
                .map(|(id, _)| id.clone())
                .collect();
            if picked.is_empty() {
                return false;
            }
            let ended = picked
                .iter()
                .filter_map(|id| inner.sessions.remove(id))
                .collect();
            inner.revision = inner.revision.wrapping_add(1);
            ended
        };
        for connection in ended {
            if let Some(stream) = connection.stream {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        true
    }

    /// The sessions as `session:agents` lists them.
    pub(crate) fn readings(&self) -> Vec<Agent> {
        self.here()
            .into_iter()
            .map(|present| {
                // Where it works from is said beside its name, never inside it: two sessions of
                // one client are told apart by it, whatever each is called.
                let (environment, environment_id) = present
                    .environment
                    .map(|entered| (entered.name, entered.id))
                    .unwrap_or_default();
                Agent {
                    key: present.session.0,
                    name: present.name,
                    state: "running".to_owned(),
                    doing: present.workspace_name,
                    mark: String::new(),
                    unseen: 0,
                    environment,
                    environment_id,
                }
            })
            .collect()
    }

    /// Changes only when a session was admitted, acted or left.
    pub(crate) fn revision(&self) -> u64 {
        self.inner.lock_recover().revision
    }
}

impl tilo_broker_core::services::AgentSessions for AgentSessions {
    fn going(
        &self,
        workspace: &str,
        agent: &str,
        session: &str,
    ) -> Option<tilo_broker_core::services::GoingSession> {
        let inner = self.inner.lock_recover();
        let connection = inner
            .sessions
            .get(&AgentSessionId::new(session))
            .filter(|connection| connection.workspace == workspace && connection.agent == agent)
            .map(|connection| tilo_broker_core::services::GoingSession {
                origin: None,
                answers: connection.answers.clone(),
            });
        let going = connection.or_else(|| {
            inner
                .answering
                .get(session)
                .filter(|answering| answering.workspace == workspace && answering.agent == agent)
                .map(|answering| tilo_broker_core::services::GoingSession {
                    origin: None,
                    answers: answering.answers.clone(),
                })
        });
        drop(inner);
        going
    }

    fn keep_answer(&self, session: &str, grant: tilo_action_policy::Grant) {
        let mut inner = self.inner.lock_recover();
        let answers = match inner.sessions.get_mut(&AgentSessionId::new(session)) {
            Some(connection) => &mut connection.answers,
            None => match inner.answering.get_mut(session) {
                Some(answering) => &mut answering.answers,
                None => return,
            },
        };
        answers.retain(|kept| kept.id != grant.id);
        answers.push(grant);
        drop(inner);
    }

    /// An agent works only inside its turns in a workspace held here, never on a workspace grant
    /// alone: what it may do is its person's grants inside the turn.
    fn confines(&self, workspace: &str) -> bool {
        self.inner.lock_recover().turns.contains_key(workspace)
    }

    fn turns(&self, workspace: &str) -> Vec<AgentTurn> {
        self.inner
            .lock_recover()
            .turns
            .get(workspace)
            .cloned()
            .unwrap_or_default()
    }

    /// Charged to the agent's day by the pass, and only for what the turn added to the total it
    /// reported before.
    fn spent(&self, session: &str, total_micros: u64) {
        {
            let mut inner = self.inner.lock_recover();
            let Some(answering) = inner.answering.get_mut(session) else {
                // The turn is over: what it spent was charged when the pass read the report before.
                return;
            };
            let added = total_micros.saturating_sub(answering.reported);
            answering.reported = answering.reported.max(total_micros);
            if added == 0 {
                return;
            }
            let key = (answering.workspace.clone(), answering.agent.clone());
            let cost = inner.spent.entry(key).or_default();
            *cost = cost.saturating_add(added);
            drop(inner);
        }
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    /// Kept for the pass, which records it on the turn's message where Chat's record is open.
    fn turn_failed(
        &self,
        session: &str,
        turn: &tilo_action_policy::turn::Turn,
        attempt: u32,
        reason: &str,
    ) {
        {
            let mut inner = self.inner.lock_recover();
            if !inner.answering.contains_key(session) {
                return;
            }
            inner.failed.insert(
                session.to_owned(),
                (turn.clone(), attempt, reason.to_owned()),
            );
        }
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }
}

/// A session id of its own.
fn new_session() -> AgentSessionId {
    AgentSessionId::new(format!(
        "{}{}",
        tilo_broker_core::services::AGENT_SESSION_PREFIX,
        uuid::Uuid::new_v4().simple()
    ))
}

/// The environment's server a connection came in by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entered {
    /// The server's number among those this session opened: what its sessions are stopped by.
    pub(crate) socket: u64,
    /// The environment's id on this device, `<workspace>/<record>`.
    pub(crate) id: String,
    /// What the person calls the environment.
    pub(crate) name: String,
}

/// An admitted agent session, as presence speaks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Present {
    pub(crate) session: AgentSessionId,
    /// The agent member it is a session of: the key of its colour.
    pub(crate) agent: String,
    /// What the shell calls it (`Claude Code`).
    pub(crate) name: String,
    /// The workspace it works in, by id.
    pub(crate) workspace: String,
    pub(crate) workspace_name: String,
    pub(crate) doing: Option<Doing>,
    /// It is at work: it took a step, or a turn of it goes.
    pub(crate) working: bool,
    /// The environment whose server it came in by: none for the session's own.
    pub(crate) environment: Option<Entered>,
}

/// A step of an agent session, as its workspace says it beside its face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Doing {
    /// The action's name in the catalogue ("Replace text at a place"); the action's id where the
    /// catalogue names none.
    pub(crate) action: String,
    /// The kind of step the action declares.
    pub(crate) step: Option<tilo_actions::StepKind>,
    /// The app it acts in, when its provider says which one.
    pub(crate) app: Option<String>,
}

/// What the commands on an agent session's card act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Working {
    /// The workspace it works in, by id.
    pub(crate) workspace: String,
    /// The program the person's answer was remembered for.
    pub(crate) program: String,
    /// The agent session its changes are grouped by: `None` where it acts on the workspace's
    /// grants as any app does.
    pub(crate) session: Option<AgentSessionId>,
    /// The records it changed, each as the app that keeps it and the record.
    pub(crate) changed: Vec<(String, String)>,
    /// The app it is working in now, when it says.
    pub(crate) app: Option<String>,
}

/// The agent sessions, as `session:agents` publishes them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Agents {
    pub sessions: Vec<Agent>,
}

/// One agent session.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Agent {
    /// The agent session.
    pub key: String,
    pub name: String,
    /// `running`, `waiting` (for the person) or `done`.
    pub state: String,
    pub doing: String,
    pub mark: String,
    pub unseen: i64,
    /// The environment it works in, when it came in by an environment's own server: what the
    /// person calls that environment. Empty for a session that came in by the session's own.
    pub environment: String,
    /// That environment's id on this device, `<workspace>/<record>`, as the environment's own
    /// programs read it.
    pub environment_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit(sessions: &AgentSessions, session: &AgentSessionId, name: &str) {
        let consent = Consent::Admit {
            workspace: "w1".to_owned(),
            agent: "agent-1".to_owned(),
            person: "p1".to_owned(),
            name: name.to_owned(),
        };
        sessions.admit(session, &consent, "claude-code", "Studio");
    }

    /// Two connections of one agent are two sessions: each its own presence, and Stop ends the
    /// one stopped.
    #[test]
    fn each_connection_is_a_session_of_its_own_and_stop_ends_one() {
        let sessions = AgentSessions::new();
        let (first, _first_peer) = UnixStream::pair().unwrap();
        let (second, mut second_peer) = UnixStream::pair().unwrap();
        let one = sessions.joining(None);
        assert!(
            sessions.readings().is_empty(),
            "nothing shows before it is admitted"
        );
        let shown = sessions.revision();
        admit(&sessions, &one, "Claude Code");
        sessions.attached(&one, &first);
        assert!(sessions.revision() > shown, "the shell is told");
        let two = sessions.joining(None);
        admit(&sessions, &two, "Claude Code");
        sessions.attached(&two, &second);
        let here = sessions.here();
        assert_eq!(here.len(), 2, "one presence per session");
        assert!(
            here.iter().all(|present| present.agent == "agent-1"),
            "one agent"
        );
        assert_ne!(one, two);

        let doing = Doing {
            action: "Replace text at a place".to_owned(),
            step: None,
            app: Some("so.tilo.notes".to_owned()),
        };
        assert!(sessions.acting(&two, doing.clone()), "its step is news");
        assert!(!sessions.acting(&two, doing), "the same step, once");

        sessions.certified(&two);
        sessions.changed(&two, "so.tilo.notes", "main");
        sessions.changed(&two, "so.tilo.notes", "main");
        let working = sessions.working(&two).expect("served here");
        assert_eq!(working.session.as_ref(), Some(&two));
        assert_eq!(
            working.changed,
            [("so.tilo.notes".to_owned(), "main".to_owned())]
        );
        assert_eq!(working.app.as_deref(), Some("so.tilo.notes"));
        assert_eq!(
            sessions.working(&one).unwrap().session,
            None,
            "it acts on the workspace's grants"
        );

        assert!(sessions.stop(&two), "the person's Stop");
        let mut byte = [0_u8; 1];
        assert_eq!(
            std::io::Read::read(&mut second_peer, &mut byte).unwrap(),
            0,
            "its connection is closed"
        );
        let left: Vec<_> = sessions
            .here()
            .into_iter()
            .map(|present| present.session)
            .collect();
        assert_eq!(left, [one], "the other session stays");
        assert!(!sessions.stop(&two), "nothing to stop twice");
        assert!(
            sessions.stop_program("claude-code"),
            "taking the answer back ends the rest"
        );
        assert!(sessions.here().is_empty());
    }

    /// A session that came in by an environment's own server is listed with that environment
    /// beside its name, by what the person calls it and by its id, and one that came in by the
    /// session's own server with none. It ends with its environment, and only it does.
    #[test]
    fn a_session_from_an_environment_is_listed_with_that_environment_beside_its_name() {
        let sessions = AgentSessions::new();
        let entered = Entered {
            socket: 7,
            id: "w1/Dev.Box".to_owned(),
            name: "Dev box".to_owned(),
        };
        let inside = sessions.joining(Some(entered));
        admit(&sessions, &inside, "Claude Code");
        let outside = sessions.joining(None);
        admit(&sessions, &outside, "Claude Code");
        let listed = sessions.readings();
        let listed = |session: &AgentSessionId| {
            listed
                .iter()
                .find(|agent| agent.key == session.as_str())
                .map(|agent| (agent.environment.clone(), agent.environment_id.clone()))
        };
        assert_eq!(
            listed(&inside),
            Some(("Dev box".to_owned(), "w1/Dev.Box".to_owned()))
        );
        assert_eq!(
            listed(&outside),
            Some((String::new(), String::new())),
            "a program outside every environment came in by none"
        );

        assert!(sessions.stop_environment(7), "its environment ended");
        let left: Vec<String> = sessions
            .readings()
            .into_iter()
            .map(|agent| agent.key)
            .collect();
        assert_eq!(
            left,
            [outside.as_str().to_owned()],
            "only the session that came in by it ended"
        );
    }

    /// A session's presence is found again by the key this device speaks for it under.
    #[test]
    fn a_session_is_found_by_its_presence_key() {
        let sessions = AgentSessions::new();
        let one = sessions.joining(None);
        admit(&sessions, &one, "Claude Code");
        let speaker = [7; 32];
        let member = hex::encode(collab_types::presence::stand_in(speaker, one.as_str()));
        assert_eq!(sessions.spoken_as(speaker, &member), Some(one));
        assert_eq!(sessions.spoken_as([8; 32], &member), None);
    }

    /// A turn of `agent` in `session`, answering the conversation's first message.
    fn asked(session: &str, agent: &str) -> AgentTurn {
        AgentTurn {
            session: session.into(),
            agent: agent.into(),
            author: "me".into(),
            turn: tilo_action_policy::turn::Turn::new("chat.workspace/channels/c_tide", "m1"),
            attempt: 0,
            request: runtime_collab::TurnRequest {
                message: "chat.workspace/channels/c_tide/messages/m1".into(),
                title: "Tide".into(),
                instruction: "add the revenue curve".into(),
                agent_instruction: None,
                fold: None,
                subject: None,
                spend: None,
            },
            asked: "add the revenue curve".into(),
        }
    }

    /// A turn's session goes while the turn does, in the workspace it was asked in, and keeps
    /// what the person allowed it until its turns end.
    #[test]
    fn a_turns_session_goes_while_its_turn_does() {
        use tilo_broker_core::services::AgentSessions as _;
        let sessions = AgentSessions::new();
        let held = vec![("w1".to_owned(), vec![asked("agent-session:t1", "scribe")])];
        assert!(sessions.asked(held.clone()));
        assert!(
            sessions.confines("w1"),
            "an agent here acts only in its turns"
        );
        assert!(!sessions.confines("w2"), "a workspace not held here is not");
        assert_eq!(sessions.turns("w1").len(), 1);
        assert!(sessions.turns("w2").is_empty());
        assert!(sessions.going("w1", "scribe", "agent-session:t1").is_some());
        assert!(sessions.going("w1", "other", "agent-session:t1").is_none());
        assert!(sessions.going("w2", "scribe", "agent-session:t1").is_none());
        let grant = tilo_action_policy::Grant::new(
            "session-answer:agent-session:t1",
            tilo_action_policy::Grantee::person("me"),
            tilo_action_policy::TreePath::workspace("w1"),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person("me"),
        );
        sessions.keep_answer("agent-session:t1", grant.clone());
        assert!(!sessions.asked(held));
        assert_eq!(
            sessions
                .going("w1", "scribe", "agent-session:t1")
                .map(|going| going.answers),
            Some(vec![grant]),
            "the answer lasts as long as the session"
        );
        assert!(sessions.asked(vec![("w1".to_owned(), Vec::new())]));
        assert!(sessions.going("w1", "scribe", "agent-session:t1").is_none());
    }

    /// What a turn reports it spent is charged to its agent for what each report added.
    #[test]
    fn a_turns_spend_is_charged_to_its_agent_for_what_each_report_added() {
        use tilo_broker_core::services::AgentSessions as _;
        let sessions = AgentSessions::new();
        sessions.asked(vec![(
            "w1".to_owned(),
            vec![asked("agent-session:t1", "scribe")],
        )]);
        sessions.spent("agent-session:t1", 420_000);
        assert_eq!(
            sessions.take_spent(),
            vec![("w1".to_owned(), "scribe".to_owned(), 420_000)]
        );
        assert_eq!(sessions.take_spent(), Vec::new(), "a charge is taken once");
        sessions.spent("agent-session:t1", 420_000);
        assert_eq!(
            sessions.take_spent(),
            Vec::new(),
            "a repeated total adds nothing"
        );
        sessions.spent("agent-session:t1", 900_000);
        assert_eq!(
            sessions.take_spent(),
            vec![("w1".to_owned(), "scribe".to_owned(), 480_000)],
            "the turn's total, not its last call's, is what its agent spent"
        );
        sessions.spent("agent-session:other", 100);
        assert_eq!(
            sessions.take_spent(),
            Vec::new(),
            "a turn this session does not hold is not charged"
        );
    }

    /// A turn whose answer ended without one reaches the pass once, naming the agent that owes the
    /// person an answer on its message.
    #[test]
    fn a_failed_turn_is_handed_to_the_pass() {
        use tilo_broker_core::services::AgentSessions as _;
        let sessions = AgentSessions::new();
        let turn = asked("agent-session:t1", "scribe");
        sessions.asked(vec![("w1".to_owned(), vec![turn.clone()])]);
        sessions.turn_failed(&turn.session, &turn.turn, turn.attempt, "it stopped");
        assert_eq!(
            sessions.take_failed(),
            vec![(
                "scribe".to_owned(),
                turn.turn.clone(),
                turn.attempt,
                "it stopped".to_owned()
            )]
        );
        assert_eq!(
            sessions.take_failed(),
            Vec::new(),
            "a failure is handed to the pass once"
        );
        sessions.turn_failed("agent-session:other", &turn.turn, 0, "it stopped");
        assert_eq!(
            sessions.take_failed(),
            Vec::new(),
            "no turn goes in that session, so no message waits on it"
        );
    }
}
