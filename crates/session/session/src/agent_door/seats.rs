//! The agents the person admitted at the door, as their desk and presence seat them.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;

/// The agents the person admitted at this door, as their desk seats them: one presence per agent,
/// from the moment the person lets its first client in until the last of its connections ends. A
/// shell shows each as its own presence on the bar.
pub(crate) struct Seats {
    inner: std::sync::Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Bumped on every change, so a reading made before it is not mistaken for the desk now.
    revision: u64,
    /// The number the door gives each connection it serves.
    next: u64,
    /// The connections the door is serving, by that number: the person's Stop on an agent takes
    /// every one of its own.
    connections: BTreeMap<u64, Connection>,
}

struct Connection {
    /// The agent the person admitted it as: empty until they answer.
    agent: String,
    /// What the client called itself, as the sheet showed it.
    client: String,
    /// The workspace it works in, by name.
    workspace: String,
    /// The workspace it works in, by id.
    workspace_id: String,
    /// What its latest step is, as the catalogue named it, and the app it was taken in when its
    /// provider names one: what everyone in its workspace reads beside its face.
    doing: Option<Doing>,
    /// The environment's door it came in by, by number: none for the session's own. It ends with
    /// that environment.
    door: Option<u64>,
    /// The run it works in, by the issue that names it: what every change it makes is stamped
    /// with, and what taking back its work names. `None` where the workspace confines no agent to
    /// a run, and what it does is the workspace's as any app's.
    run: Option<String>,
    /// The records it changed in that run, each as the app that keeps it and the record: what
    /// taking back its work undoes.
    changed: Vec<(String, String)>,
    /// The connection itself, shut when the person stops its seat.
    stream: Option<UnixStream>,
}

impl Seats {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(Inner::default()),
        })
    }

    /// A connection by the environment's door `door` (none for any other), before the person
    /// answers for it: nothing of it shows until it is admitted.
    pub(super) fn joining(&self, door: Option<u64>) -> u64 {
        let mut inner = self.inner.lock_recover();
        inner.next = inner.next.wrapping_add(1);
        let seat = inner.next;
        inner.connections.insert(
            seat,
            Connection {
                agent: String::new(),
                client: String::new(),
                workspace: String::new(),
                workspace_id: String::new(),
                doing: None,
                door,
                run: None,
                changed: Vec::new(),
                stream: None,
            },
        );
        seat
    }

    /// The person admitted connection `seat` as `agent` , a client that calls itself `client` ,
    /// working in `workspace` (`(id, name)`): who is here there from now on.
    pub(super) fn admitted(
        &self,
        seat: u64,
        agent: &str,
        client: &str,
        (id, workspace): (&str, &str),
    ) {
        let mut inner = self.inner.lock_recover();
        let Some(connection) = inner.connections.get_mut(&seat) else {
            // It ended while the person was answering: nothing is served.
            return;
        };
        agent.clone_into(&mut connection.agent);
        client.clone_into(&mut connection.client);
        workspace.clone_into(&mut connection.workspace);
        id.clone_into(&mut connection.workspace_id);
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// Connection `seat` 's agent is at `doing` : what its latest step is, and the app it was
    /// taken in. Whether that is news.
    pub(super) fn acting(&self, seat: u64, doing: Doing) -> bool {
        let mut inner = self.inner.lock_recover();
        let Some(connection) = inner.connections.get_mut(&seat) else {
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

    /// The agents seated here now, each once in the order their keys come: a client with two
    /// connections is one agent.
    pub(crate) fn here(&self) -> Vec<Seated> {
        let mut agents: BTreeMap<String, Seated> = BTreeMap::new();
        for connection in self.inner.lock_recover().connections.values() {
            if connection.agent.is_empty() {
                continue;
            }
            let seated = agents
                .entry(connection.agent.clone())
                .or_insert_with(|| Seated {
                    workspace: connection.workspace_id.clone(),
                    workspace_name: connection.workspace.clone(),
                    agent: connection.agent.clone(),
                    name: connection.client.clone(),
                    doing: None,
                });
            if seated.doing.is_none() {
                seated.doing.clone_from(&connection.doing);
            }
        }
        agents.into_values().collect()
    }

    /// The agent seated here that `speaker` (this device) speaks for on the presence board as the
    /// member `member`, by its key in hex: what a card of that member stands for.
    pub(crate) fn spoken_as(&self, speaker: [u8; 32], member: &str) -> Option<String> {
        self.here()
            .into_iter()
            .map(|seated| seated.agent)
            .find(|agent| hex::encode(collab_types::presence::stand_in(speaker, agent)) == member)
    }

    /// Connection `seat` works in the run `run` , the issue that names it: every change it makes
    /// there is stamped with it, and taking its work back names it.
    pub(super) fn working_in(&self, seat: u64, run: &str) {
        if let Some(connection) = self.inner.lock_recover().connections.get_mut(&seat) {
            connection.run = Some(run.to_owned());
        }
    }

    /// Connection `seat` changed `record` of `app` in its run: one of the records taking its work
    /// back undoes.
    pub(super) fn changed(&self, seat: u64, app: &str, record: &str) {
        let step = (app.to_owned(), record.to_owned());
        let mut inner = self.inner.lock_recover();
        if let Some(connection) = inner.connections.get_mut(&seat)
            && !connection.changed.iter().any(|known| known == &step)
        {
            connection.changed.push(step);
        }
    }

    /// What the agent `agent` works in here, as its connections name it: the run, the records it
    /// changed in it, and the app it says it works in. `None` for an agent seated here not at all.
    pub(crate) fn working(&self, agent: &str) -> Option<Working> {
        let mut working: Option<Working> = None;
        {
            let inner = self.inner.lock_recover();
            for connection in inner
                .connections
                .values()
                .filter(|connection| connection.agent == agent)
            {
                let held = working.get_or_insert_with(|| Working {
                    workspace: connection.workspace_id.clone(),
                    run: None,
                    changed: Vec::new(),
                    app: None,
                });
                if held.run.is_none() {
                    held.run.clone_from(&connection.run);
                }
                for step in &connection.changed {
                    if !held.changed.contains(step) {
                        held.changed.push(step.clone());
                    }
                }
                if held.app.is_none() {
                    held.app = connection
                        .doing
                        .as_ref()
                        .and_then(|doing| doing.app.clone());
                }
            }
        }
        working
    }

    /// How the desk shows connection `seat` 's agent ("Claude Code in dev #1"), once admitted.
    pub(super) fn shown(&self, seat: u64) -> Option<String> {
        self.inner
            .lock_recover()
            .connections
            .get(&seat)
            .map(|connection| connection.client.clone())
            .filter(|shown| !shown.is_empty())
    }

    /// The connection `seat` names, which ending its seat closes.
    pub(super) fn attached(&self, seat: u64, stream: &UnixStream) {
        let copy = match stream.try_clone() {
            Ok(copy) => Some(copy),
            Err(error) => {
                // Without its own handle the seat cannot end this connection;
                // the client keeps it until it leaves by itself.
                tracing::warn!(%error, "an agent seat's connection could not be duplicated");
                None
            }
        };
        if let Some(connection) = self.inner.lock_recover().connections.get_mut(&seat) {
            connection.stream = copy;
        }
    }

    /// The connection `seat` names ended; its agent's seat goes with its last.
    pub(super) fn left(&self, seat: u64) {
        let mut inner = self.inner.lock_recover();
        if inner.connections.remove(&seat).is_none() {
            return;
        }
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// End every connection served as `agent` , as the person's Stop on its seat does. Whether
    /// there was one.
    pub(crate) fn stop(&self, agent: &str) -> bool {
        self.end(|connection| connection.agent == agent)
    }

    /// End every connection that came in by the environment's door `door`, as that environment
    /// ends.
    pub(super) fn stop_door(&self, door: u64) -> bool {
        self.end(|connection| connection.door == Some(door))
    }

    /// End every connection `ending` picks. Whether there was one.
    fn end(&self, ending: impl Fn(&Connection) -> bool) -> bool {
        let ended: Vec<Connection> = {
            let mut inner = self.inner.lock_recover();
            let seats: Vec<u64> = inner
                .connections
                .iter()
                .filter(|(_, connection)| ending(connection))
                .map(|(seat, _)| *seat)
                .collect();
            if seats.is_empty() {
                return false;
            }
            let ended = seats
                .into_iter()
                .filter_map(|seat| inner.connections.remove(&seat))
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

    /// The agents seated here now, as the desk's shell lists them.
    pub(crate) fn readings(&self) -> Vec<tilo_session_core::shell::AgentReading> {
        self.here()
            .into_iter()
            .map(|seated| tilo_session_core::shell::AgentReading {
                key: seated.agent,
                name: seated.name,
                state: "running".to_owned(),
                doing: seated.workspace_name,
                mark: String::new(),
                unseen: 0,
                seat: true,
            })
            .collect()
    }

    /// What the seats were made from, without making them: changes only when an agent was admitted
    /// or left.
    pub(crate) fn revision(&self) -> u64 {
        self.inner.lock_recover().revision
    }
}

/// An agent seated at the door, as presence speaks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seated {
    /// The workspace it works in, by id.
    pub(crate) workspace: String,
    /// The workspace it works in, by name.
    pub(crate) workspace_name: String,
    /// The agent it was admitted as (`claude-code@host#2`): who it is, whatever it is called.
    pub(crate) agent: String,
    /// What the desk calls it (`Claude Code #2`).
    pub(crate) name: String,
    /// What its latest step is, as the catalogue named it, and the app it was taken in.
    pub(crate) doing: Option<Doing>,
}

/// What a step of an agent is, as its workspace says it beside its face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Doing {
    /// The action's own name in the catalogue, which says what it is doing ("Replace text at a
    /// place"); the action's id where the catalogue names none.
    pub(crate) action: String,
    /// The kind of step the action's own declaration names, so the line beside its face is the
    /// catalogue's words for it rather than an English verb read off the title.
    pub(crate) step: Option<tilo_actions::StepKind>,
    /// The app it acts in, when its provider says which one.
    pub(crate) app: Option<String>,
}

/// What the commands on the card of an agent seated here act on: the run it works in and the
/// records it changed there, in its workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Working {
    /// The workspace it works in, by id.
    pub(crate) workspace: String,
    /// The run, by the issue that names it: `None` where its workspace confines no agent to a run.
    pub(crate) run: Option<String>,
    /// The records it changed in that run, each as the app that keeps it and the record.
    pub(crate) changed: Vec<(String, String)>,
    /// The app it is working in now, when it says.
    pub(crate) app: Option<String>,
}
