//! Data-source abstraction for Tube.
//!
//! Tube has two worlds:
//!
//! * **offline** (the default build): the bundled [`crate::sample`] catalogue.
//!   The `--shots` renderer and every test run here — no network, fully
//!   hermetic, deterministic screenshots.
//! * **live** (`--features live`): real `YouTube` data via the pure-Rust
//!   [`rustypipe`](https://docs.rs/rustypipe) `InnerTube` client. This pulls
//!   in `tokio` + a network stack, so it is *off* by default and never
//!   compiled into the hermetic shots path.
//!
//! Both worlds implement the same [`YtBackend`] trait, so the rest of the app
//! (the fetch worker in [`crate::api`]) is written once against the trait and
//! the build feature picks the implementation: rustypipe is real but gated,
//! and the offline sample is the default.
//!
//! ## What lives where
//!
//! | Concern            | Type / module                       |
//! |--------------------|-------------------------------------|
//! | data-source trait  | [`YtBackend`]                       |
//! | offline impl       | [`offline::OfflineBackend`]         |
//! | live impl (gated)  | `live::RustyPipeBackend` (`live`)   |
//! | request/result DTO | [`Query`], [`BackendResult`]        |
//!
//! Recommendations model (Home): the trait exposes the no-auth signals
//! `NewPipe` / Piped use — [`YtBackend::trending`] and [`YtBackend::related`]
//! (per-video "up next"). See [`crate::recommend`].

pub mod offline;
/// Fully-rangeable stream resolution through public Invidious `local=true`
/// proxies — the reliable, seekable playback URL for the media engine.
pub mod proxy;

#[cfg(feature = "live")]
pub mod live;

/// yt-dlp subprocess extractor — the maintained YouTube data source. Gated
/// behind `live` alongside the rustypipe fallback.
#[cfg(feature = "live")]
pub mod ytdlp;

use crate::model::{Channel, Comment, Playlist, Video};

/// Canonical i.ytimg.com thumbnail URL for a video id. `hqdefault.jpg` is the
/// stable 480×360 still every public video serves, so a real card image is
/// available from just the id — no extra extraction round trip. Empty id in →
/// empty string out (the caller shows the seed gradient).
pub fn yt_thumb_url(video_id: &str) -> String {
    if video_id.is_empty() {
        String::new()
    } else {
        format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
    }
}

/// A read request against a [`YtBackend`]. Kept serde-free and toolkit-free so
/// it can cross the worker-thread boundary cheaply.
#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    /// Full-text search for videos.
    Search(String),
    /// Full metadata for one video (description, likes, comments, streams …).
    Video(String),
    /// A channel page + its uploads.
    Channel(String),
    /// Per-video "up next" / related videos.
    Related(String),
    /// Region trending feed.
    Trending,
}

/// The result of a [`Query`]. Backends map their native types onto Tube's
/// plain [`crate::model`] structs so the UI never sees a rustypipe type.
#[derive(Clone, Debug)]
pub enum BackendResult {
    Videos(Vec<Video>),
    Video(Box<Video>),
    Channel {
        channel: Box<Channel>,
        uploads: Vec<Video>,
    },
}

/// Error surfaced by a backend. Stringly-typed on purpose — the UI only ever
/// shows it in the status log, and the offline backend never errors.
#[derive(Clone, Debug)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BackendError {}

pub type BackendOutcome = Result<BackendResult, BackendError>;

/// A resolved, directly-playable media URL for the `GStreamer` decode path.
///
/// This whole playable URL is handed to the shared [`media::MediaEngine`],
/// which negotiates the container and codec and routes audio through the same
/// playback backend as the system video player and editor preview.
#[derive(Clone, Debug)]
pub struct PlaybackSource {
    /// Directly-playable media URL (progressive muxed when available, else the
    /// best video-only stream). Signature/nsig already applied by the extractor.
    pub url: String,
    /// True when the chosen URL carries audio as well as video (a progressive
    /// muxed format), so the caller knows sound will play.
    pub has_audio: bool,
    /// `YouTube` quality label, e.g. "360p".
    pub quality: String,
}

/// The data-source seam. One trait, two worlds (offline sample / live
/// rustypipe). Methods are intentionally coarse-grained — one per screen need
/// — so a backend can batch the underlying `InnerTube` calls however it likes.
///
/// Implementations must be `Send + Sync` because the live backend is driven
/// from the fetch worker thread.
pub trait YtBackend: Send + Sync {
    /// Human name for the status log / settings ("offline sample",
    /// "rustypipe").
    fn name(&self) -> &str;

    /// Full-text video search.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend cannot reach or parse the search results.
    fn search(&self, query: &str) -> BackendOutcome;

    /// Full metadata for a single video.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend cannot reach or parse the video.
    fn video(&self, id: &str) -> BackendOutcome;

    /// Channel page + uploads.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend cannot reach or parse the channel.
    fn channel(&self, id: &str) -> BackendOutcome;

    /// Per-video related / "up next".
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend cannot reach or parse the related list.
    fn related(&self, id: &str) -> BackendOutcome;

    /// Region trending.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend cannot reach or parse the trending feed.
    fn trending(&self) -> BackendOutcome;

    /// A playlist page: its metadata + ordered video list. The default errors —
    /// only a backend that can reach a real extractor produces one. The UI shows
    /// an "unavailable" state when this fails.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless overridden by a backend that can resolve playlists.
    fn playlist(&self, _id: &str) -> Result<(Playlist, Vec<Video>), BackendError> {
        Err(BackendError("this backend cannot resolve playlists".into()))
    }

    /// Top comments for a video. The default returns none — only a backend that
    /// can reach `YouTube`'s `next`/comments continuation (the live rustypipe one)
    /// produces real comments; the offline sample carries its own, and the
    /// yt-dlp path leaves comments to rustypipe (yt-dlp's own comment extraction
    /// is slow and unreliable). The UI degrades to "no comments" when empty.
    ///
    /// # Errors
    ///
    /// The default never errors (`Ok(Vec::new())`); an overriding backend may.
    fn comments(&self, _id: &str) -> Result<Vec<Comment>, BackendError> {
        Ok(Vec::new())
    }

    /// Popular recent videos matching `query`, used to compose a real "popular"
    /// home feed when `YouTube`'s dedicated trending browse is unavailable. The
    /// default is a plain [`search`](YtBackend::search); a live backend can
    /// override it with a view-count-sorted, recent-upload filter so the feed is
    /// genuinely "popular" rather than arbitrary.
    ///
    /// # Errors
    ///
    /// Returns `Err` under the same conditions as [`YtBackend::search`], which
    /// the default implementation delegates to.
    fn popular_search(&self, query: &str) -> BackendOutcome {
        self.search(query)
    }

    /// Resolve a whole directly-playable media URL for `id`, for the `GStreamer`
    /// decode path (VP9 / AV1 / H.264 + audio). The default errors — only a
    /// backend that can reach a real extractor (yt-dlp) produces one. The UI
    /// degrades to the poster when this fails.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless overridden by a backend that can resolve a
    /// playable source.
    fn resolve_playback_source(&self, _id: &str) -> Result<PlaybackSource, BackendError> {
        Err(BackendError(
            "this backend cannot resolve a playback source".into(),
        ))
    }

    /// Dispatch a [`Query`] to the right method. Lets the worker stay generic.
    ///
    /// # Errors
    ///
    /// Returns `Err` under the same conditions as the dispatched method.
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

/// The default backend for the current build: the offline sample unless the
/// `live` feature selects rustypipe at runtime. Constructed by the worker.
///
/// We return a boxed trait object so the worker code is identical across
/// features; only this one function differs.
pub fn default_backend() -> Box<dyn YtBackend> {
    Box::new(offline::OfflineBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_offline_backend_searches_and_dispatches_trending() {
        // Case `default_backend_is_offline_and_searches`.
        {
            let b = default_backend();
            let out = b.search("rust").expect("offline search never errors");
            match out {
                BackendResult::Videos(v) => assert!(!v.is_empty(), "sample has rust videos"),
                _ => panic!("search returns Videos"),
            }
        }
        // Case `run_dispatches_trending`.
        {
            let b = default_backend();
            assert!(matches!(
                b.run(Query::Trending),
                Ok(BackendResult::Videos(_))
            ));
        }
    }
}
