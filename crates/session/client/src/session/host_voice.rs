//! The host's word for the runs it executes, said and heard outside the live
//! session ([`tilo_server_protocol::host_voice`]).
//!
//! The host's word is a record it writes into the workspace, or an action it
//! answers; this session carries none of its own. What it keeps here is the one
//! rule the live session still applies: a run's narration travels only as the
//! host's own, never on the live session.

use tilo_server_protocol::live::{LiveFrame, OpsFrame};

/// Whether an opened live `frame` heard on the live session narrates a run: a run's
/// narration travels only as the host's own, never on the live session.
pub(super) fn live_session_narrates_a_run(frame: &[u8]) -> bool {
    matches!(
        LiveFrame::decode(frame),
        Ok(LiveFrame::Ops(OpsFrame::Semantic { event, .. })) if event.on_behalf.is_some()
    )
}
