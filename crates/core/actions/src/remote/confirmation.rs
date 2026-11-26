//! Asking the person, on their own device, about one invocation one of their
//! devices answers.
//!
//! An action the person's grants leave to them asks first. At a device that
//! is not theirs there is nobody at its screen to ask, so the question
//! travels to one of their own devices and back. What comes back names that
//! one invocation; nothing about a grant changes.

use serde::{Deserialize, Serialize};

/// Explicit confirmation carried with an invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confirmation {
    /// The host that placed a browser window hands it a call of a key its person let operate
    /// web pages: their standing permission, not an answer to an ask. Only
    /// [`super::browser::host_answered`] reads it, from that host's link alone; nothing else
    /// takes it as a confirmation, so a caller that writes it is asked like one that wrote none.
    Operated,
    /// The caller typed the phrase the contract asks for.
    Phrase(String),
    /// The person approved the asked invocation record this call was kept as
    /// ([`super::asked`]). Only the dispatch sets it, when it admits the call
    /// again on their answer; admission drops it from anything a caller sent.
    Asked(super::asked::Approval),
}

/// What a confirmation is bound to of an invocation's arguments: their
/// digest, so a provider that keeps the person's answer for one invocation keeps
/// no raw input, and arguments other than the ones answered find no answer.
/// `argument` is the invocation's argument as it travels
/// ([`InvokeArgument::to_slot`](super::InvokeArgument::to_slot)).
#[must_use]
pub fn argument_digest(argument: Option<&str>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    match argument {
        Some(value) => {
            hasher.update(b"some\0");
            hasher.update(value.as_bytes());
        }
        None => {
            hasher.update(b"none\0");
        }
    }
    *hasher.finalize().as_bytes()
}

/// A question a device of the person's puts to them on another of their
/// own devices: one invocation of an action their grants leave to them.
///
/// The device asking is the transport-authenticated one, so the labels here
/// are shown as that device's own words; the answer names this one
/// invocation and nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationQuestion {
    /// Names this question, so the answer names it back.
    pub id: [u8; 32],
    /// The device that asks, as it is labelled to the person.
    pub device_label: String,
    /// The agent the invocation is by, by its id: the device asks for an agent it acts for
    /// over its own session. Empty for the person's own invocation.
    #[serde(default)]
    pub agent: String,
    /// The workspace the invocation runs in.
    pub workspace: String,
    /// The group the invocation is a step of, by the title its agent gave it;
    /// empty when it named none.
    pub title: String,
    /// The action by the title its app declares, and the record it acts on
    /// by its name, as the asking device labels them.
    pub action_label: String,
    pub resource_label: String,
    /// The arguments it would run with, as a reviewer reads them
    /// ([`Draft::summary`](crate::draft::Draft::summary)): secrets withheld.
    pub arguments: Vec<(String, String)>,
    /// What the action risks, by [`RiskLevel::name`](crate::RiskLevel::name).
    pub risk: String,
    /// What it is able to do, by [`ActionKind`](crate::ActionKind)'s name
    /// (`read`, `undoable`, `outside`); empty when the asker cannot say.
    pub kind: String,
    /// Whether Undo takes it back
    /// ([`ActionContract::can_be_undone`](crate::ActionContract::can_be_undone)).
    pub undoable: bool,
    /// What the agent asks in its own words, where it asks the person
    /// something rather than to allow an action ([`ASK`]): the action fields
    /// above are empty then, and the person answers with one of its choices.
    #[serde(default)]
    pub ask: Option<PersonAsk>,
}

/// The person's answer to a [`ConfirmationQuestion`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmationReply {
    /// Run this one invocation.
    AllowOnce,
    /// Do not carry it out; for a [`PersonAsk`], the person chose not to answer.
    Deny,
    /// The person's answer to a [`PersonAsk`]: one of its choices.
    Replied(String),
}

/// The provider an agent asks its person under: its [`ASK`] puts a
/// [`PersonAsk`] to the person the agent acts for, on their own devices, and
/// succeeds with their answer ([`PersonAnswer`]).
pub const ASK_PROVIDER: &str = "tilo.person";
/// The resource [`ASK`] is an action of: the person the caller acts for.
pub const ASK_RESOURCE: &str = "person";
/// Ask the person a question and wait for their answer.
pub const ASK: &str = "ask";
/// The longest question, in characters.
pub const MAX_ASK_CHARS: usize = 2_000;
/// The most choices a question offers.
pub const MAX_ASK_CHOICES: usize = 8;
/// The longest choice, in characters.
pub const MAX_CHOICE_CHARS: usize = 80;

/// What a question with no choices is refused with: words come as a reply in the conversation,
/// and a secret only through the vault's `ask`.
pub const ASK_WITH_CHOICES: &str = "choices: a question is answered only with one of its \
     choices, at least one. To hear your person in their own words, ask in the conversation \
     and read their reply there. For a password, a key or another secret, use the vault's \
     `ask` (Ask your person for a secret).";

/// What [`ASK`] is invoked on.
#[must_use]
pub fn ask_target() -> super::RemoteActionTarget {
    super::RemoteActionTarget {
        provider: ASK_PROVIDER.to_owned(),
        address: super::ActionAddress::new(ASK_RESOURCE, ASK),
    }
}

/// Whether `target` asks the person.
#[must_use]
pub fn is_ask(target: &super::RemoteActionTarget) -> bool {
    target.provider == ASK_PROVIDER
        && target.address.resource == ASK_RESOURCE
        && target.address.action == ASK
}

/// A question an agent puts to its person in its own words, answered with one of its choices.
/// It waits on their devices until they answer. Nothing in it holds words of the person's:
/// what they want to say in their own words they say in the conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersonAsk {
    /// The question, as the agent words it.
    pub question: String,
    /// The answers the person picks from, in order: at least one.
    pub choices: Vec<String>,
}

impl PersonAsk {
    /// The question as it is put: trimmed, with its choices.
    ///
    /// # Errors
    /// What makes it no question to put: empty, too long, no choices ([`ASK_WITH_CHOICES`]), or
    /// choices that are empty, repeated, too long or too many.
    pub fn checked(mut self) -> Result<Self, String> {
        let question = self.question.trim().to_owned();
        self.question = question;
        if self.question.is_empty() || self.question.chars().count() > MAX_ASK_CHARS {
            return Err(format!("question: 1 to {MAX_ASK_CHARS} characters"));
        }
        if self.choices.is_empty() {
            return Err(ASK_WITH_CHOICES.to_owned());
        }
        if self.choices.len() > MAX_ASK_CHOICES {
            return Err(format!("choices: at most {MAX_ASK_CHOICES}"));
        }
        for choice in &mut self.choices {
            *choice = choice.trim().to_owned();
        }
        if self.choices.iter().any(|choice| {
            choice.is_empty()
                || choice.chars().count() > MAX_CHOICE_CHARS
                || choice.chars().any(char::is_control)
        }) {
            return Err(format!(
                "choices: each 1 to {MAX_CHOICE_CHARS} characters on one line"
            ));
        }
        let mut seen = std::collections::HashSet::new();
        if !self.choices.iter().all(|choice| seen.insert(choice)) {
            return Err("choices: each once".to_owned());
        }
        Ok(self)
    }

    /// Whether `reply` answers it: one of its choices.
    #[must_use]
    pub fn answered_by(&self, reply: &str) -> bool {
        let reply = reply.trim();
        self.choices.iter().any(|choice| choice == reply)
    }
}

/// What [`ASK`] succeeds with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonAnswer {
    /// The choice the person picked, as the question words it.
    Answered { choice: String },
    /// The person chose not to answer.
    Declined,
}

/// The answer a provider that cannot ask has for a [`RemoteActionMessage::ConfirmationRequest`].
#[must_use]
pub(crate) fn unanswered(
    correlation: u64,
    error: super::ActionCallError,
) -> super::RemoteActionMessage {
    super::RemoteActionMessage::ConfirmationResponse {
        correlation,
        result: Err(error),
    }
}
