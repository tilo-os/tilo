//! Making a workspace of the person's: on the device that keeps their root, and nowhere else.
//!
//! The person's workspaces are founded on a device that keeps their root: the computer they
//! first set Tilo up on. Another device of theirs never founds one. It keeps the ask
//! (`account/workspaces#create`) as its record in the person's own workspace, which both keep,
//! for a device that keeps the root ([`tilo_actions::remote::asked::AT_ROOT`]), and waits there
//! for how it ended ([`ask`]). That device takes the record in through the runner that runs every
//! call kept for the root ([`tilo_workspace_service::workspaces::at_root`]), founds the workspace
//! there (`Workspaces::found_asked`), lists it with the person's others, and writes into the record how it
//! went. The asking device takes the workspace in as every device of the person takes in each
//! workspace of their list. A root no device reaches is said so within [`WAITED`], and nothing
//! else founds it.
//!
//! The root runs only what the person's own other devices ask, as the person, and their grants
//! for that device decide the rest (`Workspaces::found_decides`): a computer on an operating
//! system Tilo does not control makes nothing that reaches out without the person's yes, asked
//! on the root's device, and declined as one.

use std::time::Duration;

use serde_json::Value;
use tilo_action_policy::roles::person_workspace_id;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::Caller;
use tilo_api::session::{Creating, NotFounded};
use tilo_workspace_service::workspaces::NotAtRoot;
use tilo_workspace_service::workspaces::at_root;

use super::workspaces::ServiceWorkspaces;

/// How long the device asking waits for a device that keeps the person's root to take its ask
/// in: the bound the daemon waits on any part of the person's that does not answer.
pub(crate) const WAITED: Duration = crate::daemon::WAITED_WITHIN;

/// How long the device asking waits for the person's answer on that device: long enough to walk
/// over to it and answer.
pub(crate) const ANSWERED_WITHIN: Duration = Duration::from_secs(5 * 60);

/// Make `workspace`, called `name`, as the person's shell asks it here: here on a device that
/// keeps their root, and on one that does otherwise ([`ask`]), which lists it with their others
/// so every device of theirs takes it in.
///
/// # Errors
/// This device did not make it, or the root's device did not.
pub(crate) async fn found(
    workspaces: &ServiceWorkspaces,
    workspace: tilo_desktop_model::WorkspaceId,
    name: &str,
) -> Result<(), String> {
    if super::services::holds_root_for_person(workspaces.device_cert().user_public_key) {
        return workspaces.found(workspace, name).await.map(drop);
    }
    let creating = Creating {
        name: name.to_owned(),
        labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
        id: Some(workspace.into()),
    };
    ask(workspaces, creating)
        .await
        .map(drop)
        .map_err(|refused| refused.to_string())
}

/// Have a device that keeps the person's root make the workspace `creating` names: kept as its
/// record in their own workspace, which both keep, asked by the device `workspaces` serves as
/// itself, and followed there until it ended. That device's answer as it gave it, once the
/// workspace it made is held here too.
///
/// # Errors
/// No device that keeps the root took the ask in within [`WAITED`], the person said no there or
/// did not answer within [`ANSWERED_WITHIN`], or it was refused.
pub(crate) async fn ask(
    workspaces: &ServiceWorkspaces,
    creating: Creating,
) -> Result<Value, NotFounded> {
    let own = workspaces.device_cert();
    let person = own.user_public_key;
    let workspace = person_workspace_id(&person);
    let facts = at_root::found_question(&creating.name, &own.device_label);
    let id = creating.id.map(|id| id.to_string());
    let request = at_root::found_request(&creating.name, &creating.labels, id.as_deref());
    // Asked as this device, never signed as the person: the root's device decides it by the
    // person's grants for this device, and the person answers there what they leave to them.
    let asking =
        Caller::authenticated(person, own.device_public_key).in_workspace(workspace.clone());
    let ended = workspaces
        .copies()
        .ask_at_root(
            &workspace,
            (&asking, &request),
            &facts,
            (WAITED, ANSWERED_WITHIN),
        )
        .await;
    let mut made = match ended {
        Ok(InvocationUpdate::Succeeded { output, .. }) => output.value.unwrap_or(Value::Null),
        Ok(InvocationUpdate::Failed { error, .. }) => {
            return Err(NotFounded::Refused(error.message));
        }
        Ok(InvocationUpdate::Cancelled { message, .. })
            if message.as_deref() == Some(tilo_actions::remote::asked::DECLINED) =>
        {
            return Err(NotFounded::Declined);
        }
        Ok(InvocationUpdate::Cancelled { message, .. }) => {
            return Err(NotFounded::Refused(message.unwrap_or_default()));
        }
        Ok(other) => return Err(NotFounded::Refused(format!("it ended as {other:?}"))),
        Err(NotAtRoot::Unreached(waited)) => return Err(NotFounded::Unreached { waited }),
        Err(NotAtRoot::Unanswered(waited)) => return Err(NotFounded::Unanswered { waited }),
        Err(NotAtRoot::Unkept(why)) => return Err(NotFounded::Refused(why)),
    };
    // The root's device answers what it keeps; this device takes the workspace in by itself as
    // its copy of the person's list names it, so it says what this one holds now.
    if let Some(answered) = made.get_mut("workspace").and_then(Value::as_object_mut) {
        let held = match answered.get("id").and_then(Value::as_str) {
            Some(id) => workspaces.copies().state(id).await.is_some(),
            None => false,
        };
        answered.insert("kept_here".to_owned(), Value::Bool(held));
    }
    Ok(made)
}

#[cfg(test)]
#[path = "founding_create_tests.rs"]
mod tests;
