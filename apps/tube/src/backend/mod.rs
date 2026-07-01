//! Data-source abstraction for Tube.
//!
//! Tube has two worlds:
//!
//! * **offline** (the default build): the bundled [`crate::sample`] catalogue.
//!   The `--shots` renderer and every test run here — no network, fully
//!   hermetic, deterministic screenshots.
//! * **live** (`--features live`): real YouTube data via the pure-Rust
//!   [`rustypipe`](https://docs.rs/rustypipe) InnerTube client, with PoTokens
//!   minted by a swappable [`potoken`] provider (`rustypipe-botguard` by
//!   default). This pulls in `tokio` + a network stack, so it is *off* by
//!   default and never compiled into the hermetic shots path.
//!
//! Both worlds implement the same [`YtBackend`] trait, so the rest of the app
//! (the fetch worker in [`crate::api`]) is written once against the trait and
//! the build feature picks the implementation. This is the clean seam the
//! research brief asks for: rustypipe is real but gated, the offline sample is
//! the default, and PoToken is a module you can swap.
//!
//! ## What lives where
//!
//! | Concern            | Type / module                       |
//! |--------------------|-------------------------------------|
//! | data-source trait  | [`YtBackend`]                       |
//! | offline impl       | [`offline::OfflineBackend`]         |
//! | live impl (gated)  | `live::RustyPipeBackend` (`live`)   |
//! | PoToken seam       | [`potoken::PoTokenProvider`]        |
//! | request/result DTO | [`Query`], [`BackendResult`]        |
//!
//! Recommendations model (Home): the trait exposes the three no-auth signals
//! NewPipe / Piped use — [`YtBackend::subscription_feed`] (fan-out of latest
//! uploads across subscribed channels), [`YtBackend::trending`], and
//! [`YtBackend::related`] (per-video "up next"). See [`crate::recommend`].

pub mod offline;
pub mod potoken;

#[cfg(feature = "live")]
pub mod live;

use crate::model::{Channel, Video};

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
    /// Fan-out of the latest uploads across the given subscribed channels.
    SubscriptionFeed(Vec<String>),
}

/// The result of a [`Query`]. Backends map their native types onto Tube's
/// plain [`crate::model`] structs so the UI never sees a rustypipe type.
#[derive(Clone, Debug)]
pub enum BackendResult {
    Videos(Vec<Video>),
    Video(Box<Video>),
    Channel { channel: Box<Channel>, uploads: Vec<Video> },
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

/// The data-source seam. One trait, two worlds (offline sample / live
/// rustypipe). Methods are intentionally coarse-grained — one per screen need
/// — so a backend can batch the underlying InnerTube calls however it likes.
///
/// Implementations must be `Send + Sync` because the live backend is driven
/// from the fetch worker thread.
pub trait YtBackend: Send + Sync {
    /// Human name for the status log / settings ("offline sample",
    /// "rustypipe").
    fn name(&self) -> &str;

    /// Full-text video search.
    fn search(&self, query: &str) -> BackendOutcome;

    /// Full metadata for a single video.
    fn video(&self, id: &str) -> BackendOutcome;

    /// Channel page + uploads.
    fn channel(&self, id: &str) -> BackendOutcome;

    /// Per-video related / "up next".
    fn related(&self, id: &str) -> BackendOutcome;

    /// Region trending.
    fn trending(&self) -> BackendOutcome;

    /// Fan-out the latest uploads across `channel_ids` (the subscriptions
    /// feed). The default implementation pulls each channel's uploads via
    /// [`YtBackend::channel`] and interleaves them newest-first; a backend
    /// with a cheaper bulk endpoint may override.
    fn subscription_feed(&self, channel_ids: &[String]) -> BackendOutcome {
        crate::recommend::fan_out_subscription_feed(self, channel_ids)
    }

    /// Dispatch a [`Query`] to the right method. Lets the worker stay generic.
    fn run(&self, query: Query) -> BackendOutcome {
        match query {
            Query::Search(q) => self.search(&q),
            Query::Video(id) => self.video(&id),
            Query::Channel(id) => self.channel(&id),
            Query::Related(id) => self.related(&id),
            Query::Trending => self.trending(),
            Query::SubscriptionFeed(ids) => self.subscription_feed(&ids),
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
    fn default_backend_is_offline_and_searches() {
        let b = default_backend();
        let out = b.search("rust").expect("offline search never errors");
        match out {
            BackendResult::Videos(v) => assert!(!v.is_empty(), "sample has rust videos"),
            _ => panic!("search returns Videos"),
        }
    }

    #[test]
    fn run_dispatches_trending() {
        let b = default_backend();
        assert!(matches!(b.run(Query::Trending), Ok(BackendResult::Videos(_))));
    }
}
