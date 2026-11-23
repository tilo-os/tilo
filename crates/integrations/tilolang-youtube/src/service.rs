//! The service the YouTube origin asks: what a search, a channel, a playlist, the trending list,
//! a video's stream, its comments and its community marks are, in the client's own terms.
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

/// What a search narrows its answer to: the kinds it shows, how long its videos run and how
/// recent they are, as YouTube's own search filters take them. An empty or `any` part is no
/// filter, which is what a search without one asks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchFilter {
    /// `all`, `video`, `channel` or `playlist`.
    #[serde(default)]
    pub kind: String,
    /// `short` (under four minutes), `medium` (four to twenty) or `long` (over twenty).
    #[serde(default)]
    pub length: String,
    /// `hour`, `today`, `week`, `month` or `year`.
    #[serde(default)]
    pub published: String,
}

impl SearchFilter {
    /// Whether it narrows nothing: every part is absent or `any`, so a search asks YouTube
    /// what it would without one.
    #[must_use]
    pub fn is_any(&self) -> bool {
        let any = |part: &str| part.is_empty() || part == "any" || part == "all";
        any(&self.kind) && any(&self.length) && any(&self.published)
    }
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

/// A playlist's page and the videos it holds, in the order its maker put them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistPage {
    pub playlist: PlaylistItem,
    pub videos: Vec<VideoItem>,
}

/// A comment someone left on a video, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentItem {
    pub key: String,
    pub video_key: String,
    pub author: String,
    pub avatar: String,
    pub text: String,
    pub likes: i64,
    /// When it was written, as the runtime records a `Time`.
    pub published: String,
    pub pinned: bool,
    pub hearted: bool,
    /// How many replies it has.
    pub replies: i64,
}

/// A video's comments, most liked first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentsPage {
    pub video: String,
    pub comments: Vec<CommentItem>,
}

/// One `SponsorBlock` segment of a video, in seconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SegmentItem {
    pub video_key: String,
    /// The segment's kind, as `SponsorBlock` names it: `sponsor`, `selfpromo`, `interaction`,
    /// `intro`, `outro`, `preview`, `music_offtopic` or `filler`.
    pub category: String,
    pub start: f64,
    pub end: f64,
    /// The community votes that place it.
    pub votes: i64,
}

/// The `SponsorBlock` segments of a video, in the order they play.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SegmentPage {
    pub video: String,
    pub segments: Vec<SegmentItem>,
}

/// What `DeArrow`'s community calls a video, when it names it better than its maker did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branding {
    pub video: String,
    /// The community's title, empty when it has none.
    pub title: String,
    /// The picture the community frames it with, as this device's content address.
    pub thumbnail: String,
}

/// What Return YouTube Dislike estimates people did with a video.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Votes {
    pub video: String,
    pub dislikes: i64,
    /// The likes the estimate carries, when it says any.
    pub likes: Option<i64>,
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
    /// What YouTube finds for `query`, narrowed by `filter`.
    fn search(&self, query: &str, filter: &SearchFilter) -> Result<SearchPage, ServiceError>;

    /// A channel's page and its newest videos.
    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError>;

    /// A playlist, and the videos it holds in the order its maker put them.
    fn playlist(&self, key: &str) -> Result<PlaylistPage, ServiceError>;

    /// The videos YouTube lists as trending.
    fn trending(&self) -> Result<TrendingPage, ServiceError>;

    /// The address a video's bytes play from, no taller than `quality` names (`auto` for the
    /// best that fits).
    fn stream(&self, key: &str, quality: &str) -> Result<String, ServiceError>;

    /// A video's comments, most liked first.
    fn comments(&self, key: &str) -> Result<CommentsPage, ServiceError>;

    /// The `SponsorBlock` segments of a video.
    fn segments(&self, key: &str) -> Result<SegmentPage, ServiceError>;

    /// `DeArrow`'s community title and picture of a video.
    fn branding(&self, key: &str) -> Result<Branding, ServiceError>;

    /// What Return YouTube Dislike estimates people did with a video.
    fn votes(&self, key: &str) -> Result<Votes, ServiceError>;
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

#[cfg(feature = "live")]
impl Service for Live {
    fn search(&self, query: &str, filter: &SearchFilter) -> Result<SearchPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::search(&client, query, filter)
    }

    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::channel(&client, key)
    }

    fn playlist(&self, key: &str) -> Result<PlaylistPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::playlist(&client, key)
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::trending(&client)
    }

    fn stream(&self, key: &str, quality: &str) -> Result<String, ServiceError> {
        let client = crate::live::client()?;
        crate::live::stream(&client, key, quality)
    }

    fn comments(&self, key: &str) -> Result<CommentsPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::comments(&client, key)
    }

    fn segments(&self, key: &str) -> Result<SegmentPage, ServiceError> {
        let client = crate::live::client()?;
        crate::live::segments(&client, key)
    }

    fn branding(&self, key: &str) -> Result<Branding, ServiceError> {
        let client = crate::live::client()?;
        crate::live::branding(&client, key)
    }

    fn votes(&self, key: &str) -> Result<Votes, ServiceError> {
        let client = crate::live::client()?;
        crate::live::votes(&client, key)
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
    /// The playlists, by key.
    pub playlists: std::collections::BTreeMap<String, PlaylistPage>,
    pub trending: TrendingPage,
    /// The streams, by video key.
    pub streams: std::collections::BTreeMap<String, String>,
    /// The comments, by video key.
    pub comments: std::collections::BTreeMap<String, CommentsPage>,
    /// The segments, by video key.
    pub segments: std::collections::BTreeMap<String, SegmentPage>,
    /// The community branding, by video key.
    pub branding: std::collections::BTreeMap<String, Branding>,
    /// The community votes, by video key.
    pub votes: std::collections::BTreeMap<String, Votes>,
}

impl Service for Scripted {
    fn search(&self, query: &str, _filter: &SearchFilter) -> Result<SearchPage, ServiceError> {
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

    fn playlist(&self, key: &str) -> Result<PlaylistPage, ServiceError> {
        self.playlists
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted playlist {key:?}")))
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        Ok(self.trending.clone())
    }

    fn stream(&self, key: &str, _quality: &str) -> Result<String, ServiceError> {
        self.streams
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted stream for {key:?}")))
    }

    fn comments(&self, key: &str) -> Result<CommentsPage, ServiceError> {
        self.comments
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted comments for {key:?}")))
    }

    fn segments(&self, key: &str) -> Result<SegmentPage, ServiceError> {
        self.segments
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted segments for {key:?}")))
    }

    fn branding(&self, key: &str) -> Result<Branding, ServiceError> {
        self.branding
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted branding for {key:?}")))
    }

    fn votes(&self, key: &str) -> Result<Votes, ServiceError> {
        self.votes
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted votes for {key:?}")))
    }
}

/// A scripted service, ready to share with an origin.
#[must_use]
pub fn scripted(scripted: Scripted) -> Arc<dyn Service> {
    Arc::new(scripted)
}
