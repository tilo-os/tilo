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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationQuestion {
    /// Names this question, so the answer names it back.
    pub id: [u8; 32],
    /// The device that asks, as it is labelled to the person.
    pub device_label: String,
    /// The room the invocation runs in.
    pub room: String,
    /// The action, and what it acts on, as the asking device labels them.
    pub action_label: String,
    pub resource_label: String,
    /// What the action risks, by [`RiskLevel::name`](crate::RiskLevel::name).
    pub risk: String,
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
