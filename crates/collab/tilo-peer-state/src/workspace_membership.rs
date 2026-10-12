//! Typed native workspace membership commands, not grant or identity blobs.
use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::AgentPreset;
use tilo_action_policy::{Answer, Grant, GrantId, KindSet, TreePath};

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
    /// Add to open task `task`'s grant set: work done in the task gets
    /// `answer` for actions of `kinds` at `node`, a path below the room,
    /// for as long as the task is open. A task's grant set only bounds the
    /// work: it never gives anyone more than they hold.
    GrantForTask {
        task: String,
        node: TreePath,
        kinds: KindSet,
        answer: Answer,
    },
    /// Take `grant` out of task `task`'s grant set.
    RevokeForTask {
        task: String,
        grant: GrantId,
    },
    /// Record `grant` in the room, as the caller gives it: they must hold the
    /// room's grant action and may give no more than they hold.
    Grant {
        grant: Grant,
    },
    /// Take back the grant `grant`, as the caller: they must hold the room's
    /// revoke action.
    Revoke {
        grant: GrantId,
    },
}

/// The id of the grant task `task`'s grant set holds for `node`.
#[must_use]
pub fn task_grant_id(task: &str, node: &TreePath) -> GrantId {
    GrantId::new(format!("task-grant:{task}:{node}"))
}

impl WorkspaceMembershipCommand {
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::AddAgent { agent, .. } | Self::RemoveAgent { agent } => Some(agent),
            Self::GrantAppAuthorship { .. }
            | Self::GrantForTask { .. }
            | Self::RevokeForTask { .. }
            | Self::Grant { .. }
            | Self::Revoke { .. } => None,
        }
    }

    /// Validate the canonical identifier before entering the owner turn.
    ///
    /// # Errors
    /// Refuses empty, unbounded, unnormalised or control-containing identifiers.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::GrantForTask { task, .. } | Self::RevokeForTask { task, .. } = self
            && (!task.starts_with("task:")
                || task.len() > 128
                || task.chars().any(|c| c.is_control() || c.is_whitespace()))
        {
            return Err("invalid workspace task identifier".into());
        }
        if let Self::GrantForTask { node, .. } = self
            && node.room_id().is_some()
        {
            return Err("a task grant names a path below the room".into());
        }
        if let Self::RevokeForTask { task, grant } = self
            && !grant.as_str().starts_with(&format!("task-grant:{task}:"))
        {
            return Err("the grant is not in that task's grant set".into());
        }
        if let Self::Grant { grant } = self
            && grant.id.as_str().trim().is_empty()
        {
            return Err("a grant needs an id".into());
        }
        if matches!(
            self,
            Self::GrantForTask { .. }
                | Self::RevokeForTask { .. }
                | Self::Grant { .. }
                | Self::Revoke { .. }
        ) {
            return Ok(());
        }
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
