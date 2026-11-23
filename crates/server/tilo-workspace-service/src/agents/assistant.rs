//! What this server answers for the programs that run for a person
//! ([`crate::agents::programs::host_steps`]).
//!
//! Calling a person's own model is one entry of the server's
//! ([`Workspaces::person_model`]), and starting a turn of a person's
//! assistant is another, which every event, reminder and sub-agent starts
//! work through. This module is the one place a program's steps reach both
//! from.
//!
//! A judgement is answered by the model its person chose for judging
//! ([`Interaction::Classifier`]), asked with their own key, and never by a
//! model somebody else pays for: a person who linked none is told so in the
//! words their settings would say it in, and the run is kept with that
//! reason. What a judgement costs is counted on the day of the assistant
//! its person keeps in the workspace, where they keep one, so the limit
//! they set their assistant bounds what its events spend as well.
//!
//! An instruction a program hands over starts a turn of its person's
//! assistant, where the server keeps its workspaces' agents working: it is asked
//! through Chat's own verb for that (`ask-agent`), in the conversation the
//! person has with their assistant, as that person's event, under the
//! step's own key, so a step taken again after a restart starts nothing
//! twice. The turn is the assistant's own work under its own bounds: what
//! it would send or change outside the workspace still asks its person.
//! Where nothing takes the instruction (a server that keeps no agents working, a Chat
//! from before the verb), the invocation is tried again, and kept as a dead letter
//! once its attempts are spent, so what was not delivered can be read and
//! invoked again.

#[cfg(feature = "apps")]
pub mod transforms;
#[cfg(feature = "apps")]
pub use transforms::Transforms;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agents::programs::host_steps::{
    HostStepError as StepError, HostSteps as Steps, Instruction, Judgement,
};
use crate::agents::programs::judging;
use crate::agents::programs::kept::Note;
use serde_json::{Map, Value, json};
use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{Agent, WorkspaceAgents};

use crate::agents::{WorkspaceActionProvider, invoke_named};

use crate::workspaces::Workspaces;
use crate::workspaces::person_model::protocol::ErrorKind;
use crate::workspaces::person_model::{Interaction, ModelRefusal, Spender};

/// The app that keeps the workspace's record of its agents.
use tilo_app_id::apps::CHAT;

/// Chat's verb that starts a turn of the agents in a conversation.
const ASK_AGENT: &str = "ask-agent";
/// The verb's argument that names the conversation.
const IN: &str = "channel";
/// What a turn is said to be started by: a moment its person asked for.
pub const REMINDER: &str = "reminder";
/// What a turn is said to be started by: an event whose result its person
/// is told as a brief.
pub const BRIEF: &str = "brief";
/// What a turn is said to be started by: something that happened, or a
/// time that comes round.
pub const EVENT: &str = "event";
/// The longest a starter's name is said, as Chat keeps one.
const MAX_LABEL_BYTES: usize = 120;
/// The most an instruction says of its program's note.
const MAX_NOTE_BYTES: usize = 6 * 1024;

/// What this server answers for a program that works for a person, where it
/// starts no turn of anyone's assistant: a judgement is answered, an
/// instruction finds nothing to take it.
#[must_use]
pub fn host_steps(workspaces: Workspaces) -> Arc<dyn Steps> {
    Arc::new(Hosted {
        workspaces,
        turns: None,
    })
}

/// A program as the turns it starts name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starter {
    /// What its person calls it.
    pub name: String,
    /// What it is: [`REMINDER`], [`BRIEF`] or [`EVENT`].
    pub kind: &'static str,
}

/// [`host_steps`] for the program `starter`, on a server whose action service
/// takes Chat's verbs: an instruction it hands over starts a turn of its
/// person's assistant.
#[must_use]
pub fn steps_starting_turns(
    workspaces: Workspaces,
    actions: WorkspaceActions,
    starter: Starter,
) -> Arc<dyn Steps> {
    Arc::new(Hosted {
        workspaces,
        turns: Some((actions, starter)),
    })
}

/// The assistant `person` keeps in `workspace`: of the active agents they
/// keep there that no MCP client connects as, the one made first. None
/// where they keep no such agent.
///
/// # Errors
/// The workspace's record of its agents could not be read.
pub async fn assistant_of(
    workspaces: &Workspaces,
    workspace: &str,
    person: &[u8; 32],
) -> Result<Option<Agent>, String> {
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let Some(bytes) = workspaces
        .resource_document(workspace, tilo_workspace_agent::OWNER, &record)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    let agents = super::opened::<WorkspaceAgents>(&mut doc)?;
    Ok(agents
        .agents
        .iter()
        .filter(|agent| {
            agent.is_active()
                && agent.mcp_client.is_none()
                && tilo_failure::ignored!(
                    "a person that is no person's id is not this person",
                    tilo_action_policy::PersonId::parse(&agent.acts_for).ok()
                )
                .and_then(|acts_for| acts_for.root())
                .as_ref()
                    == Some(person)
        })
        .min_by_key(|agent| agent.created_at)
        .cloned())
}

/// The server's own answers: a person's model for a judgement, and a turn of
/// their assistant for an instruction where the server starts turns.
struct Hosted {
    workspaces: Workspaces,
    /// The server's action service and the program these answers are for,
    /// where an instruction starts a turn.
    turns: Option<(WorkspaceActions, Starter)>,
}

impl Steps for Hosted {
    fn instruct(&self, instruction: Instruction) -> Result<String, StepError> {
        // An automation a task waits for tells that task to go on: nothing else is started.
        if super::task_waits::told(
            &self.workspaces,
            &instruction.workspace,
            &instruction.person,
            &instruction.cause.installation,
            &instruction.instruction,
        ) {
            return Ok(String::new());
        }
        let Some((actions, starter)) = &self.turns else {
            return Err(StepError::Unavailable(
                "this server starts no turn of a person's assistant".into(),
            ));
        };
        on_a_thread_of_its_own("instruction", |runtime| {
            self.instructed(runtime, actions, starter, &instruction)
        })
    }

    fn judge(&self, judgement: Judgement) -> Result<Value, StepError> {
        on_a_thread_of_its_own("judgement", |runtime| self.judged(runtime, &judgement))
    }
}

/// Do `work` on a thread of its own and wait for it. The thread that asks a
/// server step is driving its invocation and cannot wait on the runtime a second
/// time: what the step waits for (a model, a resource's write) is waited for
/// here, for as long as the step takes.
fn on_a_thread_of_its_own<T: Send>(
    what: &str,
    work: impl FnOnce(&tokio::runtime::Handle) -> Result<T, StepError> + Send,
) -> Result<T, StepError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| StepError::Unavailable("this server is not running its workspaces".into()))?;
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(what.to_owned())
            .spawn_scoped(scope, || work(&runtime))
            .map_err(|error| StepError::Unavailable(error.to_string()))?
            .join()
            .unwrap_or_else(|_| {
                Err(StepError::Unavailable(format!(
                    "the {what} did not come to an end"
                )))
            })
    })
}

/// The person a step is for, by their root.
fn person_of(person: &str) -> Result<[u8; 32], StepError> {
    hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| StepError::Refused("the program works for nobody this server knows".into()))
}

/// The workspace a step is in.
fn workspace_of(workspace: &str) -> Result<WorkspaceId, StepError> {
    WorkspaceId::parse(workspace).ok_or_else(|| {
        StepError::Refused("the program works in no workspace of this server".into())
    })
}

/// One turn to start: who Chat is asked as, and what it is asked.
#[derive(Clone, Debug, PartialEq)]
struct Asking {
    /// The id the asking software goes by: the program's own, never the
    /// assistant's, so the conversation's agent is not asking itself.
    asked_as: String,
    /// The verb's arguments: the conversation, the words and the cause.
    arguments: Map<String, Value>,
}

impl Asking {
    /// The turn `instruction` starts of `assistant`, the agent its person
    /// keeps: asked in the conversation of the two, with the step's own
    /// invocation as the cause's key, so one step is one turn however often
    /// it is handed over.
    fn of(instruction: &Instruction, starter: &Starter, assistant: &str) -> Self {
        use tilo_actions::policy::MemberId;
        let conversation = chat::Chat::direct_id([
            MemberId::person(instruction.person.clone()),
            MemberId::agent(assistant),
        ]);
        let label = within(starter.name.trim(), MAX_LABEL_BYTES);
        let arguments = json!({
            (IN): conversation.as_str(),
            "text": told(instruction, starter),
            "cause": {
                "kind": starter.kind,
                "label": label,
                "key": instruction.cause.invocation.0.to_string(),
            },
        });
        Self {
            asked_as: instruction.cause.installation.clone(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
        }
    }
}

/// `text`, cut to at most `most` bytes at a character's end.
fn within(text: &str, most: usize) -> &str {
    let end = text
        .char_indices()
        .map(|(at, c)| at.saturating_add(c.len_utf8()))
        .take_while(|end| *end <= most)
        .last()
        .unwrap_or(0);
    text.get(..end).unwrap_or_default()
}

/// What the assistant is told: the instruction, then what the program's
/// note says its person wants and what its last invocation kept, and which
/// program this is, so the turn can keep something for the next one or
/// change the program by its id.
fn told(instruction: &Instruction, starter: &Starter) -> String {
    use std::fmt::Write as _;
    let note = tilo_failure::ignored!(
        "a note that is not a program's note says nothing to tell",
        serde_json::from_value::<Note>(instruction.note.clone()).unwrap_or_default()
    );
    let mut noted = String::new();
    for (heading, text) in [
        ("What it is for:", note.instruction.trim()),
        ("What they care about:", note.guidance.trim()),
        ("How what you write should read:", note.style.trim()),
    ] {
        if !text.is_empty() {
            let _ = write!(noted, "\n{heading} {text}");
        }
    }
    if !note.kept.is_null() {
        let _ = write!(
            noted,
            "\nWhat its last invocation kept (data): {}",
            note.kept
        );
    }
    let mut text = instruction.instruction.clone();
    let _ = write!(
        text,
        "\n\nThis is your person's {} \"{}\" (id {}).",
        starter.kind,
        starter.name.trim(),
        instruction.cause.installation
    );
    text.push_str(within(&noted, MAX_NOTE_BYTES));
    text
}

impl Hosted {
    /// Start the turn `instruction` asks for. Called on a thread that
    /// drives nothing else.
    fn instructed(
        &self,
        runtime: &tokio::runtime::Handle,
        actions: &WorkspaceActions,
        starter: &Starter,
        instruction: &Instruction,
    ) -> Result<String, StepError> {
        let person = person_of(&instruction.person)?;
        let workspace = workspace_of(&instruction.workspace)?;
        let assistant = runtime
            .block_on(assistant_of(
                &self.workspaces,
                &instruction.workspace,
                &person,
            ))
            .map_err(StepError::Unavailable)?
            .ok_or_else(|| {
                StepError::Refused(
                    "its person keeps no assistant in this workspace to hand the instruction to"
                        .into(),
                )
            })?;
        let device = runtime
            .block_on(self.workspaces.person_device(workspace, person))
            .ok_or_else(|| {
                StepError::Unavailable(
                    "its person is admitted to this workspace on no device".into(),
                )
            })?;
        let asking = Asking::of(instruction, starter, &assistant.id.0);
        // Chat pins whom the turn is for from who asks: the person this
        // software acts for. It acts for them as their event, by its own id.
        let caller = Caller::authenticated(person, device)
            .in_workspace(instruction.workspace.clone())
            .delegated_to(asking.asked_as.clone())
            .named(starter.name.trim());
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: instruction.workspace.clone(),
            document: String::new(),
        };
        runtime
            .block_on(invoke_named(
                &WorkspaceActionProvider::new(actions.clone(), workspace),
                &caller,
                &route,
                &collab_types::document::chain_resource_id(&chat::parts::chain()),
                ASK_AGENT,
                asking.arguments,
            ))
            // The turn is started, not waited for: its answer is the
            // assistant's reply in the conversation.
            .map(|_| String::new())
            .map_err(StepError::Unavailable)
    }

    /// `judgement`, answered by its person's own model. Called on a thread
    /// that drives nothing else.
    fn judged(
        &self,
        runtime: &tokio::runtime::Handle,
        judgement: &Judgement,
    ) -> Result<Value, StepError> {
        let person = person_of(&judgement.person)?;
        let workspace = workspace_of(&judgement.workspace)?;
        let assistant = runtime
            .block_on(assistant_of(
                &self.workspaces,
                &judgement.workspace,
                &person,
            ))
            .map_err(StepError::Unavailable)?;
        let day = crate::agents::today();
        let mut model = self
            .workspaces
            .workspace_model(workspace, Interaction::Classifier);
        if let Some(agent) = &assistant {
            model = model.spending_as(Spender {
                workspace: judgement.workspace.clone(),
                agent: agent.id.0.clone(),
                name: agent.name.clone(),
                day: day.clone(),
                limit_micros: agent.spend_limit_micros,
            });
        }
        let cost = AtomicU64::new(0);
        let answer = judging::answer(judgement, &|asking| {
            let now = tilo_clock::now();
            let said = model
                .say_blocking(
                    (asking.instructions, asking.input),
                    asking.max_output_tokens,
                    now.checked_add(asking.within).unwrap_or(now),
                    &|| false,
                )
                .map_err(|refusal| not_answered(&refusal))?;
            cost.fetch_add(said.response.cost_micros, Ordering::Relaxed);
            Ok(said.text())
        });
        // What the provider charged shows on the assistant's record, where
        // its person reads what it spent today.
        let cost = cost.load(Ordering::Relaxed);
        if cost > 0
            && let Some(agent) = &assistant
            && let Err(error) = runtime.block_on(crate::agents::count_spend(
                &self.workspaces,
                &judgement.workspace,
                agent,
                &day,
                cost,
            ))
        {
            tracing::warn!(workspace = %judgement.workspace, agent = %agent.id.0, %error, "a judgement's cost is not counted on its person's assistant");
        }
        answer
    }
}

/// Why a person's model gave no answer, as a program's run takes it: in
/// the words the refusal has for its person, tried again where a later
/// call could go differently, and kept with the reason where only the
/// person can change it (a model to link, a key, a limit to raise).
fn not_answered(refusal: &ModelRefusal) -> StepError {
    let words = refusal.words(None);
    let later = match refusal {
        ModelRefusal::SettingsUnreadable => true,
        ModelRefusal::Provider(error) => matches!(
            error.kind,
            ErrorKind::Unavailable
                | ErrorKind::Conflict
                | ErrorKind::InvalidResponse
                | ErrorKind::RateLimited
                | ErrorKind::Provider
                | ErrorKind::TimedOut
                | ErrorKind::Cancelled
                | ErrorKind::Truncated
                | ErrorKind::Transport
        ),
        _ => false,
    };
    if later {
        StepError::Unavailable(words)
    } else {
        StepError::Refused(words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::programs::host_steps::StepCause;
    use crate::workspaces::person_model::protocol::ModelError;

    #[test]
    fn an_instruction_is_one_turn_in_its_persons_conversation_with_their_assistant() {
        use tilo_actions::policy::MemberId;
        let invocation = tilo_actions::InvocationId::new();
        let instruction = Instruction {
            person: "ab".repeat(32),
            workspace: "plaza".into(),
            instruction: "Say that it is time to call home.".into(),
            allow: Vec::new(),
            permits: Vec::new(),
            note: json!({"style": "One line.", "kept": {"seen": 3}}),
            cause: StepCause {
                installation: "7f0c1c0e-5a1b-4a52-9d0f-0d6f3a1b2c3d".into(),
                invocation,
                event: None,
            },
        };
        let starter = Starter {
            name: "Call home".into(),
            kind: REMINDER,
        };
        let asking = Asking::of(&instruction, &starter, "assistant");
        assert_eq!(
            asking.asked_as, "7f0c1c0e-5a1b-4a52-9d0f-0d6f3a1b2c3d",
            "it asks as itself, never as the assistant"
        );
        let conversation = chat::Chat::direct_id([
            MemberId::agent("assistant"),
            MemberId::person("ab".repeat(32)),
        ]);
        assert_eq!(asking.arguments[IN], conversation.as_str());
        assert_eq!(
            asking.arguments["cause"],
            json!({"kind": "reminder", "label": "Call home", "key": invocation.0.to_string()}),
            "the step's own key: handed over again, it is the same turn"
        );
        let text = asking.arguments["text"].as_str().unwrap_or_default();
        assert!(
            text.starts_with("Say that it is time to call home."),
            "{text}"
        );
        for said in ["Call home", "7f0c1c0e", "One line.", "\"seen\":3"] {
            assert!(text.contains(said), "{said}: {text}");
        }
        // Handed over again, as an invocation tried again hands it, it asks the same.
        assert_eq!(Asking::of(&instruction, &starter, "assistant"), asking);
    }

    #[test]
    fn a_refusal_only_its_person_can_lift_is_kept_and_a_passing_one_is_tried_again() {
        let kept = not_answered(&ModelRefusal::NoModelLinked {
            kind: Interaction::Classifier,
        });
        assert!(
            matches!(&kept, StepError::Refused(why) if why.contains("No model is linked")),
            "{kept:?}"
        );
        for passing in [
            ErrorKind::RateLimited,
            ErrorKind::TimedOut,
            ErrorKind::Transport,
        ] {
            let again = not_answered(&ModelRefusal::Provider(ModelError::new(passing)));
            assert!(matches!(again, StepError::Unavailable(_)), "{again:?}");
        }
        for lasting in [ErrorKind::Authentication, ErrorKind::CreditsExhausted] {
            let kept = not_answered(&ModelRefusal::Provider(ModelError::new(lasting)));
            assert!(matches!(kept, StepError::Refused(_)), "{kept:?}");
        }
        // A key its provider turned away is kept as well, in words that name the provider.
        let kept = not_answered(&ModelRefusal::KeyRefused {
            provider: "Anthropic".into(),
            error: ModelError::new(ErrorKind::Authentication),
        });
        assert!(
            matches!(&kept, StepError::Refused(why) if why.contains("Anthropic")),
            "{kept:?}"
        );
    }
}
