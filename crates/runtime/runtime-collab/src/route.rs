//! How an application's collaboration traffic is addressed: the typed
//! route an endpoint joins, the single channel id that route travels under
//! on the broker, and what the endpoint can do with the route's content.

use uuid::Uuid;

/// The routing key one connection names on the collaboration wire, as its one home writes it.
pub use collab_types::channel_key;

/// An explicit typed collaboration route.
///
/// A manager has one primary route (announced by its initial `Hello`) and may
/// retain additional routes for discovery or cross-app work. Callers must name
/// an additional route again when publishing, which prevents a cluster-wide
/// discovery subscription from becoming an accidental mutation target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteSpec {
    pub app_id: String,
    pub cluster_id: Uuid,
    pub record: Option<String>,
}

impl RouteSpec {
    /// The single channel id this route is joined under on the broker.
    pub fn channel_id(&self) -> String {
        channel_key::format(
            &self.app_id,
            &self.cluster_id.to_string(),
            self.record.as_deref().unwrap_or(""),
        )
    }

    pub fn cluster(cluster_id: Uuid) -> Self {
        Self {
            app_id: String::new(),
            cluster_id,
            record: None,
        }
    }

    pub fn app(app_id: impl Into<String>, cluster_id: Uuid) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: None,
        }
    }

    pub fn record(app_id: impl Into<String>, cluster_id: Uuid, record: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: Some(record.into()),
        }
    }
}

/// What an endpoint's route can do with content, as apps see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelAccessState {
    /// No channel encryption applies to this endpoint (a test harness).
    Unencrypted,
    /// End-to-end encrypted; content flows.
    Encrypted,
    /// This device asked for access and has not been admitted yet, so no
    /// content can be read or written. Edits wait on the device.
    WaitingForAccess,
    /// This device was removed from the channel.
    Removed,
    /// This device cannot join the channel right now: the workspace's
    /// governor is unknown here. Edits wait on the device.
    Unavailable,
    /// This device has no secret store open, so it cannot hold keys of a
    /// workspace hosted elsewhere. Edits wait on the device.
    NoSecretStore,
}

impl ChannelAccessState {
    /// Whether content currently flows.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Unencrypted | Self::Encrypted)
    }
}
