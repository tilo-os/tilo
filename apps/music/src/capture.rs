//! The real catalogue a `live-*` screenshot shows.
//!
//! A capture reaches nothing outside the process while it renders, so the
//! catalogue it shows is fetched before it is built, into the capture's
//! fixture: the real Home page, or a real track with its synced lyrics, and
//! the cover art either shows. The surface then renders the fixture the way
//! it renders what its worker fetches in a live window.

use std::collections::HashMap;
use std::sync::Arc;

use crate::backend::{HomeContent, TrackHit};
use crate::lyrics::Lyrics;

/// The real Home page (charts, new releases, top artists) with its covers.
pub const LIVE_HOME: &str = "live-home";
/// A real track playing in the lyrics view, with its synced lyrics.
pub const LIVE_LYRICS: &str = "live-lyrics";

/// Whether `scenario` shows the real catalogue rather than the sample library.
pub fn is_live(scenario: &str) -> bool {
    scenario == LIVE_HOME || scenario == LIVE_LYRICS
}

/// What a live capture shows, fetched before it is built.
pub struct LiveCapture {
    /// The Home page, for [`LIVE_HOME`].
    pub home: Option<HomeContent>,
    /// The track playing and its lyrics, for [`LIVE_LYRICS`].
    pub playing: Option<(TrackHit, Lyrics)>,
    /// Cover art by URL.
    covers: Arc<HashMap<String, Vec<u8>>>,
}

impl LiveCapture {
    /// The cover art the capture shows, served from what was fetched.
    pub fn fetcher(&self) -> ui::Fetcher {
        let covers = Arc::clone(&self.covers);
        Arc::new(move |url: &str| covers.get(url).cloned())
    }

    /// Fetch what `scenario` shows, if it is a live one. Without the live
    /// catalogue (an offline build, no network) the capture shows what a
    /// live window shows then: the honest unavailable state.
    #[cfg(feature = "live")]
    pub fn fetch(scenario: Option<&str>) -> Option<Self> {
        use crate::backend::MusicBackend as _;

        /// The search whose top hit the lyrics capture plays.
        const LYRICS_QUERY: &str = "aurora runaway";

        let scenario = scenario.filter(|scenario| is_live(scenario))?;
        let backend = match crate::backend::live::RustyPipeBackend::new() {
            Ok(backend) => backend,
            Err(error) => {
                tracing::warn!(target: "music::capture", %error, "no live catalogue to capture");
                return Some(Self::unavailable());
            }
        };
        let mut capture = Self::unavailable();
        let mut cover_urls = Vec::new();
        if scenario == LIVE_HOME {
            match backend.home() {
                Ok(home) => {
                    cover_urls.extend(
                        home.sections
                            .iter()
                            .flat_map(|section| &section.items)
                            .map(|item| item.cover_url.clone()),
                    );
                    capture.home = Some(home);
                }
                Err(error) => tracing::warn!(target: "music::capture", %error, "no live Home"),
            }
        } else {
            match backend.search_tracks(LYRICS_QUERY) {
                Ok(hits) => {
                    if let Some(hit) = hits.into_iter().next() {
                        cover_urls.push(hit.cover_url.clone());
                        let lyrics = live_lyrics(&hit);
                        capture.playing = Some((hit, lyrics));
                    }
                }
                Err(error) => tracing::warn!(target: "music::capture", %error, "no live search"),
            }
        }
        capture.covers = Arc::new(fetch_covers(cover_urls));
        Some(capture)
    }

    /// No live catalogue in this build: a live scenario shows what a live
    /// window shows offline.
    #[cfg(not(feature = "live"))]
    pub fn fetch(scenario: Option<&str>) -> Option<Self> {
        scenario
            .filter(|scenario| is_live(scenario))
            .map(|_| Self::unavailable())
    }

    /// A capture of `home` or `playing`, its covers served from `covers`.
    #[cfg(test)]
    pub(crate) fn new(
        home: Option<HomeContent>,
        playing: Option<(TrackHit, Lyrics)>,
        covers: HashMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            home,
            playing,
            covers: Arc::new(covers),
        }
    }

    fn unavailable() -> Self {
        Self {
            home: None,
            playing: None,
            covers: Arc::default(),
        }
    }
}

/// The synced lyrics of `hit`, fetched now.
#[cfg(feature = "live")]
fn live_lyrics(hit: &TrackHit) -> Lyrics {
    let meta = crate::lyrics::TrackMeta {
        track_id: format!("{}{}", crate::app::LIVE_TRACK_PREFIX, hit.video_id),
        title: hit.title.clone(),
        artist: hit.artist.clone(),
        album: hit.album.clone().unwrap_or_default(),
        duration_s: hit.duration_s,
    };
    ui::rt::app()
        .block_on(async move { crate::lyrics::resolve(&meta).await })
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Every cover at `urls`.
#[cfg(feature = "live")]
fn fetch_covers(mut urls: Vec<String>) -> HashMap<String, Vec<u8>> {
    let fetch = crate::app::image_fetcher();
    urls.retain(|url| !url.is_empty());
    urls.sort();
    urls.dedup();
    urls.into_iter()
        .filter_map(|url| {
            let bytes = fetch(&url)?;
            Some((url, bytes))
        })
        .collect()
}
