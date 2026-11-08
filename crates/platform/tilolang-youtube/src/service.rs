//! The service the YouTube origin asks: what a search, a channel, the trending list and a
//! video's stream are, in the client's own terms.
//!
//! The origin ([`crate::origin`]) holds no protocol: it hands a [`Service`] a question and
//! writes the answer into the package's records. Two are provided: [`Live`] asks YouTube
//! through the platform's InnerTube client, in the connector process the system runs for the
//! app (which holds the network), and a test or a development session can hand its own instead.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A channel, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelItem {
    pub key: String,
    pub name: String,
    pub handle: String,
    pub subscribers: i64,
    pub avatar: String,
    pub banner: String,
    pub description: String,
    pub verified: bool,
}

/// A video, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoItem {
    pub key: String,
    pub title: String,
    pub channel_key: String,
    pub channel_name: String,
    pub channel_avatar: String,
    pub views: i64,
    /// When it was published, as the runtime records a `Time`.
    pub published: String,
    /// How long it runs, in seconds.
    pub duration: i64,
    pub description: String,
    pub likes: i64,
    pub thumbnail: String,
    pub live: bool,
}

/// A playlist, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub key: String,
    pub title: String,
    pub channel_key: String,
    pub channel_name: String,
    pub video_count: i64,
    pub thumbnail: String,
}

/// What a search found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    /// The query, as it was asked: what its videos and channels were found for.
    pub query: String,
    pub channels: Vec<ChannelItem>,
    pub playlists: Vec<PlaylistItem>,
    pub videos: Vec<VideoItem>,
}

/// A channel's page and its newest videos.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPage {
    pub channel: ChannelItem,
    pub videos: Vec<VideoItem>,
}

/// The videos the service lists as trending.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrendingPage {
    pub videos: Vec<VideoItem>,
}

/// Why a question could not be answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

/// What the origin asks of a service. Never waits on the network itself: the service does.
pub trait Service: Send + Sync {
    /// What YouTube finds for `query`.
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError>;

    /// A channel's page and its newest videos.
    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError>;

    /// The videos YouTube lists as trending.
    fn trending(&self) -> Result<TrendingPage, ServiceError>;

    /// The address a video's bytes play from.
    fn stream(&self, key: &str) -> Result<String, ServiceError>;
}

/// The service over YouTube's own client (`tilo-innertube`, which stands on rustypipe).
pub struct Live;

impl Live {
    /// The service over the one client of the process, which is built the first time it is
    /// asked, inside the app's runtime: a window that shows no YouTube resource never builds
    /// it, and one that does starts no connection until its first search.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for Live {
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::search(&client, query)
    }

    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::channel(&client, key)
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::trending(&client)
    }

    fn stream(&self, key: &str) -> Result<String, ServiceError> {
        let client = crate::live::client()?;
        crate::live::stream(&client, key)
    }
}

/// A service that answers from a script, for a test or a development session: the same answers
/// every time, in the order they were written.
#[derive(Default)]
pub struct Scripted {
    /// The searches, by the query asked.
    pub searches: std::collections::BTreeMap<String, SearchPage>,
    /// The channels, by key.
    pub channels: std::collections::BTreeMap<String, ChannelPage>,
    pub trending: TrendingPage,
    /// The streams, by video key.
    pub streams: std::collections::BTreeMap<String, String>,
}

impl Service for Scripted {
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError> {
        self.searches
            .get(query)
            .cloned()
            .ok_or_else(|| ServiceError(format!("nothing scripted for {query:?}")))
    }

    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError> {
        self.channels
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted channel {key:?}")))
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        Ok(self.trending.clone())
    }

    fn stream(&self, key: &str) -> Result<String, ServiceError> {
        self.streams
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted stream for {key:?}")))
    }
}

/// A scripted service, ready to share with an origin.
#[must_use]
pub fn scripted(scripted: Scripted) -> Arc<dyn Service> {
    Arc::new(scripted)
}
