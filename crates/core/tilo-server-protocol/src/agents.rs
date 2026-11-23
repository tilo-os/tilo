//! The signing grants of a workspace's roster agents: who an agent is, what it may touch, and
//! how its runner asks the host for its grant.
//!
//! An agent is a device of its runner, the host's own person. It holds its own device key and a
//! root-signed `DeviceCert` issued through ordinary device certification (with no
//! capabilities), and a root-signed [`crate::delegation::Delegation`] naming it an agent, its
//! label and cursor colour, and the workspace and apps it may act in. The host refuses an agent
//! device outside its workspace; browsers verify the delegation against the runner's root
//! before drawing the agent as "yours".
//!
//! The runner asks over `tilo_actions::remote`, from a certified session of the workspace,
//! addressed to the host that governs it:
//!
//! | Resource | Action | Argument | Answer |
//! |---|---|---|---|
//! | [`SUPERVISOR_RESOURCE`] | [`REQUEST_GRANT_ACTION`] | [`RosterGrantRequest`] JSON | `{grant, pairingCode}` |
//! | `agent:<id>` | [`REVOKE_ACTION`] | none | `{}` |
//!
//! A grant is approved on a root-holding device in one step: the host queues a certification
//! request for a fresh agent key carrying the [`crate::delegation::DelegateProposal`], and the
//! approver answers with the agent's certificate and the delegation together
//! ([`crate::certify::ApproverDecision::IssueDelegate`]).

use serde::{Deserialize, Serialize};

/// Provider token of the host's agent grants in remote-action targets.
pub const AGENTS_PROVIDER: &str = "agents";
pub const SUPERVISOR_RESOURCE: &str = "agents";
/// Prefix of one agent's resource id: `agent:<agent id>`.
pub const AGENT_RESOURCE_PREFIX: &str = "agent:";
/// Ask the runner's approver for a roster agent's grant.
pub const REQUEST_GRANT_ACTION: &str = "request-grant";
/// Withdraw a grant: the host refuses its device.
pub const REVOKE_ACTION: &str = "revoke";

/// A grant as the host asks it of the approver: the roster entry's name, colour, workspaces,
/// apps and lifetime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantRequest {
    /// Display name, 1..=`MAX_AGENT_TEXT_BYTES` bytes.
    pub label: String,
    /// Cursor colour `#rrggbb`; the host picks one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Workspace uuids.
    pub workspaces: Vec<String>,
    /// App ids; empty is every app.
    #[serde(default)]
    pub apps: Vec<String>,
    /// How long the grant lasts, in seconds.
    pub expires_in_secs: u64,
}

/// The existing roster form of `request-grant`. Its workspace must be the
/// one that carried the request. The server reads the name from the roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterGrantRequest {
    pub agent_id: String,
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub apps: Vec<String>,
    pub expires_in_secs: u64,
}

/// Parse `#rrggbb`.
#[must_use]
pub fn parse_color_hex(text: &str) -> Option<[u8; 3]> {
    let bytes = hex::decode(text.strip_prefix('#')?).ok()?;
    <[u8; 3]>::try_from(bytes).ok()
}

/// Where a grant stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantState {
    /// Waiting for the approver.
    Pending,
    /// Approved: the agent signs under it.
    #[default]
    Approved,
    /// The approver declined, or nobody answered in time.
    Denied,
    /// Past its expiry.
    Expired,
    /// Withdrawn by a person.
    Revoked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_use_the_css_hex_spelling() {
        assert_eq!(parse_color_hex("#7c3aed"), Some([0x7c, 0x3a, 0xed]));
        assert_eq!(parse_color_hex("7c3aed"), None);
    }
}
