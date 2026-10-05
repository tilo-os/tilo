//! Typed native workspace membership commands, not capability or identity blobs.
use collab_types::caps::Verb;
use serde::{Deserialize, Serialize};

/// Admission profile only. It never grants actions or a human workspace role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMembershipPreset {
    Observer,
    Contributor,
    Operator,
    Custom,
}

impl AgentMembershipPreset {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observer" => Some(Self::Observer),
            "contributor" => Some(Self::Contributor),
            "operator" => Some(Self::Operator),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Contributor => "contributor",
            Self::Operator => "operator",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub const fn admission_verb(self) -> Verb {
        match self {
            Self::Observer | Self::Custom => Verb::Read,
            Self::Contributor => Verb::Comment,
            Self::Operator => Verb::Write,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceMembershipCommand {
    AddAgent {
        agent: String,
        preset: AgentMembershipPreset,
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

    #[test]
    fn presets_are_closed_admission_markers_without_admin_authority() {
        for (id, expected) in [
            ("observer", Verb::Read),
            ("contributor", Verb::Comment),
            ("operator", Verb::Write),
            ("custom", Verb::Read),
        ] {
            let preset = AgentMembershipPreset::parse(id).unwrap();
            assert_eq!(preset.id(), id);
            assert_eq!(preset.admission_verb(), expected);
        }
        for unknown in ["admin", "owner", "Observer", "operator ", ""] {
            assert!(AgentMembershipPreset::parse(unknown).is_none());
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
            preset: AgentMembershipPreset::Observer,
        }
        .validate()
        .unwrap();
    }
}
