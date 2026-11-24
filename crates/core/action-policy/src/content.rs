//! What a workspace lets its members' content take, and what a host keeps of
//! it: the limits a workspace's owners set on what members post there.
//!
//! A record names its content; a host keeps it so a member that comes up later
//! reads it from the host alone. The content is bounded, and the bounds are
//! policy: the workspace's owners say how large one file may be and how much
//! one member's files may take, now and in any hour. A host's person says how
//! much the host keeps for each workspace it governs
//! ([`crate::KeptLimits::workspace_bytes`], the server's own setting).
//! Nothing here is a fixed ceiling: a limit changed once is obeyed by every
//! host of the workspace, including after a restart.
//!
//! Past a limit the poster is refused, plainly: the record still syncs between
//! members' devices, and only the host stops keeping the extra content.

use serde::{Deserialize, Serialize};

/// One mebibyte, the unit the limits are read and spoken in.
pub const MIB: u64 = 1024 * 1024;

/// The largest file a host keeps when the workspace's owners chose nothing:
/// 25 MiB.
pub const DEFAULT_FILE_BYTES: u64 = 25 * MIB;
/// What one member's files may take when the workspace's owners chose nothing:
/// 250 MiB.
pub const DEFAULT_MEMBER_BYTES: u64 = 250 * MIB;
/// What one member's files may take in any hour when the workspace's owners
/// chose nothing: 250 MiB.
pub const DEFAULT_HOURLY_BYTES: u64 = 250 * MIB;
/// What a host keeps for one workspace when its person chose nothing: 2 GiB.
pub const DEFAULT_WORKSPACE_BYTES: u64 = 2048 * MIB;

fn default_file_bytes() -> u64 {
    DEFAULT_FILE_BYTES
}

fn default_member_bytes() -> u64 {
    DEFAULT_MEMBER_BYTES
}

fn default_hourly_bytes() -> u64 {
    DEFAULT_HOURLY_BYTES
}

/// What a workspace lets its members' content take: its owners' setting, read
/// by every host of the workspace and by the devices that post there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPolicy {
    /// The largest file the host keeps; a larger one stays with its poster.
    #[serde(default = "default_file_bytes")]
    pub file_bytes: u64,
    /// What one member's files may take together.
    #[serde(default = "default_member_bytes")]
    pub member_bytes: u64,
    /// What one member's files may take in any hour.
    #[serde(default = "default_hourly_bytes")]
    pub hourly_bytes: u64,
}

impl Default for ContentPolicy {
    fn default() -> Self {
        Self {
            file_bytes: DEFAULT_FILE_BYTES,
            member_bytes: DEFAULT_MEMBER_BYTES,
            hourly_bytes: DEFAULT_HOURLY_BYTES,
        }
    }
}

/// Every limit a host keeps a workspace's content within: the workspace's
/// owners' [`ContentPolicy`] and its own person's budget for the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptLimits {
    /// The largest file kept.
    pub file_bytes: u64,
    /// What one member's files may take together.
    pub member_bytes: u64,
    /// What one member's files may take in any hour.
    pub hourly_bytes: u64,
    /// What the host keeps for the workspace together.
    pub workspace_bytes: u64,
}

impl Default for KeptLimits {
    fn default() -> Self {
        Self::of(&ContentPolicy::default(), DEFAULT_WORKSPACE_BYTES)
    }
}

impl KeptLimits {
    /// The limits `policy` sets, within what a host keeps for one workspace
    /// (`workspace_bytes`).
    #[must_use]
    pub fn of(policy: &ContentPolicy, workspace_bytes: u64) -> Self {
        Self {
            file_bytes: policy.file_bytes,
            member_bytes: policy.member_bytes,
            hourly_bytes: policy.hourly_bytes,
            workspace_bytes,
        }
    }

    /// How many bytes `limit` allows.
    #[must_use]
    pub fn allowed(&self, limit: ContentLimit) -> u64 {
        match limit {
            ContentLimit::File => self.file_bytes,
            ContentLimit::Member => self.member_bytes,
            ContentLimit::Hourly => self.hourly_bytes,
            ContentLimit::Workspace => self.workspace_bytes,
        }
    }

    /// Whether content of `bytes` stays within `limit`.
    #[must_use]
    pub fn allows(&self, limit: ContentLimit, bytes: u64) -> bool {
        bytes <= self.allowed(limit)
    }
}

/// The limit content went over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentLimit {
    /// One file larger than a file may be.
    File,
    /// One member's files together larger than their share.
    Member,
    /// One member's files taken in within the hour larger than their intake.
    Hourly,
    /// The workspace's files together larger than the host keeps for it.
    Workspace,
}

impl ContentLimit {
    /// What the limit is called, as a person reads it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::File => "file size",
            Self::Member => "a member's share",
            Self::Hourly => "a member's hourly intake",
            Self::Workspace => "the workspace's kept content",
        }
    }

    /// Who can raise the limit: the workspace's owners for what members post
    /// there, the host's person for what the host keeps.
    #[must_use]
    pub const fn raise(self) -> &'static str {
        match self {
            Self::File | Self::Member | Self::Hourly => {
                "the workspace's owners, in its content policy"
            }
            Self::Workspace => "the host's person, in its server's settings",
        }
    }

    /// The refusal a person sees: what the limit is, what it allowed, and who
    /// can raise it.
    #[must_use]
    pub fn refusal(self, allowed: u64, bytes: u64) -> String {
        format!(
            "{bytes} bytes is over the {name} of {allowed} bytes: {raise}",
            name = self.name(),
            raise = self.raise(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_limits_a_workspace_starts_with() {
        let policy = ContentPolicy::default();
        assert_eq!(policy.file_bytes, 25 * MIB);
        assert_eq!(policy.member_bytes, 250 * MIB);
        assert_eq!(policy.hourly_bytes, 250 * MIB);
        let limits = KeptLimits::default();
        assert_eq!(limits.workspace_bytes, 2048 * MIB);
        assert_eq!(limits.file_bytes, policy.file_bytes);
    }

    #[test]
    fn a_policy_written_before_a_limit_existed_takes_the_default_for_it() {
        let stored: ContentPolicy = serde_json::from_str(r#"{"file_bytes": 1024}"#).unwrap();
        assert_eq!(stored.file_bytes, 1024);
        assert_eq!(stored.member_bytes, DEFAULT_MEMBER_BYTES);
        assert_eq!(stored.hourly_bytes, DEFAULT_HOURLY_BYTES);
    }

    #[test]
    fn a_refusal_names_the_limit_what_it_allowed_and_who_raises_it() {
        let refusal = ContentLimit::File.refusal(1024, 2048);
        assert!(refusal.contains("file size"), "{refusal}");
        assert!(refusal.contains("1024"), "{refusal}");
        assert!(refusal.contains("2048"), "{refusal}");
        assert!(refusal.contains("workspace's owners"), "{refusal}");
        assert!(
            ContentLimit::Workspace
                .refusal(1, 2)
                .contains("host's person")
        );
    }
}
