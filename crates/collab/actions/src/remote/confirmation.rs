//! Asking the person, on their own device, about one invocation a device of
//! theirs hosts.
//!
//! An action the person's grants leave to them asks first. At a device that
//! is not theirs there is nobody at its screen to ask, so the question
//! travels to one of their own devices and back. What comes back names that
//! one invocation; nothing about a grant changes.

use serde::{Deserialize, Serialize};

/// Explicit confirmation carried with an invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confirmation {
    Confirmed,
    /// The caller typed the phrase the contract asks for.
    Phrase(String),
    /// The person the caller acts for confirmed this very invocation on
    /// their own device: the host hands the caller this token, which names
    /// that answer. Only a person acting themselves confirms by their own
    /// word; anyone acting for them carries the person's answer.
    Witnessed(String),
}

/// What a confirmation is bound to of an invocation's arguments: their
/// digest, so a host that keeps the person's answer for one invocation keeps
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

/// The person's answer to [`ActionHost::confirm`](super::ActionHost::confirm):
/// the confirmation to invoke again with, and the arguments to invoke with.
///
/// Where the person changed the draft they were shown
/// ([`Draft::apply_edits`](crate::Draft::apply_edits)), `arguments` holds
/// the arguments as they changed them, and the confirmation is bound to
/// exactly those: the caller invokes again with them, under the same
/// invocation id. The host never swaps arguments on its own; arguments
/// other than these are refused, as any other confirmation is for
/// arguments it was not given for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedInvocation {
    pub confirmation: Confirmation,
    /// The person's arguments; `None` when they changed nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ConfirmedInvocation {
    /// The person confirmed the arguments as they were asked about.
    #[must_use]
    pub const fn unchanged(confirmation: Confirmation) -> Self {
        Self {
            confirmation,
            arguments: None,
        }
    }

    /// `request`, ready to be invoked again: carrying the confirmation, and
    /// the person's arguments where they changed them.
    pub fn apply(self, request: &mut super::InvokeRequest) {
        request.confirmation = Some(self.confirmation);
        if let Some(arguments) = self.arguments {
            request.argument = super::InvokeArgument::Json(arguments);
        }
    }
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
    /// The workspace the invocation runs in.
    // Sent as `room`: a frame reaches peers on other builds, which read the key they know.
    #[serde(rename = "room", alias = "workspace")]
    pub workspace: String,
    /// The run the invocation is a step of, by the title its agent gave it;
    /// empty when it named none.
    pub run: String,
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
}

/// The person's answer to a [`ConfirmationQuestion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmationReply {
    /// Run this one invocation.
    AllowOnce,
    /// Do not run it.
    Deny,
}

/// The answer a host that cannot ask has for a [`RemoteActionMessage::ConfirmationRequest`].
#[must_use]
pub(crate) fn unanswered(
    correlation: u64,
    error: super::ActionHostError,
) -> super::RemoteActionMessage {
    super::RemoteActionMessage::ConfirmationResponse {
        correlation,
        result: Err(error),
    }
}
