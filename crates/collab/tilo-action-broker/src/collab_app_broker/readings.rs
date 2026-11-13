//! The session's granted readings, as the workspace's grants decide them.
//!
//! Most of what a session publishes is everyone's to read: the sound outputs,
//! the displays. A resource the person's own is not, so its publisher declares
//! it ([`Readers::Granted`](tilo_actions::Readers::Granted)), and its reading
//! is decided where the resource sits — `workspace › app › kind › resource`, the
//! node its actions are decided at too — against the grants the workspace holds for
//! the app reading it. A reading asks nobody: a grant that would ask first
//! refuses it, because there is no one to ask about a screenful of state that
//! is redrawn every pass.
//!
//! The decision is the same the action door makes, from the same tree, for the
//! same app: an app reads here exactly the readings the workspace grants it, and
//! borrowing nothing from the person who started it.

use crate::services::{Caller, Granted};

use super::AppBrokerState;

/// What a refused reading says, as a refused action says why it was refused.
pub(super) const REFUSED: &str = "caller is not granted this reading";

/// Whether `caller` may read `resource`, which `granted` declares the workspace's
/// grants decide, now.
///
/// #
/// Errors [`REFUSED`]: the workspace the app was launched in is no longer hosted, or
/// the grants do not let this app read there.
pub(super) fn admit(
    state: &AppBrokerState,
    caller: &Caller,
    resource: &str,
    granted: &Granted,
) -> Result<(), String> {
    let membership = state
        .membership
        .as_ref()
        .ok_or_else(|| REFUSED.to_owned())?;
    let now = state.policy_now_ns().map_err(|_| REFUSED.to_owned())? / 1_000_000_000;
    let subject = tilo_action_policy::Subject::App(caller.app_id.clone());
    let allowed = membership
        .decide_in_workspace(&caller.workspace, |workspace| {
            workspace.is_some_and(|workspace| {
                let node = tilo_actions::resource_node(
                    &workspace.node(),
                    &granted.publisher,
                    &granted.kind,
                    resource,
                );
                tilo_action_policy::Authority::person(workspace.policy.party_of(subject.clone()))
                    .decide(
                        &node,
                        tilo_action_policy::ActionKind::Read,
                        &tilo_desktop_model::context_at(now),
                    )
                    .answer
                    == tilo_action_policy::Answer::Allow
            })
        })
        .map_err(|_| REFUSED.to_owned())?;
    if allowed {
        Ok(())
    } else {
        Err(REFUSED.to_owned())
    }
}
