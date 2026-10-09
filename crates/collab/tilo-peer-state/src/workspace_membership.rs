//! Typed native workspace membership commands, not grant or identity blobs.
use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::AgentPreset;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceMembershipCommand {
    AddAgent {
        agent: String,
        preset: AgentPreset,
    },
    RemoveAgent {
        agent: String,
    },
    /// Delegate normal app authorship after an authenticated person opens it.
    GrantAppAuthorship {
        app: String,
    },
}

impl WorkspaceMembershipCommand {
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::AddAgent { agent, .. } | Self::RemoveAgent { agent } => Some(agent),
            Self::GrantAppAuthorship { .. } => None,
        }
    }

    /// Validate the canonical identifier before entering the owner turn.
    ///
    /// # Errors
    /// Refuses empty, unbounded, unnormalised or control-containing identifiers.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::GrantAppAuthorship { app } = self {
            if app.is_empty()
                || app.len() > 128
                || app.split('.').any(str::is_empty)
                || !app.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b'_')
                })
            {
                return Err("invalid canonical application identifier".into());
            }
            return Ok(());
        }
        let agent = self.agent().ok_or("missing canonical agent identifier")?;
        if agent.is_empty()
            || agent.len() > 128
            || agent.trim() != agent
            || agent.to_lowercase() != agent
            || agent.chars().any(char::is_control)
        {
            return Err("invalid canonical workspace agent identifier".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{Answer, Subject, TreePath, roles};

    #[test]
    fn presets_are_closed_admission_markers_without_admin_authority() {
        use tilo_action_policy::{ActionKind, Authority, Context, Party, RoomPolicy};
        let room = RoomPolicy::new("room", "owner");
        let content = TreePath::room("room").app("notes").resource("page");
        let comment = content.clone().field(roles::COMMENTS_FIELD).action("add");
        let invite = roles::governance_action("room", roles::governance::INVITE);
        for (id, read, undoable, comments, outside) in [
            (
                "observer",
                Answer::Allow,
                Answer::Deny,
                Answer::Deny,
                Answer::Deny,
            ),
            (
                "contributor",
                Answer::Allow,
                Answer::Deny,
                Answer::Allow,
                Answer::Deny,
            ),
            (
                "operator",
                Answer::Allow,
                Answer::Allow,
                Answer::Allow,
                Answer::Ask,
            ),
            (
                "custom",
                Answer::Allow,
                Answer::Deny,
                Answer::Deny,
                Answer::Deny,
            ),
        ] {
            let preset = AgentPreset::parse(id).unwrap();
            assert_eq!(preset.id(), id);
            let grants = preset.grants("room", "scribe", &Subject::person("owner"));
            let authority = Authority::person(room.party("owner"))
                .bounded_by(Party::new(vec![Subject::agent("scribe")], &grants));
            let at = |node: &TreePath, kind| authority.decide(node, kind, &Context::at(0)).answer;
            assert_eq!(at(&content, ActionKind::Read), read, "{id}");
            assert_eq!(at(&content, ActionKind::Undoable), undoable, "{id}");
            assert_eq!(at(&comment, ActionKind::Undoable), comments, "{id}");
            assert_eq!(at(&content, ActionKind::Outside), outside, "{id}");
            assert_eq!(
                at(&invite, roles::GOVERNANCE_KIND),
                Answer::Deny,
                "{id} never governs"
            );
        }
        for unknown in ["admin", "owner", "Observer", "operator ", ""] {
            assert!(AgentPreset::parse(unknown).is_none());
        }
    }

    #[test]
    fn commands_refuse_ambiguous_or_unbounded_agent_identifiers() {
        for agent in ["", "Scribe", " scribe", "scribe\n", &"x".repeat(129)] {
            assert!(
                WorkspaceMembershipCommand::RemoveAgent {
                    agent: agent.into()
                }
                .validate()
                .is_err()
            );
        }
        WorkspaceMembershipCommand::AddAgent {
            agent: "scribe".into(),
            preset: AgentPreset::Observer,
        }
        .validate()
        .unwrap();
    }
}
