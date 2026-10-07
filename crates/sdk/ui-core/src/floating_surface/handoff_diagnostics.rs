//! Opt-in, content-free milestones for locating a stalled native handoff.
//! Each stage is emitted at most once per process, not once per frame or ID.

use std::sync::atomic::{AtomicU16, Ordering};

/// Closed milestones; no app identity, geometry, content or error is accepted.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub enum Stage {
    ClientAvailable,
    ClientUnavailable,
    PresentDequeued,
    PresentSendAccepted,
    PresentUnhosted,
    HostPresent,
    HostPrepareIssued,
    PrepareReceived,
    NativeWindowCreated,
    FirstBufferPresented,
}

impl Stage {
    const fn properties(self) -> (u16, &'static str) {
        match self {
            Self::ClientAvailable => (1, "client_available"),
            Self::ClientUnavailable => (2, "client_unavailable"),
            Self::PresentDequeued => (4, "present_dequeued"),
            Self::PresentSendAccepted => (8, "present_send_accepted"),
            Self::PresentUnhosted => (16, "present_unhosted"),
            Self::HostPresent => (32, "host_present"),
            Self::HostPrepareIssued => (64, "host_prepare_issued"),
            Self::PrepareReceived => (128, "prepare_received"),
            Self::NativeWindowCreated => (256, "native_window_created"),
            Self::FirstBufferPresented => (512, "first_buffer_presented"),
        }
    }
}

static SEEN: AtomicU16 = AtomicU16::new(0);

fn claim(seen: &AtomicU16, stage: Stage) -> bool {
    let (mask, _) = stage.properties();
    seen.fetch_or(mask, Ordering::Relaxed) & mask == 0
}

/// Record a real completed milestone only when the dedicated debug target is enabled.
#[doc(hidden)]
pub fn mark(stage: Stage) {
    if tracing::enabled!(target: "tilo_floating_handoff", tracing::Level::DEBUG)
        && claim(&SEEN, stage)
    {
        let (_, stage) = stage.properties();
        tracing::debug!(target: "tilo_floating_handoff", stage, "native floating handoff");
    }
}

#[cfg(test)]
mod tests {
    use super::{AtomicU16, Ordering, Stage, claim};

    #[test]
    fn handoff_diagnostics_are_closed_and_bounded_once_per_stage() {
        let seen = AtomicU16::new(0);
        let stages = [
            (Stage::ClientAvailable, "client_available"),
            (Stage::ClientUnavailable, "client_unavailable"),
            (Stage::PresentDequeued, "present_dequeued"),
            (Stage::PresentSendAccepted, "present_send_accepted"),
            (Stage::PresentUnhosted, "present_unhosted"),
            (Stage::HostPresent, "host_present"),
            (Stage::HostPrepareIssued, "host_prepare_issued"),
            (Stage::PrepareReceived, "prepare_received"),
            (Stage::NativeWindowCreated, "native_window_created"),
            (Stage::FirstBufferPresented, "first_buffer_presented"),
        ];
        for (stage, label) in stages {
            assert_eq!(stage.properties().1, label);
            assert!(claim(&seen, stage));
            for _ in 0..100 {
                assert!(!claim(&seen, stage));
            }
        }
        assert_eq!(seen.load(Ordering::Relaxed).count_ones(), 10);
    }
}
