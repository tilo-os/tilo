//! How long an answer stands.

use serde::{Deserialize, Serialize};

/// How long an answer stands: the one lifetime every grant, sandbox capability and permission
/// prompt uses.
///
/// Ordered by how long it lives: once, the session, an earlier time, a later one, until revoked.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    /// For one use; the API that used it revokes it. The default: the least an answer gives.
    #[default]
    Once,
    /// While this session lasts. Kept in the device's memory only, so it ends with the session.
    Session,
    /// Until a time, in Unix milliseconds (exclusive).
    Until(u64),
    /// Until someone revokes it.
    UntilRevoked,
}

/// How long the timed answer a prompt offers lasts: an hour.
pub const OFFERED_SPAN_MS: u64 = 60 * 60 * 1000;

impl Lifetime {
    /// Whether it is still live at `now_ms` (Unix milliseconds). `None` skips the clock, for a
    /// checker that shares none with the writer; a one-use or session answer ends by an event,
    /// not at an instant.
    #[must_use]
    pub const fn is_live_at(self, now_ms: Option<u64>) -> bool {
        match (self, now_ms) {
            (Self::Until(at), Some(now)) => now < at,
            _ => true,
        }
    }

    /// Whether an answer with it is written down: one-use and session answers stay in memory,
    /// so keeping them can never turn them into lasting ones.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        matches!(self, Self::Until(_) | Self::UntilRevoked)
    }

    /// The durations every permission prompt offers, in the order it offers them: once, this
    /// session, for an hour from `now_ms`, until taken back. A sheet's answer is an index into it.
    #[must_use]
    pub const fn offered(now_ms: u64) -> [Self; 4] {
        [
            Self::Once,
            Self::Session,
            Self::Until(now_ms.saturating_add(OFFERED_SPAN_MS)),
            Self::UntilRevoked,
        ]
    }

    /// The choice a sheet answers with for it: its place in [`Self::offered`].
    #[must_use]
    pub const fn choice(self) -> usize {
        match self {
            Self::Once => 0,
            Self::Session => 1,
            Self::Until(_) => 2,
            Self::UntilRevoked => 3,
        }
    }

    /// The offered duration a sheet's answer `choice` picks, at `now_ms`.
    #[must_use]
    pub fn chosen(choice: usize, now_ms: u64) -> Option<Self> {
        Self::offered(now_ms).get(choice).copied()
    }

    /// Its stable name, which labels and stored answers key on: `once`, `session`, `until` or
    /// `always`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Until(_) => "until",
            Self::UntilRevoked => "always",
        }
    }
}
