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
