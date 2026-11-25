//! What a live track is sealed and named under.
//!
//! Every live track belongs to one scope: the workspace itself, one of its
//! records, or one device of the person whose own workspace it is. The
//! workspace's content keys seal every track; the scope names the id the
//! track's name carries.

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
    /// One device of the person whose own workspace this is, as the person's
    /// other devices reach it directly: what they send that device alone (their
    /// keyboard and pointer driving it), relayed by that device itself.
    Device { workspace: String, device: [u8; 32] },
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

    /// The device `device` of the person whose own workspace `workspace` is.
    #[must_use]
    pub fn device(workspace: impl Into<String>, device: [u8; 32]) -> Self {
        Self::Device {
            workspace: workspace.into(),
            device,
        }
    }

    /// The scope of `workspace` whose namespace segment is `id`, or `None` for
    /// a segment no scope carries.
    #[must_use]
    pub fn parse(workspace: impl Into<String>, id: &str) -> Option<Self> {
        if id == "workspace" {
            return Some(Self::workspace(workspace));
        }
        if let Some(resource) = id.strip_prefix("resource/") {
            return (!resource.is_empty()).then(|| Self::resource(workspace, resource));
        }
        let device = id.strip_prefix("device/")?;
        // Spelled one way only, so one device's scope has one name.
        if device.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return None;
        }
        let mut key = [0_u8; 32];
        hex::decode_to_slice(device, &mut key).ok()?;
        Some(Self::device(workspace, key))
    }

    /// The workspace this scope belongs to.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        match self {
            Self::Workspace { workspace }
            | Self::Resource { workspace, .. }
            | Self::Device { workspace, .. } => workspace,
        }
    }

    /// The device this scope is, for a scope of one device.
    #[must_use]
    pub const fn device_of(&self) -> Option<[u8; 32]> {
        match self {
            Self::Device { device, .. } => Some(*device),
            Self::Workspace { .. } | Self::Resource { .. } => None,
        }
    }

    /// The namespace segment that carries the scope: `workspace`,
    /// `resource/<resource id>` or `device/<device key, lowercase hex>`.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Workspace { .. } => "workspace".to_owned(),
            Self::Resource { resource, .. } => format!("resource/{resource}"),
            Self::Device { device, .. } => format!("device/{}", hex::encode(device)),
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
        let device = Scope::device(WORKSPACE, [0xab; 32]);
        assert_eq!(device.id(), format!("device/{}", "ab".repeat(32)));
        assert_ne!(device.id(), workspace.id());
        for scope in [workspace, resource, device] {
            assert_eq!(Scope::parse(WORKSPACE, &scope.id()), Some(scope.clone()));
        }
        assert_eq!(Scope::parse(WORKSPACE, "device/ab"), None);
        assert_eq!(
            Scope::parse(WORKSPACE, &format!("device/{}", "AB".repeat(32))),
            None
        );
    }
}
