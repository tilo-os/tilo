//! Data-source abstraction for Tube. Live data comes from real `YouTube` via the pure-Rust
//! [`rustypipe`](https://docs.rs/rustypipe) `InnerTube` client (`--features live`, in the default
//! build) behind the [`YtBackend`] trait.

#[cfg(feature = "live")]
pub mod live;

use crate::model::{Channel, Comment, Playlist, Video};

pub fn yt_thumb_url(video_id: &str) -> String {
    if video_id.is_empty() {
        String::new()
    } else {
        format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    Search(String),
    Video(String),
    Channel(String),
    Related(String),
    Trending,
}

#[derive(Clone, Debug)]
pub enum BackendResult {
    Videos(Vec<Video>),
    Video(Box<Video>),
    Channel {
        channel: Box<Channel>,
        uploads: Vec<Video>,
    },
}

#[derive(Clone, Debug)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BackendError {}

pub type BackendOutcome = Result<BackendResult, BackendError>;

#[derive(Clone, Debug)]
pub struct PlaybackSource {
    pub url: String,
    pub has_audio: bool,
    pub quality: String,
}

pub trait YtBackend: Send + Sync {
    fn name(&self) -> &str;

    fn search(&self, query: &str) -> BackendOutcome;

    fn video(&self, id: &str) -> BackendOutcome;

    fn channel(&self, id: &str) -> BackendOutcome;

    fn related(&self, id: &str) -> BackendOutcome;

    fn trending(&self) -> BackendOutcome;

    fn playlist(&self, _id: &str) -> Result<(Playlist, Vec<Video>), BackendError> {
        Err(BackendError("this backend cannot resolve playlists".into()))
    }

    fn comments(&self, _id: &str) -> Result<Vec<Comment>, BackendError> {
        Ok(Vec::new())
    }

    fn popular_search(&self, query: &str) -> BackendOutcome {
        self.search(query)
    }

    fn resolve_playback_source(
        &self,
        _id: &str,
        _max_height: Option<u32>,
    ) -> Result<PlaybackSource, BackendError> {
        Err(BackendError(
            "this backend cannot resolve a playback source".into(),
        ))
    }

    fn run(&self, query: Query) -> BackendOutcome {
        match query {
            Query::Search(q) => self.search(&q),
            Query::Video(id) => self.video(&id),
            Query::Channel(id) => self.channel(&id),
            Query::Related(id) => self.related(&id),
            Query::Trending => self.trending(),
        }
    }
}
