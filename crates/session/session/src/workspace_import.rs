//! What a desktop kept of its person's workspaces itself, before this computer's Tilo service kept
//! them: the key groups its session sealed under its own master key, the workspaces its own
//! workspace service governed, and the workspaces it took part in, handed to that service once
//! (`account/workspaces#import`) and forgotten here once it took them in.

use std::path::Path;

use serde_json::{Value, json};
use tilo_session_client::engine::Engine;
use tilo_session_client::workspace_keys::vault;
use tilo_workspace_service::workspaces::{MovingService, moved_out, moving_out};

/// Hand this computer's Tilo service, through `service`, what the desktop whose state is under
/// `state` (its `XDG_STATE_HOME`) and whose workspaces `engine` holds kept itself: its key groups
/// with the key they were sealed with, what its own workspace service governed, read and not
/// written, each other workspace it took part in with the address its host signed, and whether
/// it was still to found its person's own workspace. Once the service took them in the groups
/// are forgotten here and the desktop's service is marked moved, so they are handed over once,
/// and the mark that this device founds its person's workspace goes when the service founded it.
///
/// # Errors
/// What was kept cannot be read, the service did not take it in, or it cannot be forgotten here
/// after; what was not taken in stays, and is handed over again on the next start.
pub async fn import(
    engine: &Engine,
    state: &Path,
    service: &tilo_api::client::Client,
) -> Result<(), String> {
    let root = tilo_paths::desktop_service::dir(state);
    let moving = moving_out(&root)?;
    let keys = vault::carried(state)?;
    if keys.is_none() && moving.is_none() {
        return Ok(());
    }
    let identity = tilo_identity::store::identity_dir_from_env().ok();
    let founds = identity
        .as_deref()
        .is_some_and(tilo_identity::provisioning::founds_person_workspace);
    // A workspace the desktop's own service governed moves with it; the rest are another
    // device's, entered at the address their host signed.
    let governed: Vec<String> = moving
        .iter()
        .flat_map(|moving| {
            moving
                .workspaces
                .iter()
                .map(|workspace| workspace.id.to_string())
        })
        .collect();
    let workspaces: Vec<Value> = engine
        .workspaces()
        .into_iter()
        .map(|workspace| workspace.to_string())
        .filter(|workspace| !governed.contains(workspace))
        .map(|workspace| {
            json!({
                "hint": hint_of(engine, &workspace),
                "workspace": workspace,
            })
        })
        .collect();
    let carried = json!({
        "sealed_with": keys.as_ref().map(|keys| keys.sealed_with.clone()),
        "groups": keys.as_ref().map(|keys| keys.groups.clone()).unwrap_or_default(),
        "workspaces": workspaces,
        "founds_person_workspace": founds,
        "service": moving.as_ref().map(MovingService::to_hex).transpose()?,
    });
    let answer = service
        .account("account/workspaces", "import", &carried)
        .await
        .map_err(|error| format!("this computer's Tilo service did not take them in: {error}"))?;
    if keys.is_some() {
        vault::forget_carried(state)?;
    }
    if moving.is_some() {
        moved_out(&root)?;
    }
    if answer.get("founded_person_workspace") == Some(&Value::Bool(true))
        && let Some(dir) = identity
    {
        tilo_identity::provisioning::clear_founding_mark(&dir)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The address `workspace`'s host signed for it, as `engine` keeps it.
fn hint_of(engine: &Engine, workspace: &str) -> Option<tilo_peer_state::join::TransportHint> {
    engine
        .host_hints()
        .into_iter()
        .filter(|(named, _)| named == workspace)
        .find_map(|(_, hint)| {
            postcard::from_bytes(&hint)
                .inspect_err(|error| {
                    tracing::warn!(%error, workspace, "a host's address for a workspace does not read");
                })
                .ok()
        })
}
