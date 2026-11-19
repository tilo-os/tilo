//! A write whose part has not arrived from the workspace: the service asks
//! for it, hands the app the parts its chains still want, and applies the
//! write again, bounded — so whoever asked hears what the write made. Only a
//! part that does not arrive in time leaves the refusal to its caller.

use std::time::{Duration, Instant};

use tilo_actions::ActionOutput;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::{Needs, Worker};

/// How long a write waits for the part it changes to arrive from the
/// workspace before its caller is told it has not: inside the wait the API's
/// own caller makes, so it hears either what the write made or why not.
pub(super) const PART_WAIT: Duration = Duration::from_secs(30);
/// The first pause between asking the workspace whether the part a write
/// waits for has arrived: short at first, as a part usually arrives at once.
const PART_LOOK: Duration = Duration::from_millis(10);
/// The longest pause between those asks, as a part stays away.
const PART_LOOK_MAX: Duration = Duration::from_millis(250);

/// One operation to apply to what is mounted here, as the request named it.
pub(super) struct Invoked<'a> {
    pub(super) address: &'a tilo_actions::ActionAddress,
    pub(super) arguments: &'a serde_json::Map<String, serde_json::Value>,
    pub(super) context: Option<&'a tilo_actions::ActionContext>,
    pub(super) appearance: Option<&'a tilo_actions::Appearance>,
    pub(super) confirmed: bool,
}

impl Worker {
    /// Apply `invoked` to what is mounted at `holder`: while what it changes
    /// has not arrived from the workspace, ask for it and hand the app the
    /// parts its chains still want, bounded ([`PART_WAIT`]), and apply it
    /// again — so a write into a chain's part answers what it made rather
    /// than nothing. Only a part that does not arrive in that time leaves the
    /// operation's refusal to whoever asked.
    pub(super) fn invoke_on(
        &mut self,
        workspace_id: WorkspaceId,
        holder: &DocumentRoute,
        needs: Needs<'_>,
        invoked: &Invoked<'_>,
    ) -> Result<ActionOutput, tilo_actions::api::ApiError> {
        let until = Instant::now().checked_add(PART_WAIT);
        let mut look = PART_LOOK;
        loop {
            let error = match self.invoke_mounted(holder, invoked) {
                Ok(output) => return Ok(output),
                Err(error) => error,
            };
            let arriving = matches!(
                &error,
                tilo_actions::api::ApiError::Unavailable { code, .. }
                    if code == tilo_actions::api::NOT_READY
            );
            if !arriving || until.is_none_or(|until| Instant::now() >= until) {
                return Err(error);
            }
            let state = self.refresh(workspace_id, needs).map_err(|refused| {
                tilo_actions::api::ApiError::Unavailable {
                    code: refused.code().to_owned(),
                    reason: refused.to_string(),
                }
            })?;
            self.hold_chains(&super::Writer::Mounted(holder.clone()), holder, &state);
            std::thread::park_timeout(look);
            look = look.saturating_mul(2).min(PART_LOOK_MAX);
        }
    }

    /// [`Self::invoke_on`] once: `invoked` on what is mounted at `holder`.
    fn invoke_mounted(
        &mut self,
        holder: &DocumentRoute,
        invoked: &Invoked<'_>,
    ) -> Result<ActionOutput, tilo_actions::api::ApiError> {
        let mounted = self
            .mounted
            .get_mut(holder)
            .ok_or_else(|| tilo_actions::api::ApiError::Unknown(invoked.address.clone()))?;
        mounted.app.invoke_in(
            invoked.context,
            invoked.appearance,
            invoked.address,
            invoked.arguments,
            invoked.confirmed,
        )
    }
}
