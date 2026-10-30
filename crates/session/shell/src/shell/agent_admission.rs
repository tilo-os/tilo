//! The person's consent for the session's agent door.
//!
//! A process in this login — Claude Code in a terminal, say — connects to the
//! door (`compositor::agent_door`) and says who it is. Nothing it asks for
//! runs until the person admits it, here, in trusted chrome: "Claude Code in
//! Alacritty wants to work in Home". Admitting it makes a task of the
//! connection, as asking an agent does (docs/architecture/agents.md, "Task
//! agents and collaborators"): an issue in Tasks ("claude-code in this
//! session"), assigned to a task agent minted for the connection, whose run
//! the person's admission starts. The connection then works in that run
//! (`tilo_action_broker::collab_broker_capnp::TaskRunHost`): the run admits
//! each of its actions, as the agent working on the issue for the person, so
//! it is attributed, in History and in Tasks like any agent's work, and
//! stopping the run ends what it may do.
//!
//! The client chooses neither its name nor the workspace. The name is the
//! one the person gives (the client's own by default); the workspace is the
//! one in front of them when they were asked.
//!
//! "This session" and "Always" remember that this client may come in: the
//! next connection of the same executable under the same client name is let
//! in without asking, into a run of its own. "Always" is kept in this
//! account's private state, never in the replicated workspace document, and
//! only a workspace owner may give it (`answer_pending_prompt`). A client
//! cannot answer its own prompt: until it is admitted it is served nothing,
//! and an agent never sees the `permission-prompts` decisions
//! (`search::visibility`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::portal::PromptOutcome;
use crate::shell::chrome_state::{Chrome, PendingPrompt};
use crate::shell::i18n::{t, t_args};

/// What the kernel and the client's own `initialize` say about a connection.
/// Shown to the person; the kernel's facts are what bindings key on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientFacts {
    /// `clientInfo.name`, bounded.
    pub client: String,
    pub version: Option<String>,
    pub pid: u32,
    /// `/proc/<pid>/exe`.
    pub exe: Option<PathBuf>,
    /// The app this session launched that the client descends from: the
    /// terminal it runs in.
    pub host_app: Option<String>,
}

impl ClientFacts {
    /// The subject line of the prompt: who asks, and from where.
    #[must_use]
    pub fn subject(&self) -> String {
        let program = self.exe.as_deref().and_then(Path::file_name).map_or_else(
            || "?".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let who = t_args(
            "agent-door-client",
            &[
                ("client", self.client.clone().into()),
                ("program", program.into()),
            ],
        );
        match &self.host_app {
            Some(app) => t_args(
                "agent-door-client-in-app",
                &[("who", who.into()), ("app", app.clone().into())],
            ),
            None => who,
        }
    }

    /// The remembered identity of this client: the executable the kernel
    /// names and the name it gives. `None` without an executable: such a
    /// client is admitted once at a time.
    fn binding_key(&self) -> Option<BindingKey> {
        Some(BindingKey {
            exe: self.exe.clone()?,
            client: self.client.clone(),
        })
    }
}

/// One connection waiting on the person, handed over by the door's worker.
pub struct DoorRequest {
    pub facts: ClientFacts,
    /// Dropped by the worker when the client leaves: the prompt is withdrawn.
    pub decide: tokio::sync::oneshot::Sender<DoorDecision>,
}

/// A connection the person admitted, working in its task's run.
#[derive(Debug)]
pub struct Admitted {
    pub room: String,
    /// The task agent minted for it, as its delegation names it.
    pub agent: String,
    /// The person who admitted it, whom its run acts for.
    pub person: String,
    /// The name it was admitted under, for what it shows.
    pub label: String,
    /// What it joins its run with: the device's certificate, the task
    /// delegation and the task key's proof.
    pub presentation: Vec<u8>,
}

/// The person's answer to a connection: admitted into its run, or turned
/// away with why.
pub type DoorDecision = Result<Admitted, String>;

/// The run Tasks made for an admitted connection, once it goes.
#[derive(Debug)]
pub struct DoorRun {
    pub agent: String,
    /// The issue's address.
    pub issue: String,
    pub presentation: Vec<u8>,
}

/// An admitted connection whose run Tasks is to make: an issue titled
/// `title`, assigned to a task agent minted for it, started by `person`.
#[derive(Debug)]
pub struct DoorRunAsk {
    pub room: crate::shell::state::RoomId,
    pub person: String,
    pub title: String,
    pub body: String,
    pub reply: tokio::sync::oneshot::Sender<Result<DoorRun, String>>,
}

/// The name the person admits the client under, when they give one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberChoice {
    pub agent: String,
}

#[derive(Debug)]
pub struct AgentAnswer {
    pub outcome: PromptOutcome,
    pub member: Option<MemberChoice>,
}

/// The agent admission a [`PendingPrompt`] asks about.
pub struct AgentAdmission {
    pub facts: ClientFacts,
    /// The workspace the client would work in, by name, as the person saw it.
    pub workspace: String,
    /// The agent it is admitted as unless the person names another.
    pub proposed_agent: String,
    answer: Option<tokio::sync::oneshot::Sender<AgentAnswer>>,
    /// Wakes the compositor loop, whose next turn hands the answer on.
    wake: Option<crate::session_runtime::Wake>,
}

impl AgentAdmission {
    /// What the prompt says is asked.
    #[must_use]
    pub fn label(&self) -> String {
        t_args(
            "agent-door-admission",
            &[
                ("workspace", self.workspace.clone().into()),
                ("agent", self.proposed_agent.clone().into()),
            ],
        )
    }

    pub(crate) fn answer(&mut self, outcome: PromptOutcome, member: Option<MemberChoice>) {
        if let Some(answer) = self.answer.take() {
            let _ = answer.send(AgentAnswer { outcome, member });
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }

    /// Whether nobody waits for the answer any more.
    pub(crate) fn is_retired(&self) -> bool {
        self.answer
            .as_ref()
            .is_none_or(tokio::sync::oneshot::Sender::is_closed)
    }
}

impl std::fmt::Debug for AgentAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentAdmission")
            .field("facts", &self.facts)
            .field("workspace", &self.workspace)
            .field("proposed_agent", &self.proposed_agent)
            .finish_non_exhaustive()
    }
}

/// The agent id a client is proposed as: its own name, folded into a
/// canonical member id (`Claude Code` → `claude-code`).
#[must_use]
pub fn proposed_agent_id(client: &str) -> String {
    let mut id = String::new();
    for character in client.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            id.push(character);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
        if id.len() >= 64 {
            break;
        }
    }
    let id = id.trim_end_matches('-');
    if id.is_empty() {
        "agent".to_owned()
    } else {
        id.to_owned()
    }
}

/// A canonical agent member id, or why the person's choice is not one.
fn canonical_agent(raw: &str) -> Result<String, String> {
    let agent = raw.trim().to_lowercase();
    if agent.is_empty() || agent.len() > 128 || agent.chars().any(char::is_control) {
        return Err(t("agent-door-agent-invalid"));
    }
    Ok(agent)
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BindingKey {
    exe: PathBuf,
    client: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StoredBindings {
    bindings: Vec<StoredBinding>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredBinding {
    #[serde(flatten)]
    key: BindingKey,
    agent: String,
}

/// A connection the person is being asked about.
struct Waiting {
    facts: ClientFacts,
    room: crate::shell::state::RoomId,
    /// The person at this device in `room`, who is asked and whom an
    /// admitted agent acts for.
    person: String,
    answer: tokio::sync::oneshot::Receiver<AgentAnswer>,
    proposed_agent: String,
    decide: tokio::sync::oneshot::Sender<DoorDecision>,
}

/// A connection the person admitted, waiting for Tasks to make its run: it
/// is served once the run goes, as a task agent is launched once its run
/// goes.
struct Admitting {
    room: crate::shell::state::RoomId,
    label: String,
    /// The person who admitted it, whom its run acts for.
    person: String,
    run: tokio::sync::oneshot::Receiver<Result<DoorRun, String>>,
    decide: tokio::sync::oneshot::Sender<DoorDecision>,
}

/// The compositor thread's half of the door: it asks the person, asks Tasks
/// for the run of each client they admit and remembers what they said to
/// remember.
pub struct AgentDoorConsent {
    requests: std::sync::mpsc::Receiver<DoorRequest>,
    waiting: Vec<Waiting>,
    admitting: Vec<Admitting>,
    session: HashMap<BindingKey, String>,
    always: HashMap<BindingKey, String>,
    /// Where "Always" is kept: this account's private state, never the
    /// replicated workspace document. `None` keeps it for this session.
    always_path: Option<PathBuf>,
    wake: Option<crate::session_runtime::Wake>,
}

/// The door's request channel and the consent that answers it.
#[must_use]
pub fn channel(
    always_path: Option<PathBuf>,
    wake: Option<crate::session_runtime::Wake>,
) -> (std::sync::mpsc::Sender<DoorRequest>, AgentDoorConsent) {
    let (sender, requests) = std::sync::mpsc::channel();
    let always = always_path
        .as_deref()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<StoredBindings>(&bytes).ok())
        .map(|stored| {
            stored
                .bindings
                .into_iter()
                .map(|binding| (binding.key, binding.agent))
                .collect()
        })
        .unwrap_or_default();
    (
        sender,
        AgentDoorConsent {
            requests,
            waiting: Vec::new(),
            admitting: Vec::new(),
            session: HashMap::new(),
            always,
            always_path,
            wake,
        },
    )
}

/// Where "Always" answers are kept for this account.
#[must_use]
pub fn default_always_path() -> Option<PathBuf> {
    tilo_paths::xdg::state_home().map(|state| state.join("tilo").join("agent-door-bindings.json"))
}

impl AgentDoorConsent {
    /// Take new requests, and settle those the person has answered or whose
    /// client left. Runs on the compositor thread each turn of its loop.
    pub fn pump(&mut self, chrome: &Chrome) {
        while let Ok(request) = self.requests.try_recv() {
            self.ask(chrome, request);
        }
        let mut still = Vec::with_capacity(self.waiting.len());
        for mut waiting in std::mem::take(&mut self.waiting) {
            if waiting.decide.is_closed() {
                // The client left; dropping `answer` withdraws its prompt.
                continue;
            }
            let label = match waiting.answer.try_recv() {
                Ok(answer) => {
                    let label = self.settle(&waiting, answer);
                    chrome.borrow_mut().bump();
                    label
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    still.push(waiting);
                    continue;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Err(t("agent-door-withdrawn"))
                }
            };
            match label {
                Ok(label) => self.ask_run(
                    chrome,
                    &waiting.facts,
                    (waiting.room, waiting.person),
                    label,
                    waiting.decide,
                ),
                Err(refused) => {
                    let _ = waiting.decide.send(Err(refused));
                }
            }
        }
        self.waiting = still;
        self.admit_runs();
    }

    /// Ask Tasks for the run of a client the person admitted under `label`
    /// in `room`, acting for `person`; it is let in once the run goes.
    fn ask_run(
        &mut self,
        chrome: &Chrome,
        facts: &ClientFacts,
        (room, person): (crate::shell::state::RoomId, String),
        label: String,
        decide: tokio::sync::oneshot::Sender<DoorDecision>,
    ) {
        let (reply, run) = tokio::sync::oneshot::channel();
        chrome.borrow_mut().ask_door_run(DoorRunAsk {
            room,
            person: person.clone(),
            title: t_args("agent-door-run-title", &[("agent", label.clone().into())]),
            body: t_args("agent-door-run-body", &[("who", facts.subject().into())]),
            reply,
        });
        if let Some(wake) = &self.wake {
            wake();
        }
        self.admitting.push(Admitting {
            room,
            label,
            person,
            run,
            decide,
        });
    }

    /// Let in each admitted client whose run now goes, and turn away one
    /// whose run Tasks did not make.
    fn admit_runs(&mut self) {
        let mut still = Vec::with_capacity(self.admitting.len());
        for mut admitting in std::mem::take(&mut self.admitting) {
            if admitting.decide.is_closed() {
                continue;
            }
            let decision = match admitting.run.try_recv() {
                Ok(Ok(run)) => {
                    tracing::info!(agent = %run.agent, issue = %run.issue, "agent door: the client works in its run");
                    Ok(Admitted {
                        room: admitting.room.to_string(),
                        agent: run.agent,
                        person: admitting.person,
                        label: admitting.label,
                        presentation: run.presentation,
                    })
                }
                Ok(Err(reason)) => Err(t_args(
                    "agent-door-run-refused",
                    &[("reason", reason.into())],
                )),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    still.push(admitting);
                    continue;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Err(t("agent-door-run-unknown"))
                }
            };
            let _ = admitting.decide.send(decision);
        }
        self.admitting = still;
    }

    fn remembered(&self, key: Option<&BindingKey>) -> Option<&String> {
        let key = key?;
        self.session.get(key).or_else(|| self.always.get(key))
    }

    fn ask(&mut self, chrome: &Chrome, request: DoorRequest) {
        let DoorRequest { facts, decide } = request;
        let remembered = self.remembered(facts.binding_key().as_ref()).cloned();
        let state = chrome.borrow();
        let workspace = state.tree().desktop.active_workspace().map(|workspace| {
            (
                workspace.room(),
                workspace.name.clone(),
                state.local_user_for_room(&workspace.room()),
            )
        });
        drop(state);
        let Some((room, workspace, person)) = workspace else {
            let _ = decide.send(Err(t("agent-door-no-workspace")));
            return;
        };
        if let Some(label) = remembered {
            // What the person said to remember: this client may come in,
            // into a run of its own.
            self.ask_run(chrome, &facts, (room, person), label, decide);
            return;
        }
        let mut state = chrome.borrow_mut();
        let proposed_agent = proposed_agent_id(&facts.client);
        let (answer, answered) = tokio::sync::oneshot::channel();
        let admission = AgentAdmission {
            facts: facts.clone(),
            workspace,
            proposed_agent: proposed_agent.clone(),
            answer: Some(answer),
            wake: self.wake.clone(),
        };
        state
            .pending_grants
            .push_back(PendingPrompt::agent_admission(admission));
        state.bump();
        self.waiting.push(Waiting {
            facts,
            room,
            person,
            answer: answered,
            proposed_agent,
            decide,
        });
    }

    /// The name the person admitted the client under, remembered as they
    /// said, or why it was turned away.
    fn settle(&mut self, waiting: &Waiting, answer: AgentAnswer) -> Result<String, String> {
        if answer.outcome == PromptOutcome::Deny {
            return Err(t("agent-door-declined"));
        }
        let agent = match answer.member {
            Some(choice) => canonical_agent(&choice.agent)?,
            None => waiting.proposed_agent.clone(),
        };
        if let Some(key) = waiting.facts.binding_key() {
            match answer.outcome {
                PromptOutcome::AllowSession => {
                    self.session.insert(key, agent.clone());
                }
                PromptOutcome::AllowAlways => {
                    self.always.insert(key, agent.clone());
                    self.store_always();
                }
                PromptOutcome::AllowOnce | PromptOutcome::Deny => {}
            }
        }
        Ok(agent)
    }

    fn store_always(&self) {
        let Some(path) = self.always_path.as_deref() else {
            return;
        };
        let stored = StoredBindings {
            bindings: self
                .always
                .iter()
                .map(|(key, agent)| StoredBinding {
                    key: key.clone(),
                    agent: agent.clone(),
                })
                .collect(),
        };
        let bytes = match serde_json::to_vec_pretty(&stored) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, "agent door: the remembered agent was kept for this session only");
                return;
            }
        };
        // `pump` runs on the compositor's loop; the write's fsync can take
        // seconds under I/O pressure, so the state writer does it. A newer
        // set of answers that has not been written yet replaces this one.
        let path = path.to_path_buf();
        crate::shell::durable_writer::global().submit(path.clone(), move || {
            if let Err(error) = write_private(&path, &bytes) {
                tracing::warn!(%error, path = %path.display(), "agent door: the remembered agent was kept for this session only");
            }
        });
    }

    /// Requests the person has not answered yet.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// Write `bytes` to `path`, readable by this account only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let staged = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staged)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&staged, path)
}

#[cfg(test)]
#[path = "agent_admission_tests.rs"]
mod tests;
