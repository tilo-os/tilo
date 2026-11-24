//! What a workspace lets its members' content take, and what a host keeps of
//! it: the limits a workspace's owners set on what members post there.
//!
//! A record names its content; a host keeps it so a member that comes up later
//! reads it from the host alone (a member's files are the workspace's storage,
//! and a person's own folder reaches their own server whole). The bounds are
//! policy, and only policy: the workspace's owners may say how large one file
//! may be and how much one member's files may take, now and in any hour, and a
//! host's person may say how much the host keeps for each workspace it governs
//! ([`crate::KeptLimits::workspace_bytes`], the server's own setting). A limit
//! nobody set is no limit at all: nothing here is a compiled-in ceiling, and a
//! limit set once is obeyed by every host of the workspace, including after a
//! restart.
//!
//! Past a limit the poster is refused, plainly: the record still syncs between
//! members' devices, and only the host stops keeping the extra content.

use serde::{Deserialize, Serialize};

/// One mebibyte, the unit the limits are read and spoken in.
pub const MIB: u64 = 1024 * 1024;

/// What a workspace lets its members' content take: its owners' setting, read
/// by every host of the workspace and by the devices that post there. A limit
/// left unset is no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentPolicy {
    /// The largest file the host keeps; a larger one stays with its poster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_bytes: Option<u64>,
    /// What one member's files may take together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_bytes: Option<u64>,
    /// What one member's files may take in any hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hourly_bytes: Option<u64>,
}

/// Every limit a host keeps a workspace's content within: the workspace's
/// owners' [`ContentPolicy`] and its own person's budget for the workspace.
/// None of them is a fixed ceiling: a limit nobody set allows any size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeptLimits {
    /// The largest file kept.
    pub file_bytes: Option<u64>,
    /// What one member's files may take together.
    pub member_bytes: Option<u64>,
    /// What one member's files may take in any hour.
    pub hourly_bytes: Option<u64>,
    /// What the host keeps for the workspace together.
    pub workspace_bytes: Option<u64>,
}

impl KeptLimits {
    /// The limits `policy` sets, within what a host keeps for one workspace
    /// (`workspace_bytes`).
    #[must_use]
    pub fn of(policy: &ContentPolicy, workspace_bytes: Option<u64>) -> Self {
        Self {
            file_bytes: policy.file_bytes,
            member_bytes: policy.member_bytes,
            hourly_bytes: policy.hourly_bytes,
            workspace_bytes,
        }
    }

    /// How many bytes `limit` allows; `None` when nobody set it.
    #[must_use]
    pub fn allowed(&self, limit: ContentLimit) -> Option<u64> {
        match limit {
            ContentLimit::File => self.file_bytes,
            ContentLimit::Member => self.member_bytes,
            ContentLimit::Hourly => self.hourly_bytes,
            ContentLimit::Workspace => self.workspace_bytes,
        }
    }

    /// Whether content of `bytes` stays within `limit`, a limit nobody set
    /// allowing any size.
    #[must_use]
    pub fn allows(&self, limit: ContentLimit, bytes: u64) -> bool {
        self.allowed(limit)
            .is_none_or(|allowed| bytes <= allowed)
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
    fn no_limit_is_set_by_default_and_none_of_them_bounds_any_size() {
        // A workspace whose owners set nothing, on a host whose person set no
        // budget: the workspace's storage is the disk's, not a ceiling's.
        let limits = KeptLimits::default();
        assert_eq!(limits.file_bytes, None);
        assert_eq!(limits.member_bytes, None);
        assert_eq!(limits.hourly_bytes, None);
        assert_eq!(limits.workspace_bytes, None);
        for limit in [
            ContentLimit::File,
            ContentLimit::Member,
            ContentLimit::Hourly,
            ContentLimit::Workspace,
        ] {
            assert_eq!(limits.allowed(limit), None, "{limit:?}");
            assert!(limits.allows(limit, 3 * 1024 * MIB), "{limit:?}");
        }
    }

    #[test]
    fn a_policy_written_before_a_limit_was_optional_takes_no_limit_for_the_ones_it_left_out() {
        let stored: ContentPolicy = serde_json::from_str(r#"{"file_bytes": 1024}"#).unwrap();
        assert_eq!(stored.file_bytes, Some(1024));
        assert_eq!(stored.member_bytes, None);
        assert_eq!(stored.hourly_bytes, None);
    }

    #[test]
    fn a_limit_the_owners_set_still_refuses_content_past_it() {
        let limits = KeptLimits::of(
            &ContentPolicy {
                file_bytes: Some(1024),
                ..ContentPolicy::default()
            },
            Some(2048),
        );
        assert!(!limits.allows(ContentLimit::File, 2048));
        assert!(limits.allows(ContentLimit::File, 1024));
        assert!(limits.allows(ContentLimit::Member, u64::MAX / 2));
        assert!(!limits.allows(ContentLimit::Workspace, 2049));
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
