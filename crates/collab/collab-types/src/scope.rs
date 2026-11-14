//! What a live track is sealed and named under.
//!
//! Every live track belongs to one scope: the workspace itself or one of its
//! records. The workspace's content keys seal every track; the scope names
//! the id the track's name carries.

/// The app whose record the live plane takes its workspace's keys through.
pub const MEDIA_APP: &str = "so.tilo.media";

/// That record.
pub const WORKSPACE_RECORD: &str = "live";

/// One scope of a workspace's live plane.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The workspace itself: its layout, presence and activity.
    Workspace { workspace: String },
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
            Self::Workspace { workspace } | Self::Record { workspace, .. } => workspace,
        }
    }

    /// The namespace segment that carries the scope: `workspace` or
    /// `record/<record id>`.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Workspace { .. } => "workspace".to_owned(),
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
    fn scopes_name_their_tracks_apart() {
        let workspace = Scope::workspace(WORKSPACE);
        let record = Scope::record(WORKSPACE, "notes");
        assert_eq!(workspace.id(), "workspace");
        assert_eq!(record.id(), "record/notes");
        assert_eq!(record.workspace_id(), WORKSPACE);
    }
}
