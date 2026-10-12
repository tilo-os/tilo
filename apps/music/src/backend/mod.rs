//! Swappable YouTube-Music data-source backend.
//!
//! Music has two worlds behind one trait:
//!
//! * **offline** (the hermetic runtime mode): the bundled [`crate::seed`] sample
//!   library. `--shots`, CI and every test run here — no network, fully
//!   hermetic, deterministic screenshots. [`offline::OfflineBackend`] answers
//!   metadata from the sample and reports "cannot stream".
//! * **live** (included in shipping defaults): `YouTube` Music via the pure-Rust
//!   [`rustypipe`](https://docs.rs/rustypipe) `InnerTube` client —
//!   `music_search_tracks` for search results and `player`
//!   to resolve a **directly-playable audio-only stream URL** for a track.
//!
//! Both worlds implement the same [`MusicBackend`] trait: the cargo feature
//! picks the implementation at build time, and [`crate::stream`]'s worker
//! picks live-or-offline at *runtime*, falling back to offline if live init
//! or the network fails.

pub mod android_vr;
#[cfg(feature = "live")]
pub mod live;
pub mod offline;
#[cfg(feature = "live")]
mod sources;
#[cfg(feature = "live")]
pub mod spotify;

/// Signed media URLs are accepted only from `YouTube`'s HTTPS media origin.
/// A substring check would also accept `googlevideo.com.attacker.invalid`.
#[cfg(any(feature = "live", test))]
pub(crate) fn trusted_googlevideo_url(raw: &str) -> bool {
    if raw.len() > 16 * 1024 {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (host == "googlevideo.com" || host.ends_with(".googlevideo.com"))
}

/// A track returned by a search (real from `YouTube` Music, or none offline).
///
/// Toolkit- and serde-free so it crosses the worker-thread boundary cheaply and
/// never leaks a rustypipe type into the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackHit {
    /// `YouTube` video id — the key [`MusicBackend::resolve_audio`] streams from.
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
    /// Real album-cover thumbnail URL (`YouTube` image CDN), best-size pick.
    /// Empty when the source omits it (offline sample) — the UI then falls
    /// back to a procedural placeholder tile.
    pub cover_url: String,
}

/// What kind of release an album is, so an artist's discography can be
/// sectioned the way listeners expect (Albums vs. Singles & EPs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlbumKind {
    #[default]
    Album,
    Single,
    Ep,
    /// Audiobook / show / anything else — grouped under a generic label.
    Other,
}

impl AlbumKind {
    pub fn label(self) -> &'static str {
        match self {
            AlbumKind::Album => "Album",
            AlbumKind::Single => "Single",
            AlbumKind::Ep => "EP",
            AlbumKind::Other => "Release",
        }
    }
}

/// An album returned by a search. Toolkit-free so it crosses the worker boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumHit {
    /// YouTube-Music album browse id (e.g. `MPREb_…`).
    pub id: String,
    pub title: String,
    /// Primary artist display name.
    pub artist: String,
    pub artist_id: Option<String>,
    /// Release year (0 when omitted).
    pub year: u32,
    pub kind: AlbumKind,
    pub cover_url: String,
}

/// An artist returned by a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistHit {
    /// `YouTube` channel id (e.g. `UC…`) — the key [`MusicBackend::artist`] fetches.
    pub id: String,
    pub name: String,
    /// Approximate subscriber count (0 when hidden/omitted).
    pub subscriber_count: u64,
    /// Round avatar thumbnail URL.
    pub avatar_url: String,
}

/// A playlist returned by a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistHit {
    /// YouTube-Music playlist browse id (e.g. `PL…` / `RDCLAK…`).
    pub id: String,
    pub title: String,
    /// Channel / owner display name ("`YouTube` Music" for curated).
    pub subtitle: String,
    /// Track count (0 when omitted).
    pub track_count: u64,
    pub cover_url: String,
}

/// The single best "top result" a main search surfaces (the hero card). Carries
/// enough to render + route a click to the right destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchTop {
    Track(TrackHit),
    Album(AlbumHit),
    Artist(ArtistHit),
    Playlist(PlaylistHit),
}

/// A full multi-type search response: the mixed "top result" plus per-type
/// buckets, in YouTube-Music ranking order. The offline backend returns an
/// empty bundle; the live backend fills every bucket from one `music_search`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchBundle {
    pub top: Option<SearchTop>,
    pub tracks: Vec<TrackHit>,
    pub albums: Vec<AlbumHit>,
    pub artists: Vec<ArtistHit>,
    pub playlists: Vec<PlaylistHit>,
}

impl SearchBundle {
    pub fn is_empty(&self) -> bool {
        self.top.is_none()
            && self.tracks.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
            && self.playlists.is_empty()
    }
}

/// Which result type a deep per-type "see all" search targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Songs,
    Albums,
    Artists,
    Playlists,
}

/// What a [`HomeItem`] represents, so the UI can route a click and pick the
/// right tile shape (artists render as circles).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HomeItemKind {
    Track,
    Album,
    Artist,
    Playlist,
}

/// One tile on the real Home page (a chart track, a new album, a top artist, a
/// mood playlist). Toolkit-free so it crosses the worker boundary.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HomeSection {
    pub title: String,
    pub items: Vec<HomeItem>,
}

/// Real YouTube-Music home/charts content: a handful of titled sections.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
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
    pub kind: AlbumKind,
    pub cover_url: String,
}

/// A real YouTube-Music artist page: top songs, discography, related artists,
/// bio + subscriber count — everything a full artist page renders.
#[derive(Clone, Debug, Default)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub subscriber_count: u64,
    pub description: String,
    /// Link to the artist's Wikipedia page, when `YouTube` exposes one.
    pub wikipedia_url: String,
    pub header_url: String,
    pub tracks: Vec<TrackHit>,
    pub albums: Vec<ArtistAlbum>,
    /// Related / similar artists surfaced on the page.
    pub similar: Vec<ArtistHit>,
    /// Provider-confirmed device-local follow status, independent of the CRDT.
    pub provider_followed: Option<bool>,
}

/// A real YouTube-Music playlist: its ordered tracks + cover + owner. Backs the
/// live playlist detail page a search-result playlist click opens.
#[derive(Clone, Debug, Default)]
pub struct PlaylistPage {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub description: String,
    pub cover_url: String,
    pub tracks: Vec<TrackHit>,
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
/// media URL; feeding it to `GStreamer` `playbin` produces sound.
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
    /// `YouTube` stream format id (itag) — e.g. 140 (m4a 128k), 251 (opus 160k).
    pub itag: u32,
    /// Content length in bytes as reported by the player response.
    pub size_bytes: u64,
    /// Stream duration in milliseconds, when known.
    pub duration_ms: u64,
    /// A secondary source the audio engine falls back to when [`Self::url`]
    /// yields no bytes. Set when `url` is a (fully-rangeable but sometimes-down)
    /// Invidious proxy: the fallback is the direct googlevideo URL, which always
    /// renders at least the first segment. `None` when `url` is already the
    /// last-resort direct URL.
    pub fallback_url: Option<String>,
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

/// Device-local Spotify work; remote player state never claims local PCM.
#[derive(Clone, Debug)]
pub enum SpotifyOperation {
    Devices,
    Queue,
    LibraryPage {
        offset: u32,
        artists_after: Option<String>,
    },
    CreatePlaylist {
        name: String,
    },
    Transfer {
        device: String,
    },
    RemotePlay {
        device: String,
        track: String,
    },
    RemotePause {
        device: String,
    },
    Save {
        uri: String,
        saved: bool,
    },
    RenamePlaylist {
        playlist: String,
        name: String,
    },
    PlaylistAdd {
        playlist: String,
        track: String,
    },
    PlaylistRemove {
        playlist: String,
        track: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct SpotifyDevice {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub restricted: bool,
}

#[derive(Clone, Debug)]
pub struct SpotifyReply {
    pub devices: Vec<SpotifyDevice>,
    pub state: serde_json::Value,
}

/// The data-source seam. One trait, two worlds (offline sample / live
/// rustypipe). Implementations are `Send + Sync` because the live backend is
/// driven from the [`crate::stream`] worker thread.
pub trait MusicBackend: Send + Sync {
    /// Execute a Spotify API operation, reporting only confirmed provider state.
    /// # Errors
    /// Source unavailable, missing scopes, restricted device or unconfirmed state.
    fn spotify_operation(
        &self,
        _operation: &SpotifyOperation,
    ) -> Result<SpotifyReply, BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }
    /// Human name for the status log / toast ("offline (bundled library)",
    /// "youtube-music (live)").
    fn name(&self) -> &str;

    /// Select a catalogue or authorize it, on the background worker.
    ///
    /// # Errors
    /// Source unavailable, sign-in failed or account inaccessible.
    fn select_source(&self, _spotify: bool, _sign_in: bool) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Connect with profile and library read access only.
    /// # Errors
    /// Source unavailable, sign-in failed or account inaccessible.
    fn connect_spotify_read_only(&self) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Validate Premium entitlement and the separate native playback grant.
    /// # Errors
    /// Unavailable support, missing sign-in, Free account or rejected grant.
    fn spotify_playback(&self, _sign_in: bool) -> Result<String, BackendError> {
        Err(BackendError(
            "Native Spotify playback is unavailable in this build".into(),
        ))
    }

    /// Forget Spotify authorization on this device.
    ///
    /// # Errors
    /// Spotify unavailable or credential custody failed.
    fn disconnect_spotify(&self) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Whether this backend can actually reach `YouTube` and stream audio. The UI
    /// uses it to decide whether to surface live search / "streaming" state.
    fn is_live(&self) -> bool {
        false
    }

    /// Real full-text track search. Offline returns an empty list (the app
    /// shows the bundled library instead); live hits `YouTube` Music.
    ///
    /// # Errors
    ///
    /// Live: returns a [`BackendError`] on a network failure or an
    /// unparseable `YouTube` Music response.
    fn search_tracks(&self, query: &str) -> SearchOutcome;

    /// A real multi-type search: the mixed "top result" plus songs, albums,
    /// artists and playlists in one call (`YouTube` Music's main results page).
    /// Offline returns an empty bundle; live fills every bucket.
    ///
    /// # Errors
    ///
    /// Live: returns a [`BackendError`] on a network failure or an
    /// unparseable `YouTube` Music response. The default (offline) body never
    /// errors.
    fn search_all(&self, _query: &str) -> Result<SearchBundle, BackendError> {
        Ok(SearchBundle::default())
    }

    /// Deep per-type search ("see all Albums / Artists / Playlists"). Offline
    /// returns an empty bundle; live fills just the requested bucket with a
    /// fuller list than the mixed [`Self::search_all`] surfaces.
    ///
    /// # Errors
    ///
    /// Live: returns a [`BackendError`] on a network failure or an
    /// unparseable `YouTube` Music response. The default (offline) body never
    /// errors.
    fn search_more(&self, _query: &str, _kind: SearchKind) -> Result<SearchBundle, BackendError> {
        Ok(SearchBundle::default())
    }

    /// Resolve a video id to a directly-playable audio stream. Offline errors;
    /// live returns a real googlevideo media URL.
    ///
    /// # Errors
    ///
    /// Offline: always errors (there is no stream to resolve). Live: returns
    /// a [`BackendError`] if the video id is unknown/unplayable or the
    /// resolution request fails.
    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome;

    /// Fetch the real Home page: charts, trending, new releases, top artists —
    /// with real cover art URLs. Offline returns an error (the app then shows
    /// the bundled sample home).
    ///
    /// # Errors
    ///
    /// The default (offline) body always errors. Live: returns a
    /// [`BackendError`] on a network failure or an unparseable response.
    fn home(&self) -> Result<HomeContent, BackendError> {
        Err(BackendError("home content needs the live backend".into()))
    }

    /// Fetch a real artist page (top songs, albums, subscriber count). Offline
    /// errors.
    ///
    /// # Errors
    ///
    /// The default (offline) body always errors. Live: returns a
    /// [`BackendError`] if the artist id is unknown or the request fails.
    fn artist(&self, _artist_id: &str) -> Result<ArtistPage, BackendError> {
        Err(BackendError("artist pages need the live backend".into()))
    }

    /// Fetch a real album's ordered track list + cover. Offline errors.
    ///
    /// # Errors
    ///
    /// The default (offline) body always errors. Live: returns a
    /// [`BackendError`] if the album id is unknown or the request fails.
    fn album(&self, _album_id: &str) -> Result<AlbumPage, BackendError> {
        Err(BackendError("album pages need the live backend".into()))
    }

    /// Fetch a real playlist's ordered track list + cover. Offline errors.
    ///
    /// # Errors
    ///
    /// The default (offline) body always errors. Live: returns a
    /// [`BackendError`] if the playlist id is unknown or the request fails.
    fn playlist(&self, _playlist_id: &str) -> Result<PlaylistPage, BackendError> {
        Err(BackendError("playlist pages need the live backend".into()))
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
                return Box::new(sources::Sources::new(Box::new(b)));
            }
            Err(e) => {
                note(format!(
                    "live backend unavailable ({e}); using offline sample"
                ));
            }
        }
    }
    #[cfg(not(feature = "live"))]
    {
        note("offline build (no `live` feature); using bundled library".to_string());
    }
    #[cfg(feature = "live")]
    {
        Box::new(sources::Sources::new(default_backend()))
    }
    #[cfg(not(feature = "live"))]
    default_backend()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn default_backend_is_offline_empty_and_cannot_stream() {
        // Phase `default_backend_is_offline_and_cannot_stream`.
        {
            let b = default_backend();
            assert!(!b.is_live());
            // Offline search never errors; it just yields nothing (local library
            // is what the app renders offline).
            assert!(b.search_tracks("aurora").unwrap().is_empty());
            // Resolution reports it cannot stream, rather than faking a URL.
            assert!(b.resolve_audio("dQw4w9WgXcQ").is_err());
        }

        // Phase `default_backend_multi_type_search_is_empty_bundle`.
        {
            let b = default_backend();
            // The multi-type search shape exists on the real trait; offline it just
            // yields an empty bundle across every bucket (never an error).
            let bundle = b.search_all("aurora").unwrap();
            assert!(bundle.is_empty());
            assert!(bundle.tracks.is_empty());
            assert!(bundle.albums.is_empty());
            assert!(bundle.artists.is_empty());
            assert!(bundle.playlists.is_empty());
            assert!(bundle.top.is_none());
            for kind in [
                SearchKind::Songs,
                SearchKind::Albums,
                SearchKind::Artists,
                SearchKind::Playlists,
            ] {
                assert!(b.search_more("aurora", kind).unwrap().is_empty());
            }
        }

        // Phase `preferred_backend_offline_build_notes_and_is_offline`.
        {
            // A deliberately feature-minimal build remains honest and offline;
            // the ordinary default build has `live` enabled.
            let mut msg = String::new();
            let b = preferred_backend(|m| msg = m);
            assert!(!msg.is_empty());
            #[cfg(not(feature = "live"))]
            assert!(!b.is_live());
            let _ = b;
        }
    }
}
