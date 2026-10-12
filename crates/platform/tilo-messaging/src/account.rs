//! Non-secret account lifecycle, reported by the owning host rather than metadata.

/// Operational health is information, never a sending or credential grant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountState {
    #[default]
    Unknown,
    Unavailable,
    Linking,
    Locked,
    Offline,
    Ready,
    NeedsRelink,
    Failed,
}

impl AccountState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
            Self::Linking => "linking",
            Self::Locked => "locked",
            Self::Offline => "offline",
            Self::Ready => "ready",
            Self::NeedsRelink => "needs-relink",
            Self::Failed => "failed",
        }
    }
}
