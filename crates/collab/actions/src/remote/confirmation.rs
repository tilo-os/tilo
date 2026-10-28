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
