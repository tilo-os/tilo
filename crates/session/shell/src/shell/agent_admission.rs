//! The person's consent for the session's agent door.
//!
//! A process in this login — Claude Code in a terminal, say — connects to the
//! door (`compositor::agent_door`) and says who it is. Nothing it asks for
//! runs until the person admits it, here, in trusted chrome: "Claude Code in
//! Alacritty wants to work in Home as an agent". Admitting it makes the
//! client an *agent member* of that workspace — the same membership
//! Settings → Agents shows and revokes — and serves it as that agent. Its
//! authority is the room policy's agent preset grants (observer unless the
//! person picks another), never a human role, and removing it revokes
//! catalogue and authority at once.
//!
//! The client chooses neither the principal nor the workspace. The agent is
//! the one the person picks (an existing member by name, or a new one named
//! after the client); the workspace is the one in front of them when they
//! were asked.
//!
//! "This session" and "Always" remember that this client is that agent:
//! the next connection of the same executable under the same client name is
//! let in without asking wherever that agent is already a member. "Always"
//! is kept in this account's private state, never in the replicated
//! workspace document, and only a workspace owner may give it
//! (`answer_pending_prompt`). A client cannot answer its own prompt: until
//! it is admitted it is served nothing, and an agent never sees the
//! `permission-prompts` decisions (`search::visibility`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tilo_action_policy::roles::AgentPreset;
use tilo_runtime::session_door::Consent;

use crate::portal::PromptOutcome;
use crate::shell::chrome_state::{Chrome, ChromeState, PendingPrompt};
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
                ("pid", self.pid.to_string().into()),
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
    pub decide: tokio::sync::oneshot::Sender<Consent>,
}

/// Which agent the person admits the client as, when they say: an existing
/// member by id, or a new one with a preset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberChoice {
    pub agent: String,
    pub preset: Option<AgentPreset>,
}

/// The person's answer to an agent admission.
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

/// Make `agent` a member of the workspace in `room` with `preset`'s grants,
/// unless it already is one, through the ordinary governance path (the room
/// document, every member, disk). Refused when the person does not administer it.
///
/// # Errors
/// Names why the agent could not be added.
pub fn ensure_agent_member(
    state: &ChromeState,
    room: &str,
    agent: &str,
    preset: AgentPreset,
) -> Result<(), String> {
    let workspace = {
        let tree = state.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .map(|workspace| {
                (
                    workspace.id,
                    workspace
                        .agent_role_at(agent, tilo_clock::unix_now())
                        .is_some(),
                )
            })
    };
    let Some((workspace_id, member)) = workspace else {
        return Err(t("agent-door-workspace-gone"));
    };
    if member {
        return Ok(());
    }
    let user = state.active_local_user();
    let changes = preset
        .grants(
            room,
            agent,
            &tilo_action_policy::Subject::Person(user.clone()),
        )
        .into_iter()
        .map(tilo_action_policy::Change::Grant)
        .collect();
    state
        .tree_mut()
        .govern_in_workspace(workspace_id, &user, changes)
        .map(drop)
        .map_err(|_| {
            t_args(
                "agent-door-not-administrator",
                &[("agent", agent.to_owned().into())],
            )
        })
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
    room: String,
    answer: tokio::sync::oneshot::Receiver<AgentAnswer>,
    proposed_agent: String,
    decide: tokio::sync::oneshot::Sender<Consent>,
}

/// The compositor thread's half of the door: it asks the person, adds the
/// agent they choose and remembers what they said to remember.
pub struct AgentDoorConsent {
    requests: std::sync::mpsc::Receiver<DoorRequest>,
    waiting: Vec<Waiting>,
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
            let consent = match waiting.answer.try_recv() {
                Ok(answer) => {
                    let consent = self.settle(&chrome.borrow(), &waiting, answer);
                    chrome.borrow_mut().bump();
                    consent
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    still.push(waiting);
                    continue;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    Consent::Refuse(t("agent-door-withdrawn"))
                }
            };
            let _ = waiting.decide.send(consent);
        }
        self.waiting = still;
    }

    fn remembered(&self, key: Option<&BindingKey>) -> Option<&String> {
        let key = key?;
        self.session.get(key).or_else(|| self.always.get(key))
    }

    fn ask(&mut self, chrome: &Chrome, request: DoorRequest) {
        let DoorRequest { facts, decide } = request;
        let mut state = chrome.borrow_mut();
        let remembered = self.remembered(facts.binding_key().as_ref()).cloned();
        let now = tilo_clock::unix_now();
        let workspace = state.tree().desktop.active_workspace().map(|workspace| {
            let member = remembered
                .as_deref()
                .is_some_and(|agent| workspace.agent_role_at(agent, now).is_some());
            (workspace.room.to_string(), workspace.name.clone(), member)
        });
        let Some((room, workspace, remembered_member)) = workspace else {
            let _ = decide.send(Consent::Refuse(t("agent-door-no-workspace")));
            return;
        };
        if remembered_member && let Some(agent) = remembered {
            // What the person said to remember: this client is this agent,
            // let in wherever it is already a member.
            let _ = decide.send(Consent::Admit { room, agent });
            return;
        }
        let proposed_agent = remembered.unwrap_or_else(|| proposed_agent_id(&facts.client));
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
            answer: answered,
            proposed_agent,
            decide,
        });
    }

    fn settle(&mut self, state: &ChromeState, waiting: &Waiting, answer: AgentAnswer) -> Consent {
        if answer.outcome == PromptOutcome::Deny {
            return Consent::Refuse(t("agent-door-declined"));
        }
        let (agent, preset) = match answer.member {
            Some(choice) => match canonical_agent(&choice.agent) {
                Ok(agent) => (agent, choice.preset),
                Err(reason) => return Consent::Refuse(reason),
            },
            None => (waiting.proposed_agent.clone(), None),
        };
        if let Err(reason) = ensure_agent_member(
            state,
            &waiting.room,
            &agent,
            preset.unwrap_or(AgentPreset::Observer),
        ) {
            return Consent::Refuse(reason);
        }
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
        Consent::Admit {
            room: waiting.room.clone(),
            agent,
        }
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
