//! Driving a [`PeerView`] the way the compositor loop does, for tests that
//! decode a stream frame by frame.

use std::time::{Duration, Instant};

use shell::shell::peer_view::{PeerView, PushOutcome};

/// Push one encoded unit and settle its picture. A picture that outlasted
/// the decoder's wait ([`PushOutcome::Pending`], a loaded runner) is
/// collected as the compositor loop collects it, so a slow decoder reads as
/// slow, never as a rejected unit. A unit the decoder rejected is still
/// [`PushOutcome::DecodeFailed`], and one whose picture never comes is still
/// [`PushOutcome::Pending`] after the hang bound.
pub fn push_and_settle(
    view: &mut PeerView,
    encoded: &[u8],
    latency_ns: Option<u64>,
) -> PushOutcome {
    let outcome = view.push_encoded_with_latency(encoded, latency_ns);
    if outcome != PushOutcome::Pending {
        return outcome;
    }
    let deadline = Instant::now()
        .checked_add(super::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while Instant::now() < deadline {
        if let Some(outcome) = view.collect_late_picture() {
            return outcome;
        }
        if !view.picture_pending() {
            // The decoder gave up on the unit instead of finishing it.
            return PushOutcome::DecodeFailed;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    PushOutcome::Pending
}
