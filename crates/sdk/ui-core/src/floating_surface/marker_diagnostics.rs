//! Opt-in, bounded diagnostics for the real painted-marker publication door.
//! No identifiers, labels, content, or raw transport errors are accepted.

use std::sync::atomic::{AtomicU64, Ordering};
use vello::kurbo::Rect;

#[derive(Clone, Copy)]
pub enum Stage {
    MissingSurface,
    MissingViewport,
    MissingWorkspace,
    NoPaintedTargets,
    PaintedTargets,
    CacheUnchanged,
    EmptySent,
    TargetsSent,
    SendFailed,
    HostEmpty,
    HostMalformed,
    HostSourceMissing,
    HostOutsideSource,
    HostAppMismatch,
    HostActorMismatch,
    HostCurrentMissing,
    HostCurrentMismatch,
    HostAccepted,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::MissingSurface => "missing_surface",
            Self::MissingViewport => "missing_viewport",
            Self::MissingWorkspace => "missing_workspace",
            Self::NoPaintedTargets => "no_painted_targets",
            Self::PaintedTargets => "painted_targets",
            Self::CacheUnchanged => "cache_unchanged",
            Self::EmptySent => "empty_sent",
            Self::TargetsSent => "targets_sent",
            Self::SendFailed => "send_failed",
            Self::HostEmpty => "host_empty",
            Self::HostMalformed => "host_malformed",
            Self::HostSourceMissing => "host_source_missing",
            Self::HostOutsideSource => "host_outside_source",
            Self::HostAppMismatch => "host_app_mismatch",
            Self::HostActorMismatch => "host_actor_mismatch",
            Self::HostCurrentMissing => "host_current_missing",
            Self::HostCurrentMismatch => "host_current_mismatch",
            Self::HostAccepted => "host_accepted",
        }
    }
}

#[derive(Debug)]
pub enum SequenceClass {
    Missing,
    Same,
    Older,
    Newer,
}

#[derive(Debug)]
pub struct HostChecks {
    /// Marker origin/end, then locus origin/end, inside the source app.
    pub inside: [bool; 4],
    /// Source app, authenticated actor, complete current interaction.
    pub identity: [bool; 3],
    /// Phase, resource, action, anchor against the shell's current entry.
    pub interaction: [bool; 4],
    pub sequence: SequenceClass,
}

static SEEN: AtomicU64 = AtomicU64::new(0);

fn claim(seen: &AtomicU64, stage: Stage) -> bool {
    let mask = 1_u64.checked_shl(stage as u32).unwrap_or(0);
    seen.fetch_or(mask, Ordering::Relaxed) & mask == 0
}

pub fn mark(stage: Stage) {
    if tracing::enabled!(target: "tilo_floating_handoff", tracing::Level::DEBUG)
        && claim(&SEEN, stage)
    {
        tracing::debug!(target: "tilo_floating_handoff", marker_stage = stage.label(),
            "native agent marker publication");
    }
}

/// Record one rejection class at most once. Rectangles are the actual public
/// app/marker/locus geometry, never coordinates reconstructed from an ID.
pub fn host(stage: Stage, checks: HostChecks, geometry: [Rect; 3]) {
    if tracing::enabled!(target: "tilo_floating_handoff", tracing::Level::DEBUG)
        && claim(&SEEN, stage)
    {
        tracing::debug!(target: "tilo_floating_handoff", marker_stage = stage.label(),
            ?checks, ?geometry, "native agent marker publication");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_diagnostic_restatements_are_bounded_per_rejection_class() {
        let seen = AtomicU64::new(0);
        for stage in [
            Stage::TargetsSent,
            Stage::HostCurrentMissing,
            Stage::HostAccepted,
        ] {
            assert!(claim(&seen, stage));
            for _ in 0..100 {
                assert!(!claim(&seen, stage));
            }
        }
        assert_eq!(seen.load(Ordering::Relaxed).count_ones(), 3);
    }
}
