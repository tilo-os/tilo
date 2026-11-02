//! What a live track is sealed and named under.
//!
//! Every live track belongs to one scope: the workspace itself, one of its
//! channels, or one of its records. The scope names the channel whose content
//! keys seal the scope's tracks and the id the track's name carries, so a call
//! in a private channel is sealed under that channel's keys and is named in
//! that channel's scope alone. A device outside the channel holds neither the
//! keys nor a name in it.

use crate::{ChannelId, ChannelRoute};

/// The app whose records carry the live plane's own scopes.
pub const MEDIA_APP: &str = "so.tilo.media";

/// The record of a workspace-scope live plane.
pub const WORKSPACE_RECORD: &str = "live";

/// One scope of a workspace's live plane.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The workspace itself: its layout, presence and activity.
    Workspace { workspace: String },
    /// One channel of the workspace, named by the channel id that seals it.
    Channel {
        workspace: String,
        channel: ChannelId,
    },
    /// One record of the workspace: presence on a record belongs to it.
    Record { workspace: String, record: String },
}

impl Scope {
    /// The workspace itself.
    #[must_use]
    pub fn workspace(workspace: impl Into<String>) -> Self {
        Self::Workspace {
            workspace: workspace.into(),
        }
    }

    /// One existing channel of `workspace`, named by its id: the id is
    /// derived from the channel's route, so the two can never disagree.
    #[must_use]
    pub fn channel(workspace: impl Into<String>, channel: ChannelId) -> Self {
        Self::Channel {
            workspace: workspace.into(),
            channel,
        }
    }

    /// The scope of one record.
    #[must_use]
    pub fn record(workspace: impl Into<String>, record: impl Into<String>) -> Self {
        Self::Record {
            workspace: workspace.into(),
            record: record.into(),
        }
    }

    /// The workspace this scope belongs to.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        match self {
            Self::Workspace { workspace }
            | Self::Channel { workspace, .. }
            | Self::Record { workspace, .. } => workspace,
        }
    }

    /// The channel whose content keys seal this scope's tracks.
    #[must_use]
    pub fn channel_id(&self) -> ChannelId {
        match self {
            Self::Workspace { workspace } => {
                ChannelRoute::new(MEDIA_APP, WORKSPACE_RECORD).channel(workspace)
            }
            Self::Channel { channel, .. } => *channel,
            Self::Record { workspace, record } => {
                ChannelRoute::new(MEDIA_APP, record).channel(workspace)
            }
        }
    }

    /// The namespace segment that carries the scope: `workspace`,
    /// `channel/<uuid>` or `record/<record id>`.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Workspace { .. } => "workspace".to_owned(),
            Self::Channel { channel, .. } => format!("channel/{}", channel.0),
            Self::Record { record, .. } => format!("record/{record}"),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(&self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "w1";

    #[test]
    fn a_workspace_scope_keys_under_the_live_record() {
        let scope = Scope::workspace(WORKSPACE);
        assert_eq!(
            scope.channel_id(),
            ChannelRoute::new(MEDIA_APP, "live").channel(WORKSPACE)
        );
        assert_eq!(scope.id(), "workspace");
    }

    #[test]
    fn a_channel_scope_keys_under_that_channel() {
        let route = ChannelRoute::new("so.tilo.sheets", "payroll");
        let scope = Scope::channel(WORKSPACE, route.channel(WORKSPACE));
        assert_eq!(scope.channel_id(), route.channel(WORKSPACE));
        assert_eq!(
            scope.id(),
            format!("channel/{}", route.channel(WORKSPACE).0)
        );
    }

    #[test]
    fn a_record_scope_keys_under_its_own_record() {
        let scope = Scope::record(WORKSPACE, "notes");
        assert_eq!(scope.workspace_id(), WORKSPACE);
        assert_eq!(
            scope.channel_id(),
            ChannelRoute::new(MEDIA_APP, "notes").channel(WORKSPACE)
        );
        assert_eq!(scope.id(), "record/notes");
    }

    #[test]
    fn scopes_of_different_members_never_share_a_channel() {
        let workspace = Scope::workspace(WORKSPACE);
        let channel = Scope::channel(
            WORKSPACE,
            ChannelRoute::new("so.tilo.sheets", "payroll").channel(WORKSPACE),
        );
        let record = Scope::record(WORKSPACE, "live");
        let ids = [
            workspace.channel_id(),
            channel.channel_id(),
            record.channel_id(),
        ];
        assert_eq!(ids[0], ids[2], "record/live is the workspace's own record");
        assert_ne!(ids[0], ids[1]);
        assert_ne!(
            workspace.id(),
            record.id(),
            "the two scopes keep their names"
        );
    }
}
