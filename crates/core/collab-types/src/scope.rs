//! What a live track is sealed and named under.
//!
//! Every live track belongs to one scope: the workspace itself or one of its
//! records. The workspace's content keys seal every track; the scope names
//! the id the track's name carries.

/// The app whose record the live session takes its workspace's keys through.
pub const MEDIA_APP: &str = "so.tilo.media";

/// That record.
pub const WORKSPACE_RESOURCE: &str = "live";

/// One scope of a workspace's live session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The workspace itself: its layout, presence and activity.
    Workspace { workspace: String },
    /// One resource of the workspace: presence on it belongs to it.
    Resource { workspace: String, resource: String },
}

impl Scope {
    /// The workspace itself.
    #[must_use]
    pub fn workspace(workspace: impl Into<String>) -> Self {
        Self::Workspace {
            workspace: workspace.into(),
        }
    }

    /// The scope of one resource.
    #[must_use]
    pub fn resource(workspace: impl Into<String>, resource: impl Into<String>) -> Self {
        Self::Resource {
            workspace: workspace.into(),
            resource: resource.into(),
        }
    }

    /// The workspace this scope belongs to.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        match self {
            Self::Workspace { workspace } | Self::Resource { workspace, .. } => workspace,
        }
    }

    /// The namespace segment that carries the scope: `workspace` or
    /// `resource/<resource id>`.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Workspace { .. } => "workspace".to_owned(),
            Self::Resource { resource, .. } => format!("resource/{resource}"),
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
        let resource = Scope::resource(WORKSPACE, "notes");
        assert_eq!(workspace.id(), "workspace");
        assert_eq!(resource.id(), "resource/notes");
        assert_eq!(resource.workspace_id(), WORKSPACE);
    }
}
