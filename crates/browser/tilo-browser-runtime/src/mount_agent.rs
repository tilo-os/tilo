//! A standalone mount that acts as an agent (`MountOptions.agent`).
//!
//! Its author is an agent task participant, admitted into the app with its
//! display name the way a host worker admits a delivered task, so the app
//! attributes what the mount does to that agent.

use tilo_actions::ParticipantId;

/// The longest display name an agent mount may carry.
pub(crate) const MAX_AGENT_LABEL_BYTES: usize = 64;

/// The participant and display name of a mount whose options name an agent;
/// `None` for an ordinary mount.
///
/// # Errors
///
/// Refuses an author that is not `agent-task:<task>` and an empty or
/// oversized display name.
pub(crate) fn mount_agent(
    author: &str,
    agent: Option<&str>,
) -> Result<Option<(ParticipantId, String)>, String> {
    let Some(label) = agent else {
        return Ok(None);
    };
    let participant = ParticipantId::new(author)
        .ok()
        .filter(|participant| {
            participant
                .agent_task()
                .is_some_and(|task| !task.is_empty())
        })
        .ok_or("an agent mount's author must be an agent task participant: agent-task:<task>")?;
    let label = label.trim();
    if label.is_empty() || label.len() > MAX_AGENT_LABEL_BYTES {
        return Err(format!(
            "an agent's display name must be 1 to {MAX_AGENT_LABEL_BYTES} bytes"
        ));
    }
    Ok(Some((participant, label.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_mount_is_an_agent_task_participant_with_a_display_name() {
        assert_eq!(mount_agent("Visitor", None).unwrap(), None);
        let (participant, label) = mount_agent("agent-task:reading", Some(" Reading agent "))
            .unwrap()
            .unwrap();
        assert_eq!(participant.as_str(), "agent-task:reading");
        assert_eq!(participant.agent_task(), Some("reading"));
        assert_eq!(label, "Reading agent");
        for author in ["Visitor", "agent-task:", "agent-task:with space", ""] {
            assert!(
                mount_agent(author, Some("Reading agent")).is_err(),
                "{author}"
            );
        }
        assert!(mount_agent("agent-task:reading", Some("  ")).is_err());
        let long = "a".repeat(MAX_AGENT_LABEL_BYTES + 1);
        assert!(mount_agent("agent-task:reading", Some(&long)).is_err());
    }
}
