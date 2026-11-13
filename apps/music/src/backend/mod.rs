//! The catalogue backend: one trait with two implementations, the bundled offline sample
//! and the live `YouTube` Music client. The worker picks one at runtime and falls
//! back to offline.

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

/// A track from a search; toolkit- and serde-free so it crosses the worker boundary
/// cheaply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackHit {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    /// The length in seconds, 0 when the source omits it.
    pub duration_s: u32,
    /// Real album-cover thumbnail URL (`YouTube` image CDN), best-size pick.
    /// Empty when the source omits it (offline sample) — the UI then falls
    /// back to a procedural placeholder tile.
    pub cover_url: String,
}

/// What kind of release an album is, so a discography can be sectioned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlbumKind {
    #[default]
    Album,
    Single,
    Ep,
    /// Anything else: audiobooks, shows.
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

/// An album from a search, toolkit-free like the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumHit {
    /// The album's catalogue id, `MPREb_…`.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    /// The release year, 0 when omitted.
    pub year: u32,
    pub kind: AlbumKind,
    pub cover_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistHit {
    /// The channel id, `UC…`.
    pub id: String,
    pub name: String,
    pub subscriber_count: u64,
    pub avatar_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistHit {
    /// The playlist's catalogue id, `PL…` or `RDCLAK…`.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: u64,
    pub cover_url: String,
}

/// The best result a main search surfaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchTop {
    Track(TrackHit),
    Album(AlbumHit),
    Artist(ArtistHit),
    Playlist(PlaylistHit),
}

/// A whole search answer: the top result and one bucket per type.
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

/// What a [`HomeItem`] is, so the UI routes a click and picks the tile shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HomeItemKind {
    Track,
    Album,
    Artist,
    Playlist,
}

/// One tile on the Home page.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HomeItem {
    /// The entity's id; for a track, the library id the app mints.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub cover_url: String,
    pub kind: HomeItemKind,
    // The stream key, for a track tile.
    pub video_id: String,
    pub artist_id: String,
    pub artist_name: String,
    pub album_id: String,
    pub album_title: String,
    pub duration_s: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HomeSection {
    pub title: String,
    pub items: Vec<HomeItem>,
}

/// Home content, as a few titled sections.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct HomeContent {
    pub sections: Vec<HomeSection>,
}

impl HomeContent {
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|s| s.items.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct ArtistAlbum {
    pub id: String,
    pub title: String,
    pub year: u32,
    pub kind: AlbumKind,
    pub cover_url: String,
}

/// A full artist page: top songs, discography, related artists, bio, subscribers.
#[derive(Clone, Debug, Default)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub subscriber_count: u64,
    pub description: String,
    pub wikipedia_url: String,
    pub header_url: String,
    pub tracks: Vec<TrackHit>,
    pub albums: Vec<ArtistAlbum>,
    pub similar: Vec<ArtistHit>,
    /// Follow status as the provider reports it; the library's own is separate.
    pub provider_followed: Option<bool>,
}

/// A catalogue playlist: its ordered tracks, cover and owner.
#[derive(Clone, Debug, Default)]
pub struct PlaylistPage {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub description: String,
    pub cover_url: String,
    pub tracks: Vec<TrackHit>,
}

/// A catalogue album: its ordered tracks and cover.
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

/// A playable stream, as the audio engine takes it: an HTTPS media URL.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedStream {
    pub video_id: String,
    pub url: String,
    /// Full MIME (e.g. `video/mp4; codecs="avc1.42001E, mp4a.40.2"`).
    pub mime: String,
    /// Average bitrate in bits per second.
    pub bitrate: u32,
    /// The stream's format id, e.g. 18.
    pub itag: u32,
    /// The stream's length in bytes, as the player reports it.
    pub size_bytes: u64,
    pub duration_ms: u64,
}

/// Why a backend could not answer; the UI shows it in a toast.
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
    AccountStatus,
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

/// Provider product classifications; unknown metadata never establishes eligibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpotifyProduct {
    Premium,
    Free,
    Open,
    Unknown,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SpotifyAccountStatus {
    pub catalogue_product: SpotifyProduct,
    pub streaming_product: SpotifyProduct,
    pub account_match: bool,
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

/// The data source; implementations are `Send + Sync` because the worker drives them.
pub trait MusicBackend: Send + Sync {
    /// Execute a Spotify API operation, reporting only confirmed provider state.
    fn spotify_operation(
        &self,
        _operation: &SpotifyOperation,
    ) -> Result<SpotifyReply, BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }
    /// The backend's name, for the status log.
    fn name(&self) -> &str;

    fn select_source(&self, _spotify: bool, _sign_in: bool) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Connect with profile and library read access only.
    fn connect_spotify_read_only(&self) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Validate Premium entitlement and the separate native playback grant.
    fn spotify_playback(&self, _sign_in: bool) -> Result<String, BackendError> {
        Err(BackendError(
            "Native Spotify playback is unavailable in this build".into(),
        ))
    }

    fn disconnect_spotify(&self) -> Result<(), BackendError> {
        Err(BackendError("Spotify is unavailable in this build".into()))
    }

    /// Whether this backend can reach the catalogue and stream audio.
    fn is_live(&self) -> bool {
        false
    }

    /// Real track search; offline returns nothing, live asks `YouTube` Music.
    ///
    /// # Errors
    ///
    /// A [`BackendError`] on a network failure or an unparseable response, live.
    fn search_tracks(&self, query: &str) -> SearchOutcome;

    /// A multi-type search: the top result plus songs, albums, artists and playlists.
    ///
    /// # Errors
    ///
    /// A [`BackendError`] on a network failure or an unparseable response, live.
    fn search_all(&self, _query: &str) -> Result<SearchBundle, BackendError> {
        Ok(SearchBundle::default())
    }

    /// The catalogue's quick answer while a query is typed, a fraction of
    /// [`Self::search_all`]'s time; offline there is none.
    fn search_quick(&self, _query: &str) -> Result<SearchBundle, BackendError> {
        Ok(SearchBundle::default())
    }

    /// Open the connection before the first query; blocks, so it runs off the UI thread.
    fn warm(&self) {}

    fn catalogue_label(&self) -> &'static str {
        "YouTube Music"
    }

    /// A playable stream URL for a video id; offline errors.
    ///
    /// # Errors
    ///
    /// Offline always; live when the id is unplayable or the request fails.
    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome;

    /// The Home page: charts, new releases, top artists with cover art; offline errors.
    ///
    /// # Errors
    ///
    /// The offline body always; live on a network failure or an unparseable response.
    fn home(&self) -> Result<HomeContent, BackendError> {
        Err(BackendError("home content needs the live backend".into()))
    }

    /// An artist page: top songs, albums, subscriber count; offline errors.
    ///
    /// # Errors
    ///
    /// The offline body always; live when the id is unknown or the request fails.
    fn artist(&self, _artist_id: &str) -> Result<ArtistPage, BackendError> {
        Err(BackendError("artist pages need the live backend".into()))
    }

    /// An album's ordered tracks and cover; offline errors.
    ///
    /// # Errors
    ///
    /// The offline body always; live when the id is unknown or the request fails.
    fn album(&self, _album_id: &str) -> Result<AlbumPage, BackendError> {
        Err(BackendError("album pages need the live backend".into()))
    }

    /// A playlist's ordered tracks and cover; offline errors.
    ///
    /// # Errors
    ///
    /// The offline body always; live when the id is unknown or the request fails.
    fn playlist(&self, _playlist_id: &str) -> Result<PlaylistPage, BackendError> {
        Err(BackendError("playlist pages need the live backend".into()))
    }
}

/// The offline sample: what a build without a working live client uses.
pub fn default_backend() -> Box<dyn MusicBackend> {
    Box::new(offline::OfflineBackend::new())
}

/// The live client when it initialises, else the offline sample; `note` carries a
/// status line for the UI.
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
    fn default_backend_is_offline_empty_and_cannot_stream() {
        {
            let b = default_backend();
            assert!(!b.is_live());
            assert!(b.search_tracks("aurora").unwrap().is_empty());
            assert!(b.resolve_audio("dQw4w9WgXcQ").is_err());
        }

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
        }

        {
            let mut msg = String::new();
            let b = preferred_backend(|m| msg = m);
            assert!(!msg.is_empty());
            #[cfg(not(feature = "live"))]
            assert!(!b.is_live());
            let _ = b;
        }
    }
}
