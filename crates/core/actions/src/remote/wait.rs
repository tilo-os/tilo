//! How far a caller had followed an invocation, and how long a provider holds a waiting answer:
//! what a waiting request ([`RemoteActionMessage::Observe`] or [`InvokeRequest`]) names.
//!
//! [`RemoteActionMessage::Observe`]: super::RemoteActionMessage::Observe
//! [`InvokeRequest`]: super::InvokeRequest

use serde::{Deserialize, Serialize};
use tilo_clock::time::{Duration, Instant};

use crate::outcome::InvocationUpdate;

/// The longest a provider holds a waiting answer, whatever `wait_ms` asked, so a
/// server connection or a broker call is never held open indefinitely.
#[allow(clippy::cast_lossless, reason = "u64::from is not a const fn")]
pub const MAX_REMOTE_WAIT: Duration = Duration::from_millis(MAX_REMOTE_WAIT_MS as u64);

/// [`MAX_REMOTE_WAIT`] as the `wait_ms` a request carries: a follow asking for more is held no
/// longer, so it asks for this.
pub const MAX_REMOTE_WAIT_MS: u32 = 10_000;

/// How far an invocation's updates had got when its caller last looked:
/// what a waiting [`Observe`](super::RemoteActionMessage::Observe) asks the
/// provider to move past.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpdateMark {
    Accepted,
    Progress {
        sequence: u64,
    },
    Item {
        sequence: u64,
    },
    /// Settled: nothing follows.
    Finished,
}

impl UpdateMark {
    /// Where `update` stands.
    #[must_use]
    pub const fn of(update: &InvocationUpdate) -> Self {
        match update {
            InvocationUpdate::Accepted { .. } => Self::Accepted,
            InvocationUpdate::Progress { sequence, .. } => Self::Progress {
                sequence: *sequence,
            },
            InvocationUpdate::Item { sequence, .. } => Self::Item {
                sequence: *sequence,
            },
            InvocationUpdate::Succeeded { .. }
            | InvocationUpdate::Failed { .. }
            | InvocationUpdate::Cancelled { .. } => Self::Finished,
        }
    }
}

/// When a wait of `wait_ms` asked now ends, capped at [`MAX_REMOTE_WAIT`].
pub fn deadline(wait_ms: u32) -> Instant {
    let now = tilo_clock::now();
    let wait = Duration::from_millis(u64::from(wait_ms)).min(MAX_REMOTE_WAIT);
    now.checked_add(wait).unwrap_or(now)
}
