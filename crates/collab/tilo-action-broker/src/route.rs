/// Sentinel cluster id: `tilo_desktop_model::RoomId::nil` rendered as
/// a string. A workspace announces this when its durable room identity
/// has not been minted yet; never delivered to or from — see
/// [`CrdtRoute::is_addressable`].
const UNMINTED_CLUSTER_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The address a CRDT (or chrome) event is delivered to, built from the
/// `(app_id, cluster_id)` pair a connection announces on `Hello`. App
/// scope (both fields set) carries CRDT payloads, so one app's deltas
/// never reach another; cluster scope (`app_id` empty) carries chrome
/// traffic, where every app in the cluster is a legitimate audience, and
/// the Cap'n Proto transport, whose `joinChannel` surface has a channel
/// id but no app identity.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CrdtRoute {
    /// App identity from the sender's `Hello`. Empty means "this
    /// transport does not announce an app identity" — matched at
    /// cluster granularity.
    pub app_id: String,
    /// Collaboration cluster id, carried in the typed `channelId`.
    pub cluster_id: String,
    /// The record the sender announced, or empty for an app whose state
    /// is still one document per `(app, cluster)`. Finest granularity —
    /// two people editing different files in one room share app and
    /// cluster, and only the record tells them apart. Routing key only.
    pub record_id: String,
}

impl CrdtRoute {
    /// App-scoped route — the addressing a CRDT payload from an app
    /// that has not adopted records uses.
    pub fn app(app_id: impl Into<String>, cluster_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id: cluster_id.into(),
            record_id: String::new(),
        }
    }

    /// Cluster-scoped route — chrome traffic and the capnp transport.
    pub fn cluster(cluster_id: impl Into<String>) -> Self {
        Self {
            app_id: String::new(),
            cluster_id: cluster_id.into(),
            record_id: String::new(),
        }
    }

    /// Record-scoped route — one document's change stream.
    pub fn record(
        app_id: impl Into<String>,
        cluster_id: impl Into<String>,
        record_id: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id: cluster_id.into(),
            record_id: record_id.into(),
        }
    }

    /// Narrow this route to a record; an empty `record_id` leaves it as
    /// it was (pre-record addressing).
    #[must_use]
    pub fn on_record(mut self, record_id: impl Into<String>) -> Self {
        self.record_id = record_id.into();
        self
    }

    /// Widen an app-scoped route to its cluster — presence rides this.
    /// "Who else is here" is a fact about the room, so a peer editing a
    /// different document is still a legitimate audience for it.
    #[must_use]
    pub fn to_cluster_scope(&self) -> Self {
        Self::cluster(self.cluster_id.clone())
    }

    /// The channel this route names, as one string — the capnp
    /// transport subscribes by channel id. Encoding lives in
    /// [`runtime_collab::route::channel_key`] (shared with the SDK) so both
    /// ends can't drift.
    pub fn channel_key(&self) -> String {
        runtime_collab::route::channel_key::format(&self.app_id, &self.cluster_id, &self.record_id)
    }

    /// Split a capnp `channelId` back into its route — the inverse of
    /// [`Self::channel_key`].
    pub fn from_channel_key(key: &str) -> Self {
        let (app_id, cluster_id, record_id) = runtime_collab::route::channel_key::parse(key);
        Self {
            app_id,
            cluster_id,
            record_id,
        }
    }

    /// Whether a cluster id names a real room: not the empty string
    /// (never said `Hello`) and not the nil uuid (a workspace whose room
    /// identity has not been minted yet). The nil case must be checked
    /// separately from empty — otherwise every workspace still carrying
    /// the sentinel would share one enormous room on every machine.
    fn is_addressable(cluster_id: &str) -> bool {
        !cluster_id.is_empty() && cluster_id != UNMINTED_CLUSTER_ID
    }

    /// Whether this route may be stamped onto a frame leaving the
    /// machine. Same test as [`Self::delivers_to`], because the
    /// receiving broker rebuilds the route from the wire fields and runs
    /// the identical check — publishing an unaddressable route would put
    /// a frame in the room every receiver silently drops.
    pub fn is_publishable(&self) -> bool {
        Self::is_addressable(&self.cluster_id)
    }

    /// Whether an event addressed to `self` reaches a subscriber that
    /// announced `subscriber`. Fails closed if either side lacks an
    /// addressable cluster. App granularity applies only when both ends
    /// announced an app id; an end with none stays cluster-scoped rather
    /// than silently receiving nothing.
    ///
    /// Record granularity is innermost and fails closed both ways — "no
    /// record" is a value like any other, so a converted app's deltas
    /// never reach an unconverted connection of the same app.
    pub fn delivers_to(&self, subscriber: &CrdtRoute) -> bool {
        if !Self::is_addressable(&self.cluster_id) || !Self::is_addressable(&subscriber.cluster_id)
        {
            return false;
        }
        if self.cluster_id != subscriber.cluster_id {
            return false;
        }
        if self.app_id.is_empty() || subscriber.app_id.is_empty() {
            return true; // Cluster-scoped on at least one end.
        }
        if self.app_id != subscriber.app_id {
            return false;
        }
        self.record_id == subscriber.record_id
    }
}
