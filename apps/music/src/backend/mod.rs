//! Swappable YouTube-Music data-source backend.
//!
//! Music has two worlds behind one trait:
//!
//! * **offline** (the default build): the bundled [`crate::seed`] sample
//!   library. `--shots`, CI and every test run here — no network, fully
//!   hermetic, deterministic screenshots. [`offline::OfflineBackend`] answers
//!   metadata from the sample and reports "cannot stream".
//! * **live** (`--features live`): real YouTube Music via the pure-Rust
//!   [`rustypipe`](https://docs.rs/rustypipe) InnerTube client —
//!   `music_search_tracks` for real search results and, crucially, `player`
//!   to resolve a **directly-playable audio-only stream URL** for a track.
//!
//! Both worlds implement the same [`MusicBackend`] trait, so the rest of the
//! app (the non-blocking [`crate::stream`] worker) is written once against the
//! trait and the cargo feature picks the implementation at build time; the
//! worker then picks live-or-offline at *runtime* (falling back to offline if
//! live init or the network fails). This is the clean seam the brief asks for:
//! rustypipe is real but gated, the offline sample is the default, and the
//! PoToken concern is a module you can swap ([`potoken`]).
//!
//! ## The stream-resolution gotcha (PoToken / SABR)
//!
//! YouTube gates *some* clients' streams behind a BotGuard-minted PoToken. The
//! trick rustypipe uses (and we rely on): the **iOS** and **TV** player clients
//! return direct `googlevideo` URLs **without** a PoToken. rustypipe's default
//! `player_client_order` with no BotGuard is `[Ios, Tv]`, so
//! [`MusicBackend::resolve_audio`] gets a real, fetchable audio URL with zero
//! extra runtime deps. The [`potoken`] seam stays in place so a future BotGuard
//! provider can unlock the Desktop/Music clients if YouTube ever tightens this.

pub mod offline;
pub mod potoken;

#[cfg(feature = "live")]
pub mod live;

/// A track returned by a search (real from YouTube Music, or none offline).
///
/// Toolkit- and serde-free so it crosses the worker-thread boundary cheaply and
/// never leaks a rustypipe type into the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackHit {
    /// YouTube video id — the key [`MusicBackend::resolve_audio`] streams from.
    pub video_id: String,
    pub title: String,
    /// Primary artist display name ("Various Artists" for VA compilations).
    pub artist: String,
    /// Primary artist channel id, when known.
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    /// Track length in whole seconds (0 when the source omits it).
    pub duration_s: u32,
    /// Real album-cover thumbnail URL (YouTube image CDN), best-size pick.
    /// Empty when the source omits it (offline sample) — the UI then falls
    /// back to a procedural placeholder tile.
    pub cover_url: String,
}

/// What a [`HomeItem`] represents, so the UI can route a click and pick the
/// right tile shape (artists render as circles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeItemKind {
    Track,
    Album,
    Artist,
    Playlist,
}

/// One tile on the real Home page (a chart track, a new album, a top artist, a
/// mood playlist). Toolkit-free so it crosses the worker boundary.
#[derive(Clone, Debug)]
pub struct HomeItem {
    /// Entity id: album id / artist channel id / playlist id. For a track it's
    /// the library id the app mints; [`Self::video_id`] carries the stream key.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub cover_url: String,
    pub kind: HomeItemKind,
    // Track-only correlation so a chart track can be played + merged.
    pub video_id: String,
    pub artist_id: String,
    pub artist_name: String,
    pub album_id: String,
    pub album_title: String,
    pub duration_s: u32,
}

/// A titled row of tiles on the Home page.
#[derive(Clone, Debug)]
pub struct HomeSection {
    pub title: String,
    pub items: Vec<HomeItem>,
}

/// Real YouTube-Music home/charts content: a handful of titled sections.
#[derive(Clone, Debug, Default)]
pub struct HomeContent {
    pub sections: Vec<HomeSection>,
}

impl HomeContent {
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.items.is_empty())
    }
}

/// One album on an artist page.
#[derive(Clone, Debug)]
pub struct ArtistAlbum {
    pub id: String,
    pub title: String,
    pub year: u32,
    pub cover_url: String,
}

/// A real YouTube-Music artist page: top songs, albums, subscriber count.
#[derive(Clone, Debug, Default)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub subscriber_count: u64,
    pub description: String,
    pub header_url: String,
    pub tracks: Vec<TrackHit>,
    pub albums: Vec<ArtistAlbum>,
}

/// A real YouTube-Music album (or single/EP): its ordered tracks + cover.
#[derive(Clone, Debug, Default)]
pub struct AlbumPage {
    pub id: String,
    pub title: String,
    pub artist_id: String,
    pub artist_name: String,
    pub year: u32,
    pub cover_url: String,
    pub tracks: Vec<TrackHit>,
}

/// A resolved, directly-playable audio stream — what actually gets handed to
/// the audio engine. The `url` is a fetchable `https://…googlevideo.com/…`
/// media URL; feeding it to GStreamer `playbin` produces sound.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedStream {
    pub video_id: String,
    pub url: String,
    /// Full MIME (e.g. `audio/mp4; codecs="mp4a.40.2"`).
    pub mime: String,
    /// Short codec name (`mp4a`, `opus`, …).
    pub codec: String,
    /// Container format (`m4a`, `webm`).
    pub format: String,
    /// Average bitrate in bits/second (higher = better).
    pub bitrate: u32,
    /// YouTube stream format id (itag) — e.g. 140 (m4a 128k), 251 (opus 160k).
    pub itag: u32,
    /// Content length in bytes as reported by the player response.
    pub size_bytes: u64,
    /// Stream duration in milliseconds, when known.
    pub duration_ms: u64,
}

/// Error surfaced by a backend. Stringly-typed on purpose — the UI only shows
/// it in a toast / the probe prints it; the offline backend never errors on
/// search (it returns an empty result set and the app shows the local library).
#[derive(Clone, Debug)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BackendError {}

pub type SearchOutcome = Result<Vec<TrackHit>, BackendError>;
pub type ResolveOutcome = Result<ResolvedStream, BackendError>;

/// The data-source seam. One trait, two worlds (offline sample / live
/// rustypipe). Implementations are `Send + Sync` because the live backend is
/// driven from the [`crate::stream`] worker thread.
pub trait MusicBackend: Send + Sync {
    /// Human name for the status log / toast ("offline (bundled library)",
    /// "youtube-music (live)").
    fn name(&self) -> &str;

    /// Whether this backend can actually reach YouTube and stream audio. The UI
    /// uses it to decide whether to surface live search / "streaming" state.
    fn is_live(&self) -> bool {
        false
    }

    /// Real full-text track search. Offline returns an empty list (the app
    /// shows the bundled library instead); live hits YouTube Music.
    fn search_tracks(&self, query: &str) -> SearchOutcome;

    /// Resolve a video id to a directly-playable audio stream. Offline errors;
    /// live returns a real googlevideo media URL.
    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome;

    /// Fetch the real Home page: charts, trending, new releases, top artists —
    /// with real cover art URLs. Offline returns an error (the app then shows
    /// the bundled sample home).
    fn home(&self) -> Result<HomeContent, BackendError> {
        Err(BackendError("home content needs the live backend".into()))
    }

    /// Fetch a real artist page (top songs, albums, subscriber count). Offline
    /// errors.
    fn artist(&self, _artist_id: &str) -> Result<ArtistPage, BackendError> {
        Err(BackendError("artist pages need the live backend".into()))
    }

    /// Fetch a real album's ordered track list + cover. Offline errors.
    fn album(&self, _album_id: &str) -> Result<AlbumPage, BackendError> {
        Err(BackendError("album pages need the live backend".into()))
    }
}

/// The default backend for any build: the offline sample. The live backend is
/// constructed explicitly by the worker under `--features live` and only when
/// its network init succeeds — otherwise this is the graceful fallback.
pub fn default_backend() -> Box<dyn MusicBackend> {
    Box::new(offline::OfflineBackend::new())
}

/// Construct the best backend available in this build: the live rustypipe
/// client if `--features live` is on and it initialises, otherwise the offline
/// sample. Never panics — a failed live init degrades to offline, so the worker
/// always has a working data source. The `note` closure receives a one-line
/// status string for the UI/log.
pub fn preferred_backend(note: impl FnOnce(String)) -> Box<dyn MusicBackend> {
    #[cfg(feature = "live")]
    {
        match live::RustyPipeBackend::new() {
            Ok(b) => {
                note(format!("live backend ready: {}", b.name()));
                return Box::new(b);
            }
            Err(e) => {
                note(format!("live backend unavailable ({e}); using offline sample"));
            }
        }
    }
    #[cfg(not(feature = "live"))]
    {
        note("offline build (no `live` feature); using bundled library".to_string());
    }
    default_backend()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_is_offline_and_cannot_stream() {
        let b = default_backend();
        assert!(!b.is_live());
        // Offline search never errors; it just yields nothing (local library
        // is what the app renders offline).
        assert!(b.search_tracks("aurora").unwrap().is_empty());
        // Resolution reports it cannot stream, rather than faking a URL.
        assert!(b.resolve_audio("dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn preferred_backend_offline_build_notes_and_is_offline() {
        // In the default (non-`live`) build the preferred backend is offline.
        let mut msg = String::new();
        let b = preferred_backend(|m| msg = m);
        assert!(!msg.is_empty());
        #[cfg(not(feature = "live"))]
        assert!(!b.is_live());
        let _ = b;
    }
}
