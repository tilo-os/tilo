//! Workspace-level collaboration experience shared by every application.
//!
//! Pointer/caret presence answers *where*. This module answers the two human
//! questions that should not be inferred independently by every app: what a
//! participant is doing, and what they are trying to achieve. Inference is
//! deliberately local and transient; only the resulting short status is fit
//! for publication, and a manual status always wins.

use crate::time::{Duration, SystemTime};

/// Automatic summaries go stale quickly. A stale intention is worse than no
/// status because it makes collaborators coordinate around work that stopped.
pub const INFERRED_STATUS_TTL: Duration = Duration::from_secs(5 * 60);
/// Manual statuses are explicit communication and therefore remain until the
/// author clears them.
pub const MAX_STATUS_CHARS: usize = 96;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusSharing {
    /// No inferred status is produced or published. Manual status remains
    /// available so privacy never removes the useful explicit path.
    Off,
    /// Only a status typed by the participant is visible.
    ManualOnly,
    /// A local model may summarize recent semantic actions.
    #[default]
    Automatic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusOrigin {
    Manual,
    /// Name of the local generator, surfaced in details rather than disguised
    /// as something the participant literally wrote.
    Inferred {
        generator: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveStatus {
    pub text: String,
    pub origin: StatusOrigin,
    pub updated_at: SystemTime,
}
