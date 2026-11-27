//! A person's assistant in a workspace, as the steps that work for them reach it.
//!
//! A model step of a person's shortcut is answered by the model they chose for that kind of
//! work, asked with their own key, and counted on the day of the assistant they keep in the
//! workspace ([`transforms`]), so the limit they set their assistant bounds what their events
//! spend as well. What an event hands over is a turn of that assistant, asked through Chat's
//! own verb for it ([`super::events`]).

pub mod transforms;
pub use transforms::Transforms;

use tilo_workspace_agent::{Agent, WorkspaceAgents};

use tilo_workspace_service::workspaces::Workspaces;

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
    let agents = super::opened::<WorkspaceAgents>((workspace, &record), &mut doc)?;
    Ok(agents
        .agents
        .iter()
        .filter(|agent| {
            agent.is_active()
                && agent.mcp_client.is_none()
                && tilo_failure::ignored!(
                    "a person that is no person's id is not this person",
                    tilo_member_types::PersonId::parse(&agent.acts_for).ok()
                )
                .and_then(|acts_for| acts_for.root())
                .as_ref()
                    == Some(person)
        })
        .min_by_key(|agent| agent.created_at)
        .cloned())
}

/// The person a step is for, by their root.
///
/// # Errors
/// `person` is no root.
pub fn person_of(person: &str) -> Result<[u8; 32], String> {
    hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| "the step works for nobody this device knows".to_owned())
}
