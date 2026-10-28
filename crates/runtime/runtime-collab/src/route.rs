//! How an application's collaboration traffic is addressed: the typed
//! route an endpoint joins, the single channel id that route travels under
//! on the broker, and what the endpoint can do with the route's content.

use uuid::Uuid;

/// The routing key one connection names on the capnp transport.
///
/// An app's traffic is addressed by `(app_id, cluster_id, record_id)`,
/// but capnp's `joinChannel(channelId, identity)` carries a single
/// string, so the triple travels *inside* the channel id. This pair of
/// functions is the only place the encoding is written down, so the two
/// halves cannot drift apart.
///
/// # Shape
///
/// ```text
/// <cluster>[#app:<app_id>][#record:<record_id>]
/// ```
///
/// A uuid contains no `#`. `#record:` is stripped first and takes
/// everything after it, so a `#`-containing record id (a file path may
/// have one) stays intact. Both suffixes are optional, so a missing
/// `#app:` yields an empty app id rather than failing to parse — needed
/// for the earlier two-field keys chrome traffic still rides.
///
/// Without the app id in this key, every packaged app would publish
/// deltas cluster-scoped — one app's changes delivered to every app in
/// the room.
pub mod channel_key {
    /// Encode a route triple into one channel id.
    pub fn format(app_id: &str, cluster_id: &str, record_id: &str) -> String {
        let mut key = String::from(cluster_id);
        if !app_id.is_empty() {
            key.push_str("#app:");
            key.push_str(app_id);
        }
        if !record_id.is_empty() {
            key.push_str("#record:");
            key.push_str(record_id);
        }
        key
    }

    /// Decode a channel id into `(app_id, cluster_id, record_id)`. Total:
    /// every string decodes, and one naming no marker is a bare cluster.
    pub fn parse(key: &str) -> (String, String, String) {
        let (head, record_id) = match key.split_once("#record:") {
            Some((head, record)) => (head, record.to_string()),
            None => (key, String::new()),
        };
        let (cluster, app_id) = match head.split_once("#app:") {
            Some((cluster, app)) => (cluster, app.to_string()),
            None => (head, String::new()),
        };
        (app_id, cluster.to_string(), record_id)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn channel_keys_round_trip_every_field_combination_and_legacy_form() {
            // Case `round_trips_every_combination_of_present_fields`.
            {
                for (app, cluster, record) in [
                    ("", "c", ""),
                    ("app", "c", ""),
                    ("", "c", "rec"),
                    ("app", "c", "rec"),
                ] {
                    let key = format(app, cluster, record);
                    assert_eq!(
                        parse(&key),
                        (app.to_string(), cluster.to_string(), record.to_string()),
                        "key {key:?} did not round-trip",
                    );
                }
            }

            // Case `keys_written_before_the_app_segment_still_parse`.
            {
                // Both must decode to a cluster-scoped address rather than
                // nothing, or an app that hasn't been rebuilt stops routing.
                assert_eq!(
                    parse("cluster-1"),
                    (String::new(), "cluster-1".into(), String::new()),
                );
                assert_eq!(
                    parse("cluster-1#record:/notes/a.md"),
                    (String::new(), "cluster-1".into(), "/notes/a.md".into()),
                );
            }

            // Case `a_record_id_containing_a_hash_survives`.
            {
                // Record ids are file paths and a path may contain `#`.
                // Stripping `#record:` first and keeping the whole tail is
                // what makes that safe.
                let key = format("app", "c", "/notes/draft#2.md");
                assert_eq!(
                    parse(&key),
                    ("app".into(), "c".into(), "/notes/draft#2.md".into()),
                );
            }
        }
    }
}

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
    /// This device cannot join the channel right now: the room's governor
    /// is unknown here. Edits wait on the device.
    Unavailable,
    /// This device has no secret store open, so it cannot hold keys of a
    /// room hosted elsewhere. Edits wait on the device.
    NoSecretStore,
}

impl ChannelAccessState {
    /// Whether content currently flows.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Unencrypted | Self::Encrypted)
    }
}
