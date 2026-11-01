//! Music's effects and the rows it publishes for what the window shows.
//!
//! Every change to the shared library is the [`Library`](crate::model::Library)
//! resource's: its verbs (making and deleting a playlist, bringing in a
//! catalogue song) and field edits (likes, saved albums, followed artists,
//! recommendation feedback, a playlist's name, cover and tracks). The
//! on-screen affordances, the palette and agents all submit those.
//!
//! What Music does that changes no document is a [`MusicEffect`]: what is
//! shown, and this device's player, queue and offline copies. Opening a
//! destination or an album of the library is all the library's own row
//! offers, published from [`crate::model::Library`] itself; the songs,
//! artists and playlists, whose rows say what this device can do with them,
//! are rows this window builds ([`MusicRows`]), beside its player and queue.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Effect};

use tilo_actions::ActionSpec;
/// The maximum decoded size of a portable embedded playlist cover.
pub const MAX_PORTABLE_COVER_BYTES: usize = 256 * 1024;
/// Portal normalization never persists an image wider or taller than this.
pub const MAX_PORTABLE_COVER_EDGE: u32 = 512;
/// The contextual Ctrl+K snapshot is deliberately small. The full library is
/// still discoverable through the paged search provider and every returned
/// canonical address is resolved through `PublishesResources::resource`.
/// Keeping this bounded prevents opening the palette from cloning a large
/// library (or every membership in every long playlist) on the UI thread.
pub const OPEN_RESOURCE_CATEGORY_LIMIT: usize = 16;
/// A playlist shown in the contextual snapshot exposes its first few ordered
/// memberships. The complete playlist remains reachable by its stable
/// resource id, and the visible app has its own virtualised list.
pub const OPEN_PLAYLIST_TRACK_LIMIT: usize = 32;

/// Stable identifiers for Music's app-owned destinations.  These are not
/// synthetic content: every value names a real screen in [`crate::app::View`].
/// Keeping the allowlist at the resource boundary prevents a caller from
/// turning an arbitrary string into local routing authority.
pub const NAVIGATION_VIEWS: &[(&str, &str)] = &[
    ("home", "Home"),
    ("recommendations", "Recommendations"),
    ("songs", "Songs"),
    ("albums", "Albums"),
    ("artists", "Artists"),
    ("playlists", "Playlists"),
    ("downloads", "Downloads"),
    ("liked", "Liked songs"),
    ("browse", "Browse"),
    ("queue", "Queue"),
    ("lyrics", "Lyrics"),
];

pub fn view_resource_id(id: &str) -> String {
    format!("music.view:{id}")
}

pub fn known_view_label(id: &str) -> Option<&'static str> {
    NAVIGATION_VIEWS
        .iter()
        .find_map(|(candidate, label)| (*candidate == id).then_some(*label))
}

pub fn parse_view_resource_id(id: &str) -> Option<&str> {
    let view = id.strip_prefix("music.view:")?;
    known_view_label(view).map(|_| view)
}

pub fn track_resource_id(id: &str) -> String {
    format!("music.track:{id}")
}

pub fn album_resource_id(id: &str) -> String {
    format!("music.album:{id}")
}

pub fn artist_resource_id(id: &str) -> String {
    format!("music.artist:{id}")
}

/// A playlist is an item of the library: its row joins the item, which
/// offers deleting it.
pub fn playlist_resource_id(id: &str) -> String {
    tilo_resource::items::item_resource_id::<crate::model::Library>(
        "playlists",
        &tilo_resource::Id::from_raw(id),
    )
}

/// The playlist a playlist row id names.
pub fn parse_playlist_resource_id(id: &str) -> Option<String> {
    tilo_resource::items::item_of_resource::<crate::model::Library>(id)
        .filter(|(field, item)| *field == "playlists" && !item.as_str().is_empty())
        .map(|(_, item)| item.as_str().to_owned())
}

/// The local-device queue. Unlike a playlist, this is intentionally not part
/// of the shared library document: queuing or playing a track on one device
/// must never commandeer a collaborator's speakers.
pub const QUEUE_RESOURCE_ID: &str = "music.queue:queue";

/// A queue occurrence has an identity of its own. A track may legitimately
/// appear more than once, so `music.track:<id>` cannot name a queue row.
/// Encoding both the observed index and the track id lets the operation host
/// fail closed if the local queue changed before an old palette result lands.
pub fn queue_item_resource_id(index: usize, track: &str) -> String {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "music.queue-item:v1:{index}:{}",
        engine.encode(track.as_bytes())
    )
}

/// Decode a queue occurrence resource identity. Invalid base64, UTF-8, and
/// empty ids never become addressable queue authority.
pub fn parse_queue_item_resource_id(id: &str) -> Option<(usize, String)> {
    let value = id.strip_prefix("music.queue-item:v1:")?;
    let (index, track) = value.split_once(':')?;
    let index = index.parse().ok()?;
    if track.is_empty() {
        return None;
    }
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let track = String::from_utf8(engine.decode(track).ok()?).ok()?;
    (!track.is_empty()).then_some((index, track))
}

/// A playlist membership is distinct from the track itself: it owns the
/// playlist-specific remove and reorder actions without duplicating a track's
/// library-level identity in the resource tree.
pub fn playlist_track_resource_id(playlist: &str, track: &str) -> String {
    // URL-safe base64 without padding is an unambiguous, Unicode-preserving
    // component encoding (unlike joining raw ids with `:`). The stable v1
    // prefix leaves room for a future representation without changing the
    // namespace of existing resources.
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "music.playlist-track:v1:{}:{}",
        engine.encode(playlist.as_bytes()),
        engine.encode(track.as_bytes())
    )
}

/// Decode a canonical playlist-membership resource identity. Invalid UTF-8,
/// missing fields, and malformed base64 values never become an addressable
/// resource.
pub fn parse_playlist_track_resource_id(id: &str) -> Option<(String, String)> {
    let value = id.strip_prefix("music.playlist-track:v1:")?;
    let (playlist, track) = value.split_once(':')?;
    if playlist.is_empty() || track.is_empty() {
        return None;
    }
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let playlist = String::from_utf8(engine.decode(playlist).ok()?).ok()?;
    let track = String::from_utf8(engine.decode(track).ok()?).ok()?;
    (!playlist.is_empty() && !track.is_empty()).then_some((playlist, track))
}

/// A playlist the live catalogue found that the library does not hold.
pub fn catalogue_playlist_resource_id(id: &str) -> String {
    format!("music.catalogue-playlist:{id}")
}

/// What opening the row `anchor` names does — a search hit's or an agent's
/// `open`: a song plays, an album, artist, playlist, membership or place is
/// shown. `None` for an anchor that names no row Music opens.
pub fn anchor_effect(anchor: &str) -> Option<MusicEffect> {
    let open = |place| Some(MusicEffect::Open { place });
    if let Some(view) = parse_view_resource_id(anchor) {
        return open(Place::View(view.to_owned()));
    }
    if anchor == crate::recommendations::PREFERENCES {
        return open(Place::View("recommendations".to_owned()));
    }
    let named = |prefix: &str| {
        anchor
            .strip_prefix(prefix)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    };
    if let Some(track) = named("music.track:") {
        return Some(MusicEffect::Play {
            track,
            queued: None,
        });
    }
    if let Some(album) = named("music.album:") {
        return open(Place::Album(album));
    }
    if let Some(artist) = named("music.artist:") {
        return open(Place::Artist(artist));
    }
    if let Some(playlist) = named("music.catalogue-playlist:") {
        return open(Place::Playlist(playlist));
    }
    if let Some((playlist, track)) = parse_playlist_track_resource_id(anchor) {
        return open(Place::Membership { playlist, track });
    }
    parse_playlist_resource_id(anchor).and_then(|playlist| open(Place::Playlist(playlist)))
}

/// A place in Music a view effect shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    /// One of the stable destinations in [`NAVIGATION_VIEWS`]. The receiver
    /// validates the id again before it changes what is shown.
    View(String),
    Album(String),
    Artist(String),
    Playlist(String),
    /// A playlist, with the still-current membership row revealed.
    Membership {
        playlist: String,
        track: String,
    },
}

/// What Music does that changes no document: what is shown, and this
/// device's player, queue and offline copies. Stream resolution and audio
/// output are never replicated into a collaborator's session, and the queue
/// and transport stay this device's own. Effects on one item ride that item;
/// the ones that take arguments a caller chooses are offered on the library.
#[derive(Debug, Clone, PartialEq, Effect)]
#[effect(operation = "music.effects")]
pub enum MusicEffect {
    /// Choose music source
    ///
    /// Opens the source picker without starting sign-in or changing a grant.
    Sources,
    /// Enable full Spotify library and device controls
    ///
    /// Explicitly requests editing, playlist, followed-artist and remote-device permissions.
    #[effect(attended, job)]
    ConnectSpotifyLibrary,
    /// Connect Spotify
    ///
    /// Requests profile and saved-library read access, without editing or remote control.
    #[effect(attended, job)]
    ConnectSpotify,
    /// Authorize Spotify playback
    ///
    /// Separately grants native streaming for the same Premium account.
    #[effect(attended, job)]
    ConnectSpotifyPlayback,
    /// Inspect Spotify account eligibility
    /// Reads existing grants only; returns product classifications and account matching.
    #[effect(job, returns, idempotent, kind = "read")]
    SpotifyAccountStatus,
    /// Refresh Spotify devices
    #[effect(job, returns)]
    SpotifyDevices,
    /// Show Spotify's remote queue
    ///
    /// Reads a provider snapshot; never replaces this device's queue.
    #[effect(job, returns)]
    SpotifyQueue,
    /// Load more Spotify library collections
    #[effect(job, returns)]
    SpotifyLibraryMore,
    /// Name a new private Spotify playlist
    NewSpotifyPlaylist,
    /// Create private Spotify playlist
    ///
    /// Creates an empty private playlist. Refresh before retrying an uncertain result.
    #[effect(job, returns)]
    SpotifyCreatePlaylist {
        #[param("Playlist name")]
        name: String,
    },
    /// Transfer Spotify playback
    #[effect(job, returns)]
    SpotifyTransfer {
        #[param("Spotify Connect device ID")]
        device: String,
    },
    /// Play on a Spotify device
    #[effect(job, returns)]
    SpotifyRemotePlay {
        #[param("Spotify Connect device ID")]
        device: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Pause a Spotify device
    #[effect(job, returns)]
    SpotifyRemotePause {
        #[param("Spotify Connect device ID")]
        device: String,
    },
    /// Save to Spotify library
    #[effect(job, returns)]
    SpotifySave {
        #[param("Spotify track, album, artist or playlist URI")]
        uri: String,
    },
    /// Remove from Spotify library
    #[effect(job, returns)]
    SpotifyUnsave {
        #[param("Spotify track, album, artist or playlist URI")]
        uri: String,
    },
    /// Rename Spotify playlist
    #[effect(job, returns)]
    SpotifyRenamePlaylist {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("New name")]
        name: String,
    },
    /// Add to Spotify playlist
    #[effect(job, returns)]
    SpotifyPlaylistAdd {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Remove from Spotify playlist
    ///
    /// Removes every occurrence of the track from this Spotify playlist.
    #[effect(job, returns)]
    SpotifyPlaylistRemove {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Disconnect Spotify
    ///
    /// Forgets the local grant. Revoke Tilo in Spotify account settings to remove provider-side access.
    #[effect(job)]
    DisconnectSpotify,
    /// Browse Spotify
    #[effect(job)]
    BrowseSpotify,
    /// Browse `YouTube` Music
    #[effect(job)]
    BrowseYouTube,

    /// Search the remote catalogue
    /// Results join the canonical search index after the worker answers.
    #[effect(job, kind = "read")]
    Search { query: String },
    /// Inspect playback
    /// Device-local transport observations, with no account or credential data.
    #[effect(row, returns, idempotent, kind = "read")]
    PlaybackStatus,
    /// Open audio file
    ///
    /// Plays a local audio file the person chooses on this device.
    #[effect(attended, job, returns)]
    OpenFile,
    /// Open
    #[effect(view, row)]
    Open { place: Place },
    /// Open album
    #[effect(view, row)]
    OpenAlbum { album: String },
    /// Open artist
    #[effect(view, row)]
    OpenArtist { artist: String },
    /// Browse genre
    ///
    /// Opens the platform search for Music, prefilled with the genre.
    #[effect(view)]
    BrowseGenre {
        #[param("The genre to search for")]
        genre: String,
    },
    /// Play
    ///
    /// Plays a track on its own, or (with `queued`) this exact occurrence of
    /// it in the queue, which a stale queue result cannot select.
    #[effect(row, job, returns)]
    Play {
        track: String,
        queued: Option<usize>,
    },
    /// Like or unlike a catalogue song, importing its real metadata first.
    /// It changes only the library, through its edits.
    #[effect(row, edit)]
    ToggleLike { track: String },
    /// Like the song that plays
    ///
    /// Likes or unlikes whatever plays on this device when it runs, as
    /// liking that song's own row does.
    #[effect(row, edit)]
    ToggleLikePlaying,
    /// Add to queue
    #[effect(row)]
    AddToQueue { track: String },
    /// Play next
    #[effect(row, job, returns)]
    PlayNext { track: String },
    /// Keep offline
    ///
    /// Downloads the tracks' media to this device only.
    #[effect(row, job, cancellable, idempotent, returns)]
    KeepOffline { tracks: Vec<String> },
    /// Remove from offline cache
    #[effect(row)]
    RemoveOffline { tracks: Vec<String> },
    /// Play or pause
    #[effect(row, job, returns)]
    PlayPause,
    /// Previous track
    #[effect(row, job, returns)]
    Previous,
    /// Next queued track
    #[effect(row, job, returns)]
    Next,
    /// Toggle mute
    #[effect(row)]
    ToggleMute,
    /// Toggle shuffle
    #[effect(row)]
    ToggleShuffle,
    /// Change repeat mode
    #[effect(row)]
    CycleRepeat,
    /// Remove from queue
    #[effect(row, destructive)]
    Remove { index: usize, track: String },
    /// Move queue item up
    #[effect(row)]
    MoveUp { index: usize, track: String },
    /// Move queue item down
    #[effect(row)]
    MoveDown { index: usize, track: String },
    /// Seek playback
    Seek {
        #[param("absolute playback position in milliseconds")]
        position_ms: u64,
    },
    /// Set volume
    SetVolume {
        #[param("volume from 0 to 100")]
        volume: u8,
    },
    /// Add track to queue
    QueueAdd {
        #[param("stable music.track resource address")]
        track: String,
    },
    /// Remove track from queue
    #[effect(destructive)]
    QueueRemove {
        #[param("stable music.track resource address")]
        track: String,
    },
}

/// What invoking `effect` where it is bound sends its handler.
#[cfg(test)]
pub(crate) fn effect_payload(effect: &MusicEffect) -> Vec<u8> {
    effect
        .bound()
        .build_json(&serde_json::Map::new())
        .map(|encoded| encoded.payload)
        .unwrap_or_default()
}

/// `effect` bound to the row it acts on, under the row's own `label`.
pub(crate) fn bound(effect: &MusicEffect, label: &str) -> ActionSpec {
    let mut spec = effect.bound();
    label.clone_into(&mut spec.type_mut().label);
    spec
}

/// The only durable cover values: a short HTTPS URL, an empty value to clear,
/// or an app-generated PNG data URL whose decoded bytes fit the document budget.
pub fn portable_cover_value(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value.starts_with("https://") {
        return value.len() <= 2_048 && !value.chars().any(char::is_whitespace);
    }
    let Some(encoded) = value.strip_prefix("data:image/png;base64,") else {
        return false;
    };
    if encoded.is_empty() || value.len() > MAX_PORTABLE_COVER_BYTES.saturating_mul(2) {
        return false;
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .is_ok_and(|bytes| {
            bytes.len() <= MAX_PORTABLE_COVER_BYTES
                && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                && image::ImageReader::with_format(
                    std::io::Cursor::new(&bytes),
                    image::ImageFormat::Png,
                )
                .into_dimensions()
                .is_ok_and(|(width, height)| {
                    width > 0
                        && height > 0
                        && width <= MAX_PORTABLE_COVER_EDGE
                        && height <= MAX_PORTABLE_COVER_EDGE
                })
                // Header dimensions cap the decode allocation. Fully decode
                // afterwards so a PNG signature with malformed/truncated data
                // never becomes a durable cover peers cannot render.
                && image::load_from_memory(&bytes).is_ok()
        })
}

// ===========================================================================
// Rows
// ===========================================================================

/// The file this device has open outside any library.
#[derive(Debug, Clone)]
pub(crate) struct LocalFile {
    pub id: String,
    pub label: String,
}

/// What the window's rows are built from: the library, this device's
/// playback, and the facts about this device the library does not hold —
/// which songs it keeps offline and which artists it follows — beside the file
/// it has open, if any. The library's own rows (its places and the items of
/// its playlists) are published from [`crate::model::Library`] itself.
pub(crate) struct MusicRows<'a, D: DeviceFacts + ?Sized> {
    pub(crate) lib: &'a crate::model::Library,
    pub(crate) playback: &'a crate::model::Playback,
    /// What this device knows beyond the library.
    pub(crate) device: &'a D,
    pub(crate) local_file: Option<LocalFile>,
}

/// The facts about this device a row carries that the library does not hold.
pub(crate) trait DeviceFacts {
    /// Whether this device keeps a copy of the song offline.
    fn offline(&self, track: &str) -> bool;
    /// Whether this device follows the artist.
    fn followed(&self, artist: &str) -> bool;
}

/// A device that holds nothing offline and follows nobody.
#[cfg(test)]
pub(crate) struct PlainDevice;

#[cfg(test)]
impl DeviceFacts for PlainDevice {
    fn offline(&self, _: &str) -> bool {
        false
    }

    fn followed(&self, _: &str) -> bool {
        false
    }
}

/// This device's player, the row the transport's controls act on.
pub(crate) const PLAYER_RESOURCE_ID: &str = "music.player:player";

/// A device-local source has no collaborative track/catalog identity.
pub const LOCAL_FILE_RESOURCE_ID: &str = "music.local-file:current";

impl<D: DeviceFacts + ?Sized> MusicRows<'_, D> {
    /// The page: songs, artists and playlists of the library, then this
    /// device's player, queue and open file, each with what the window offers
    /// on it. Bounded; [`Self::row`] resolves the rest.
    pub(crate) fn all(&self) -> Vec<ui::framework::LiveRow> {
        let limit = OPEN_RESOURCE_CATEGORY_LIMIT;
        let mut playlists = self.lib.playlists().collect::<Vec<_>>();
        playlists.sort_by(|a, b| a.id.cmp(&b.id));
        let mut rows = playlists
            .into_iter()
            .take(limit)
            .map(|playlist| self.playlist(playlist))
            .collect::<Vec<_>>();
        let mut tracks = self.lib.tracks.keys().collect::<Vec<_>>();
        tracks.sort();
        rows.extend(tracks.into_iter().take(limit).map(|id| self.track(id)));
        let mut artists = self.lib.artists.keys().collect::<Vec<_>>();
        artists.sort();
        rows.extend(artists.into_iter().take(limit).map(|id| self.artist(id)));
        rows.push(self.player());
        rows.push(self.queue());
        if let Some(file) = &self.local_file {
            rows.push(self.file(file));
        }
        rows
    }

    /// The row `id` names, against the whole library rather than the page: a
    /// search hit or an agent may name any song, artist, playlist or
    /// membership, the player, the queue or an entry in it, or the open file.
    pub(crate) fn row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if id == LOCAL_FILE_RESOURCE_ID {
            return self.local_file.as_ref().map(|file| self.file(file));
        }
        if id == PLAYER_RESOURCE_ID {
            return Some(self.player());
        }
        if id == QUEUE_RESOURCE_ID {
            return Some(self.queue());
        }
        if let Some((index, track)) = parse_queue_item_resource_id(id) {
            if self.playback.queue.get(index).map(String::as_str) != Some(track.as_str()) {
                return None;
            }
            return Some(self.queue_item(index, &track));
        }
        if let Some((playlist, track)) = parse_playlist_track_resource_id(id) {
            let playlist = self.lib.playlist(&playlist)?;
            if !playlist.contains(&track) {
                return None;
            }
            self.lib.track(&track)?;
            return Some(self.membership(&playlist.id, &track));
        }
        if let Some(track) = track_id_from_resource(id) {
            self.lib.track(track)?;
            return Some(self.track(track));
        }
        if let Some(artist) = id.strip_prefix("music.artist:").filter(|id| !id.is_empty()) {
            self.lib.artist(artist)?;
            return Some(self.artist(artist));
        }
        let playlist = parse_playlist_resource_id(id)?;
        Some(self.playlist(self.lib.playlist(&playlist)?))
    }

    /// The song's title, then who performs it.
    fn track_title(&self, id: &str) -> String {
        self.lib
            .track(id)
            .map_or_else(|| id.to_owned(), |track| track.title.clone())
    }

    /// Who performs the song, the row's secondary line: empty when unknown.
    fn track_artist(&self, id: &str) -> String {
        self.lib
            .track(id)
            .map(|track| self.lib.artist_name(&track.artist_id).to_owned())
            .unwrap_or_default()
    }

    /// A song of the library, with what this device can do with it: play it,
    /// keep a copy offline, like it, queue it, and open its album or artist.
    fn track(&self, id: &str) -> ui::framework::LiveRow {
        let artist = self.track_artist(id);
        let label = if artist.is_empty() {
            self.track_title(id)
        } else {
            format!("{} — {artist}", self.track_title(id))
        };
        let tracks = vec![id.to_owned()];
        let mut row = ui::framework::LiveRow::new(track_resource_id(id), "music.track", label)
            .operation(bound(
                &MusicEffect::Play {
                    track: id.to_owned(),
                    queued: None,
                },
                "Play track",
            ))
            .operation(if id.starts_with("spotify:") {
                MusicEffect::KeepOffline { tracks }.bound().unavailable(
                    "spotify_offline_unsupported",
                    "Spotify protected audio cannot be downloaded through Tilo",
                )
            } else if self.device.offline(id) {
                MusicEffect::RemoveOffline { tracks }.bound()
            } else {
                MusicEffect::KeepOffline { tracks }.bound()
            })
            .operation(like_operation(id, self.lib.is_liked(id)))
            .effect(&MusicEffect::AddToQueue {
                track: id.to_owned(),
            })
            .effect(&MusicEffect::PlayNext {
                track: id.to_owned(),
            });
        if id.starts_with("spotify:track:") {
            row = row
                .effect(&MusicEffect::SpotifySave { uri: id.to_owned() })
                .effect(&MusicEffect::SpotifyUnsave { uri: id.to_owned() });
        }
        if let Some(song) = self.lib.track(id) {
            if !song.album_id.is_empty() {
                row = row.effect(&MusicEffect::OpenAlbum {
                    album: song.album_id.clone(),
                });
            }
            if !song.artist_id.is_empty() {
                row = row.effect(&MusicEffect::OpenArtist {
                    artist: song.artist_id.clone(),
                });
            }
        }
        row
    }

    /// An artist of the library, and on Spotify whether this device follows
    /// them.
    fn artist(&self, id: &str) -> ui::framework::LiveRow {
        let name = self
            .lib
            .artist(id)
            .map_or_else(|| id.to_owned(), |artist| artist.name.clone());
        let row = ui::framework::LiveRow::new(artist_resource_id(id), "music.artist", name).effect(
            &MusicEffect::Open {
                place: Place::Artist(id.to_owned()),
            },
        );
        if !id.starts_with("spotify:artist:") {
            return row;
        }
        let followed = self.device.followed(id);
        row.effect(&if followed {
            MusicEffect::SpotifyUnsave { uri: id.to_owned() }
        } else {
            MusicEffect::SpotifySave { uri: id.to_owned() }
        })
    }

    /// A playlist as the library holds it, with its first few songs: its row
    /// is the item's, here adding what opens it.
    fn playlist(&self, playlist: &crate::model::Playlist) -> ui::framework::LiveRow {
        let mut row = ui::framework::LiveRow::new(
            playlist_resource_id(&playlist.id),
            "music.playlist",
            playlist_label(playlist),
        )
        .operation(bound(
            &MusicEffect::Open {
                place: Place::Playlist(playlist.id.clone()),
            },
            "Open playlist",
        ));
        let tracks = playlist
            .tracks
            .values()
            .take(OPEN_PLAYLIST_TRACK_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        for track in &tracks {
            row = row.row(self.membership(&playlist.id, track));
        }
        row
    }

    /// One song in a playlist: opening it there shows that membership, and
    /// liking it likes the song itself.
    fn membership(&self, playlist: &str, track: &str) -> ui::framework::LiveRow {
        let artist = self.track_artist(track);
        let mut row = ui::framework::LiveRow::new(
            playlist_track_resource_id(playlist, track),
            "music.playlist-track",
            self.track_title(track),
        )
        .operation(bound(
            &MusicEffect::Open {
                place: Place::Membership {
                    playlist: playlist.to_owned(),
                    track: track.to_owned(),
                },
            },
            "Open in playlist",
        ))
        .operation(like_operation(track, self.lib.is_liked(track)));
        if !artist.is_empty() {
            row = row.presentation(tilo_actions::ResourcePresentation {
                detail: Some(artist),
                ..tilo_actions::ResourcePresentation::default()
            });
        }
        row
    }

    /// This device's player: the transport's controls, each offered (those
    /// that cannot act right now with the reason why).
    fn player(&self) -> ui::framework::LiveRow {
        transport(
            ui::framework::LiveRow::new(PLAYER_RESOURCE_ID, "music.player", "Player"),
            self.playback,
        )
    }

    /// This device's queue and its first few entries.
    fn queue(&self) -> ui::framework::LiveRow {
        let mut row = ui::framework::LiveRow::new(QUEUE_RESOURCE_ID, "music.queue", "Queue")
            .operation(open_queue());
        for (index, track) in self
            .playback
            .queue
            .iter()
            .take(OPEN_RESOURCE_CATEGORY_LIMIT)
            .enumerate()
        {
            row = row.row(self.queue_item(index, track));
        }
        row
    }

    /// One occurrence in the queue: a song may be queued twice, so the row is
    /// its position there, not the song.
    fn queue_item(&self, index: usize, track: &str) -> ui::framework::LiveRow {
        let artist = self.track_artist(track);
        let total = self.playback.queue.len();
        let move_up = MusicEffect::MoveUp {
            index,
            track: track.to_owned(),
        }
        .bound();
        let move_down = MusicEffect::MoveDown {
            index,
            track: track.to_owned(),
        }
        .bound();
        let mut row = ui::framework::LiveRow::new(
            queue_item_resource_id(index, track),
            "music.queue-item",
            self.track_title(track),
        )
        .operation(open_queue())
        .operation(bound(
            &MusicEffect::Play {
                track: track.to_owned(),
                queued: Some(index),
            },
            if self.playback.current.as_deref() == Some(track) && self.playback.queue_index == index
            {
                "Restart track"
            } else {
                "Play queue item"
            },
        ))
        .operation(
            MusicEffect::Remove {
                index,
                track: track.to_owned(),
            }
            .bound(),
        )
        .operation(if index > 0 {
            move_up
        } else {
            move_up.unavailable(
                "first_queue_item",
                "This item is already first in the queue",
            )
        })
        .operation(if index + 1 < total {
            move_down
        } else {
            move_down.unavailable("last_queue_item", "This item is already last in the queue")
        });
        if !artist.is_empty() {
            row = row.presentation(tilo_actions::ResourcePresentation {
                detail: Some(artist),
                ..tilo_actions::ResourcePresentation::default()
            });
        }
        row
    }

    /// The file this device has open: the transport when it is what plays,
    /// else what starts it.
    fn file(&self, file: &LocalFile) -> ui::framework::LiveRow {
        let row = ui::framework::LiveRow::new(
            LOCAL_FILE_RESOURCE_ID,
            "music.local-file",
            file.label.clone(),
        );
        if self.playback.current.as_deref() == Some(file.id.as_str()) {
            return transport(row, self.playback);
        }
        row.operation(bound(
            &MusicEffect::Play {
                track: file.id.clone(),
                queued: None,
            },
            "Play file",
        ))
    }
}

fn playlist_label(playlist: &crate::model::Playlist) -> String {
    let name = playlist.name.trim();
    if name.is_empty() {
        "Untitled playlist".to_owned()
    } else {
        name.to_owned()
    }
}

/// Liking or unliking `track`, which is `liked` now.
fn like_operation(track: &str, liked: bool) -> ActionSpec {
    bound(
        &MusicEffect::ToggleLike {
            track: track.to_owned(),
        },
        if liked { "Unlike song" } else { "Like song" },
    )
}

/// `spec` refusing with `unavailable`, when there is a reason it cannot act.
fn with_unavailable(
    spec: ActionSpec,
    unavailable: Option<(&'static str, &'static str)>,
) -> ActionSpec {
    match unavailable {
        Some((code, reason)) => spec.unavailable(code, reason),
        None => spec,
    }
}

/// The transport's controls on `row`: the player's, or the file that plays.
fn transport(
    row: ui::framework::LiveRow,
    playback: &crate::model::Playback,
) -> ui::framework::LiveRow {
    let mut row = row.effect(&MusicEffect::PlaybackStatus);
    let play = bound(
        &MusicEffect::PlayPause,
        if playback.playing { "Pause" } else { "Play" },
    );
    row = row.operation(if playback.current.is_some() {
        play
    } else {
        play.unavailable(
            "nothing_playing",
            "Choose a track before using playback controls",
        )
    });
    row = row
        .effect(&MusicEffect::ToggleMute)
        .operation(bound(
            &MusicEffect::ToggleShuffle,
            if playback.shuffle {
                "Turn shuffle off"
            } else {
                "Turn shuffle on"
            },
        ))
        .effect(&MusicEffect::CycleRepeat);
    let like = MusicEffect::ToggleLikePlaying.bound();
    row = row.operation(if playback.current.is_some() {
        like
    } else {
        like.unavailable("nothing_playing", "Choose a track to like")
    });
    let unavailable = if playback.current.is_none() {
        Some((
            "nothing_playing",
            "Choose a track before using playback controls",
        ))
    } else if playback.queue.is_empty() {
        Some(("empty_queue", "There are no queued tracks"))
    } else {
        None
    };
    // Next steps through this device's own shuffle order while shuffle is
    // on, as the transport's button does; the order stays device-local and
    // never reaches the shared library.
    row.operation(with_unavailable(MusicEffect::Previous.bound(), unavailable))
        .operation(with_unavailable(MusicEffect::Next.bound(), unavailable))
}

fn open_queue() -> ActionSpec {
    bound(
        &MusicEffect::Open {
            place: Place::View("queue".into()),
        },
        "Open queue",
    )
}

/// One song joining or leaving a queue, at its position there. A queue can
/// hold the same song twice side by side, so only the edit itself says which
/// of the two moved, and with it which one a shuffled order has already
/// played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueEdit {
    /// A song joined at this position, to be heard after the songs still to
    /// come.
    Added(usize),
    /// A song joined at this position, right after the current one, to be
    /// heard next.
    PutNext(usize),
    /// The song at this position left.
    Removed(usize),
}

/// The playback `effect` leads to from `playback` on this device, for the
/// transport and queue effects that only change it, with the queue edit it
/// made; `None` for any other effect. `known` says whether a track can be
/// played here.
pub(crate) fn next_playback(
    effect: &MusicEffect,
    playback: &crate::model::Playback,
    known: &dyn Fn(&str) -> bool,
) -> Option<Result<(crate::model::Playback, Option<QueueEdit>), String>> {
    let mut next = playback.clone();
    let mut edit = None;
    let known_track = |address: &str| {
        let track = track_id_from_resource(address)
            .ok_or_else(|| "track must be a music.track resource".to_string())?;
        if known(track) {
            Ok(track.to_owned())
        } else {
            Err("track is no longer in the library".to_string())
        }
    };
    let result = match effect {
        MusicEffect::PlayPause => {
            if next.current.is_none() {
                return Some(Err("nothing is playing".into()));
            }
            next.playing = !next.playing;
            Ok(())
        }
        MusicEffect::Previous => {
            if next.queue_index > 0 {
                next.queue_index -= 1;
                next.current = next.queue.get(next.queue_index).cloned();
            }
            next.position_ms = 0;
            Ok(())
        }
        MusicEffect::Next => {
            if next.queue_index + 1 < next.queue.len() {
                next.queue_index += 1;
                next.current = next.queue.get(next.queue_index).cloned();
                next.position_ms = 0;
            } else if next.repeat == crate::model::RepeatMode::All && !next.queue.is_empty() {
                next.queue_index = 0;
                next.current = next.queue.first().cloned();
                next.position_ms = 0;
            } else {
                next.playing = false;
            }
            Ok(())
        }
        MusicEffect::ToggleMute => {
            next.volume = if next.volume == 0 { 80 } else { 0 };
            Ok(())
        }
        MusicEffect::ToggleShuffle => {
            next.shuffle = !next.shuffle;
            Ok(())
        }
        MusicEffect::CycleRepeat => {
            next.repeat = next.repeat.next();
            Ok(())
        }
        MusicEffect::Seek { position_ms } => {
            next.position_ms = *position_ms;
            Ok(())
        }
        MusicEffect::SetVolume { volume } => {
            next.volume = (*volume).min(100);
            Ok(())
        }
        MusicEffect::AddToQueue { track } => {
            if known(track) {
                edit = Some(QueueEdit::Added(next.queue.len()));
                next.queue.push(track.clone());
                Ok(())
            } else {
                Err("track is no longer in the library".into())
            }
        }
        MusicEffect::PlayNext { track } => {
            if !known(track) {
                return Some(Err("track is no longer in the library".into()));
            }
            if next.queue.is_empty() || next.current.is_none() {
                next.queue = vec![track.clone()];
                next.current = Some(track.clone());
                next.queue_index = 0;
                next.playing = true;
                next.position_ms = 0;
            } else {
                let at = (next.queue_index + 1).min(next.queue.len());
                next.queue.insert(at, track.clone());
                edit = Some(QueueEdit::PutNext(at));
            }
            Ok(())
        }
        MusicEffect::QueueAdd { track } => known_track(track).map(|track| {
            edit = Some(QueueEdit::Added(next.queue.len()));
            next.queue.push(track);
        }),
        MusicEffect::QueueRemove { track } => known_track(track)
            .and_then(|track| remove_from_queue(&mut next, &track))
            .map(|index| edit = Some(QueueEdit::Removed(index))),
        _ => return None,
    };
    Some(result.map(|()| (next, edit)))
}

/// Take `track`'s first occurrence out of `playback`'s queue, answering
/// where it was; removing the current one moves on to what follows it, or
/// stops at the end.
fn remove_from_queue(playback: &mut crate::model::Playback, track: &str) -> Result<usize, String> {
    let index = playback
        .queue
        .iter()
        .position(|queued| queued == track)
        .ok_or_else(|| "track is not in the current queue".to_string())?;
    playback.queue.remove(index);
    if index < playback.queue_index {
        playback.queue_index -= 1;
    } else if index == playback.queue_index {
        if playback.queue.is_empty() {
            playback.current = None;
            playback.playing = false;
            playback.queue_index = 0;
        } else {
            playback.queue_index = playback.queue_index.min(playback.queue.len() - 1);
            playback.current = playback.queue.get(playback.queue_index).cloned();
        }
        playback.position_ms = 0;
    }
    Ok(index)
}

fn track_id_from_resource(address: &str) -> Option<&str> {
    address
        .strip_prefix("music.track:")
        .filter(|id| !id.is_empty())
}

/// An album, artist or playlist the live catalogue found and the library
/// does not hold, labelled `label`, as the row `id`: opening its page is all
/// it offers. `None` for any other row.
pub fn live_catalogue_row(id: &str, label: &str) -> Option<ui::framework::LiveRow> {
    let kind = if id.starts_with("music.album:") {
        "music.album"
    } else if id.starts_with("music.artist:") {
        "music.artist"
    } else if id.starts_with("music.catalogue-playlist:") {
        "music.playlist"
    } else {
        return None;
    };
    let effect = anchor_effect(id)?;
    Some(ui::framework::LiveRow::new(id, kind, label).operation(bound(&effect, "Open")))
}

#[cfg(test)]
impl crate::app::Music {
    /// What the palette and agents see of `doc` in a window showing this
    /// surface: the library as the framework publishes it, with the
    /// surface's rows merged in by id as a publication does.
    pub(crate) fn palette_tree(
        &self,
        doc: &automerge::AutoCommit,
    ) -> Vec<tilo_actions::ResourceSpec> {
        use ui::tilo_resource::Resource as _;
        let state = crate::model::Library::read(doc).unwrap_or_default();
        let mut root =
            ui::tilo_resource::publish::<crate::model::Library>("music.library", "Library", &state);
        for row in self.device_rows() {
            let row = row.into_resource();
            match root.children.iter_mut().find(|child| child.id == row.id) {
                Some(item) => {
                    item.presentation = item.presentation.take().or(row.presentation);
                    for action in row.actions {
                        if !item.actions.iter().any(|known| known.id == action.id) {
                            item.actions.push(action);
                        }
                    }
                    item.children.extend(row.children);
                }
                None => root.children.push(row),
            }
        }
        vec![root]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Music;
    use crate::model::{Library, Playlist};
    use automerge::AutoCommit;
    use tilo_actions::ActionKind;
    use tilo_actions::{
        ActionRegistry, ArgValue, ExecutionMode, OperationKind, ParamType, ResourceSpec, ResultSpec,
    };
    use ui::tilo_resource::{Edit, Id, Invoke, Replica, Resource as _};

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        Library::register(&mut registry);
        registry
    }

    fn tiny_portable_png() -> String {
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(output.into_inner())
        )
    }

    /// A Music app with the sample library synced into a fresh document.
    fn seeded() -> (Music, AutoCommit) {
        let mut app = Music::new();
        let doc = crate::seed::sample_document();
        ui::widget::Widget::sync(&mut app, &doc, &automerge::ROOT);
        (app, doc)
    }

    /// The sample library behind the door the host uses.
    fn library() -> Replica<Library> {
        let mut library = Replica::<Library>::new();
        for action in crate::seed::sample_actions() {
            library.apply(action).unwrap();
        }
        library
    }

    fn edit(library: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
        library
            .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn first_playlist(library: &Replica<Library>) -> Playlist {
        library
            .state()
            .unwrap()
            .playlists()
            .next()
            .cloned()
            .expect("a sample playlist")
    }

    fn spec_encoding_of(spec: &ActionSpec) -> tilo_actions::EncodedAction {
        ui::testing::action_contract::spec_encoding(spec, None)
    }

    /// A song of `id`, with nothing else recorded about it.
    fn track(id: &str) -> crate::model::Track {
        crate::model::Track {
            id: id.to_owned(),
            title: id.to_owned(),
            album_id: String::new(),
            artist_id: String::new(),
            track_no: 0,
            duration_s: 0,
            yt_id: String::new(),
            plays: 0,
        }
    }

    static PLAIN: crate::actions::PlainDevice = crate::actions::PlainDevice;

    /// The rows a window with no offline copies and no followed artists
    /// builds from `lib` and `playback`.
    fn music_rows<'a>(
        lib: &'a Library,
        playback: &'a crate::model::Playback,
    ) -> crate::actions::MusicRows<'a, crate::actions::PlainDevice> {
        crate::actions::MusicRows {
            lib,
            playback,
            device: &PLAIN,
            local_file: None,
        }
    }

    /// The library as the framework publishes it: its places and the items
    /// of its playlists.
    fn published(lib: &Library) -> ResourceSpec {
        ui::tilo_resource::publish::<Library>("music.library", "Library", lib)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn library_resources_publish_bounded_safe_canonical_views() {
        // Phase `ctrl_k_publishes_the_library_tree`.
        // The library offers making a playlist; each playlist is an item that
        // offers deleting it, and the window's row adds opening it and its
        // tracks; tracks, albums and artists carry their effects.
        {
            let (app, doc) = seeded();
            let resources = app.palette_tree(&doc);
            assert_eq!(resources.len(), 1, "one library root");
            let lib = &resources[0];
            assert_eq!(lib.kind, "music.library");
            let lib_actions: Vec<&str> = lib.actions.iter().map(|a| a.id.as_str()).collect();
            for id in ["create-playlist", "import-track", "field-edit"] {
                assert!(lib_actions.contains(&id), "{id} in {lib_actions:?}");
            }

            let playlist = lib
                .children
                .iter()
                .find(|r| r.id.as_str() == playlist_resource_id("pl_focus"))
                .expect("a playlist item");
            let pl_actions: Vec<&str> = playlist.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(pl_actions.contains(&"delete-playlist"), "{pl_actions:?}");
            assert!(pl_actions.contains(&"open"), "{pl_actions:?}");
            assert_eq!(
                playlist
                    .actions
                    .iter()
                    .find(|a| a.id == "delete-playlist")
                    .unwrap()
                    .contract
                    .risk,
                tilo_actions::RiskLevel::Destructive
            );

            let player = lib
                .children
                .iter()
                .find(|r| r.kind == "music.player")
                .expect("a stable player resource");
            assert!(
                MusicEffect::specs()
                    .iter()
                    .any(|action| action.id == "queue-add")
            );
            assert!(
                player
                    .actions
                    .iter()
                    .any(|action| action.id == "toggle-shuffle")
            );

            // A playlist with tracks nests a membership resource that opens it
            // in the playlist and likes the song. It is not a duplicate of the
            // catalog track entity.
            let track = &playlist.children[0];
            assert_eq!(track.kind, "music.playlist-track");
            let tk_actions: Vec<&str> = track.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(tk_actions.contains(&"open"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"toggle-like"), "{tk_actions:?}");

            for kind in ["music.album", "music.artist", "music.track"] {
                let row = lib
                    .children
                    .iter()
                    .find(|r| r.kind == kind)
                    .unwrap_or_else(|| panic!("a {kind} row"));
                assert!(
                    row.actions
                        .iter()
                        .all(|action| action.kind != OperationKind::Document),
                    "{kind} rows carry what this window does"
                );
            }

            let flat: Vec<&str> = tilo_actions::flatten(&resources)
                .iter()
                .map(|f| f.action.label.as_str())
                .collect();
            assert!(flat.contains(&"Create a new playlist"), "{flat:?}");
            assert!(flat.contains(&"Delete a playlist"), "{flat:?}");
            assert!(flat.contains(&"Open in playlist"), "{flat:?}");
        }

        {
            let mut lib = Library::default();
            for number in 0..(OPEN_RESOURCE_CATEGORY_LIMIT + 9) {
                let id = format!("track-{number:03}");
                lib.tracks.insert(id.clone(), track(&id));
            }
            let playback = crate::model::Playback::default();
            assert!(
                !published(&lib)
                    .children
                    .iter()
                    .any(|resource| resource.kind == "music.track"),
                "a song is this device's row, not the library document's"
            );
            let rows = music_rows(&lib, &playback).all();
            assert_eq!(
                rows.iter()
                    .filter(|row| row.id().starts_with("music.track:"))
                    .count(),
                OPEN_RESOURCE_CATEGORY_LIMIT,
                "opening Ctrl+K never clones every catalog track"
            );

            let late_id = track_resource_id("track-024");
            assert!(
                !rows.iter().any(|row| row.id() == late_id),
                "the late track is intentionally absent from the contextual page"
            );
            let late = music_rows(&lib, &playback)
                .row(&late_id)
                .expect("late track resolves by stable id")
                .into_resource();
            assert_eq!(late.id.as_str(), late_id);
            assert!(late.actions.iter().any(|action| action.id == "play"));
            assert!(
                music_rows(&lib, &playback)
                    .row("music.track:deleted")
                    .is_none(),
                "a stale address never resolves into new authority"
            );
        }

        {
            let mut lib = Library::default();
            let mut song = track("track-1");
            song.album_id = "album-9".into();
            song.artist_id = "artist-2".into();
            lib.tracks.insert(song.id.clone(), song);
            let playback = crate::model::Playback::default();
            let resource = music_rows(&lib, &playback)
                .row(&track_resource_id("track-1"))
                .expect("a song of the library")
                .into_resource();
            let album = resource
                .actions
                .iter()
                .find(|action| action.id == "open-album")
                .expect("track has an exact album action");
            let artist = resource
                .actions
                .iter()
                .find(|action| action.id == "open-artist")
                .expect("track has an exact artist action");
            for (action, wanted) in [(album, "album-9"), (artist, "artist-2")] {
                let encoded = spec_encoding_of(action);
                match MusicEffect::from_payload(&encoded.payload).expect("effect decodes") {
                    MusicEffect::OpenAlbum { album: id }
                    | MusicEffect::OpenArtist { artist: id } => {
                        assert_eq!(id, wanted);
                    }
                    other => panic!("unexpected parent action: {other:?}"),
                }
            }
        }

        {
            let (app, doc) = seeded();
            let resources = app.palette_tree(&doc);
            for flat in tilo_actions::flatten(&resources) {
                let permission = flat.action.contract.kind;
                match flat.action.kind {
                    OperationKind::Document => assert_eq!(
                        permission,
                        ActionKind::Undoable,
                        "{} must require write permission",
                        flat.key
                    ),
                    // A view shows, or edits the library through its edits
                    // (liking a song); it never reaches outside it.
                    OperationKind::View => assert!(
                        matches!(permission, ActionKind::Read | ActionKind::Undoable),
                        "{} must stay inside the document",
                        flat.key
                    ),
                    OperationKind::Effect => {}
                }
            }
        }

        {
            let lib = Library::default();
            let root = published(&lib);
            for &(id, label) in NAVIGATION_VIEWS {
                let resource_id = view_resource_id(id);
                let resource = root
                    .children
                    .iter()
                    .find(|resource| resource.id.as_str() == resource_id)
                    .expect("published top-level destination");
                assert_eq!(resource.kind, "music.view");
                assert_eq!(resource.label, label);
                let open = resource
                    .actions
                    .iter()
                    .find(|action| action.id == "open")
                    .expect("canonical open action");
                assert_eq!(open.kind, OperationKind::View);
                assert_eq!(open.contract.kind, ActionKind::Read);
                let encoded = spec_encoding_of(open);
                assert_eq!(
                    MusicEffect::from_payload(&encoded.payload).expect("effect decodes"),
                    MusicEffect::Open {
                        place: Place::View(id.to_string())
                    }
                );
                assert_eq!(
                    anchor_effect(&resource_id),
                    Some(MusicEffect::Open {
                        place: Place::View(id.to_string())
                    }),
                    "opening the place's anchor shows it"
                );
                assert_eq!(
                    <Library as ui::tilo_resource::Resource>::child(&lib, &resource_id)
                        .map(|row| row.id.as_str().to_string()),
                    Some(resource_id)
                );
            }
            assert!(parse_view_resource_id("music.view:../../etc/passwd").is_none());
            assert!(anchor_effect("music.view:../../etc/passwd").is_none());
        }

        {
            let (app, doc) = seeded();
            let resources = app.palette_tree(&doc);
            let operation_handlers =
                std::collections::HashSet::from([MusicEffect::OPERATION.to_string()]);
            let violations =
                tilo_actions::validate_surface(&resources, &registry(), &operation_handlers);
            assert!(violations.is_empty(), "{violations:#?}");
        }
    }

    #[test]
    fn playlist_creation_and_covers_are_portable_and_validated() {
        {
            let mut a = library();
            let mut b = a.fork();
            let playlist = first_playlist(&a).id;
            let cover = tiny_portable_png();
            assert!(portable_cover_value(&cover));
            assert!(!cover.contains("file:"));
            assert!(!cover.contains("/tmp/"));

            edit(
                &mut a,
                &Library::playlist_paths(&playlist)
                    .cover
                    .replace(cover.clone()),
            )
            .unwrap();
            a.sync_with(&mut b).unwrap();
            assert_eq!(a.state().unwrap(), b.state().unwrap(), "cover converges");
            assert_eq!(
                b.state().unwrap().playlist(&playlist).unwrap().cover,
                cover,
                "the peer receives a portable value, not the picker's local path"
            );
        }

        {
            let mut library = library();
            let playlist = first_playlist(&library).id;
            let oversized = format!(
                "data:image/png;base64,{}",
                "A".repeat(MAX_PORTABLE_COVER_BYTES.saturating_mul(2))
            );
            for cover in [
                "file:///tmp/cover.png",
                "/tmp/cover.png",
                "http://example.test/cover.png",
                "data:image/png;base64,not-valid-base64",
                "data:image/png;base64,iVBORw0KGgo=",
                "data:image/jpeg;base64,iVBORw0KGgo=",
                oversized.as_str(),
            ] {
                assert!(
                    edit(
                        &mut library,
                        &Library::playlist_paths(&playlist).cover.replace(cover)
                    )
                    .is_err(),
                    "must refuse hostile/nonportable cover {cover:?}"
                );
            }
        }

        {
            let mut library = library();
            let before = library.state().unwrap().playlists.len();
            library
                .apply(
                    Library::CREATE_PLAYLIST.with(crate::resource::library::CreatePlaylist {
                        name: " Late Night ".into(),
                        track: Some("tk_g1".into()),
                    }),
                )
                .unwrap();
            let lib = library.state().unwrap();
            assert_eq!(lib.playlists.len(), before + 1, "the verb added a playlist");
            let created = lib
                .playlists()
                .find(|p| p.name == "Late Night")
                .expect("the created playlist carries the trimmed name");
            assert_eq!(created.track_ids(), ["tk_g1"]);
            assert!(
                library
                    .apply(Library::CREATE_PLAYLIST.with(
                        crate::resource::library::CreatePlaylist {
                            name: "Ghost".into(),
                            track: Some("tk_gone".into()),
                        }
                    ))
                    .is_err(),
                "a playlist starts only with a song of the library"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn playback_and_queue_arguments_are_typed_canonical_and_fresh() {
        {
            let playback = crate::model::Playback {
                current: Some("track-1".into()),
                queue: vec!["track-1".into()],
                ..crate::model::Playback::default()
            };
            let player = music_rows(&Library::default(), &playback)
                .player()
                .into_resource();
            // The transport rides the player; what takes a caller's argument
            // is offered on the library.
            let library = MusicEffect::specs();
            for (id, on) in [
                ("play-pause", &player.actions),
                ("seek", &library),
                ("set-volume", &library),
                ("queue-add", &library),
                ("queue-remove", &library),
            ] {
                let action = on.iter().find(|action| action.id == id).unwrap();
                assert_eq!(action.kind, tilo_actions::OperationKind::Effect, "{id}");
                assert_eq!(
                    action.contract.kind,
                    ActionKind::Outside,
                    "this device's audio is outside the document: {id}"
                );
                assert!(
                    !action.contract.can_be_undone(),
                    "local device effects cannot claim an executable undo: {id}"
                );
            }
            assert_eq!(
                library.iter().find(|a| a.id == "seek").unwrap().params()[0].ty,
                ParamType::Integer
            );
            for (effect, kind) in [
                (
                    MusicEffect::ToggleLike {
                        track: "track-1".into(),
                    },
                    ActionKind::Undoable,
                ),
                (
                    MusicEffect::Open {
                        place: Place::View("songs".into()),
                    },
                    ActionKind::Read,
                ),
                (
                    MusicEffect::OpenAlbum {
                        album: "album-1".into(),
                    },
                    ActionKind::Read,
                ),
            ] {
                assert_eq!(effect.bound().contract.kind, kind, "{effect:?}");
            }
            let mut lib = Library::default();
            lib.tracks.insert("track-2".into(), track("track-2"));
            let song = music_rows(&lib, &playback)
                .row(&track_resource_id("track-2"))
                .expect("a song of the library")
                .into_resource();
            let play = song
                .actions
                .iter()
                .find(|action| action.id == "play")
                .unwrap();
            assert_eq!(
                play.contract.execution,
                ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(
                play.contract.result,
                ResultSpec::Json {
                    schema: serde_json::json!({}),
                }
            );
            let offline = song
                .actions
                .iter()
                .find(|action| action.id == "keep-offline")
                .unwrap();
            assert_eq!(
                offline.contract.execution,
                ExecutionMode::Background { cancellable: true },
                "offline transfer is a bounded cancellable device effect"
            );
        }

        {
            let playback = crate::model::Playback {
                queue: vec!["track-1".into(), "track-1".into(), "track-2".into()],
                current: Some("track-1".into()),
                ..crate::model::Playback::default()
            };
            let queue = music_rows(&Library::default(), &playback)
                .queue()
                .into_resource();
            assert_eq!(queue.kind, "music.queue");
            assert_eq!(queue.children.len(), 3);
            assert_ne!(queue.children[0].id, queue.children[1].id);
            assert_eq!(
                parse_queue_item_resource_id(queue.children[1].id.as_str()),
                Some((1, "track-1".into())),
                "a duplicate track retains an occurrence-specific address"
            );
            let remove = queue.children[1]
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .expect("queue rows are actionable without pixels");
            assert_eq!(
                MusicEffect::from_payload(&spec_encoding_of(remove).payload).unwrap(),
                MusicEffect::Remove {
                    index: 1,
                    track: "track-1".into(),
                }
            );
            assert!(
                parse_queue_item_resource_id("music.queue-item:v1:not-an-index:dHJhY2stMQ")
                    .is_none(),
                "malformed occurrence ids cannot acquire queue authority"
            );
        }

        {
            let (mut app, _doc) = seeded();
            let effects = MusicEffect::specs();
            let queue_add = effects
                .iter()
                .find(|action| action.id == "queue-add")
                .unwrap();
            for address in ["music.album:not-a-track", "music.track:removed"] {
                let encoded = queue_add
                    .build_args([(String::from("track"), ArgValue::Text(address.into()))])
                    .expect("the address binds before Music validates its kind and state");
                assert!(
                    app.start_effect(tilo_actions::InvocationId::new(), &encoded.payload)
                        .is_err(),
                    "{address}"
                );
            }
            let mut library = library();
            let playlist = first_playlist(&library).id;
            let stale = Library::playlist_paths(&playlist).tracks.insert_as(
                &Id::from_raw("tk_removed"),
                0,
                &"tk_removed".to_owned(),
            );
            assert!(
                edit(&mut library, &stale).is_err(),
                "a playlist holds only songs of the library"
            );
        }

        {
            let mut library = library();
            let gone = Library::playlist_paths("gone").tracks.insert_as(
                &Id::from_raw("tk_g1"),
                0,
                &"tk_g1".to_owned(),
            );
            assert!(edit(&mut library, &gone).is_err());
            assert!(
                library
                    .apply(Library::DELETE_PLAYLIST.with(Id::from_raw("gone")))
                    .is_err()
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn playlist_membership_rows_resolve_lazily_and_edits_converge() {
        {
            let mut lib = Library::default();
            let mut songs = ui::tilo_resource::Collection::new();
            for number in 0..(OPEN_PLAYLIST_TRACK_LIMIT + 8) {
                let id = format!("track:{number}:🎧");
                songs.push(Id::from_raw(id.as_str()), id.clone());
                lib.tracks.insert(id.clone(), track(&id));
            }
            let target = songs.values().last().cloned().expect("late membership");
            lib.playlists.push(
                Id::from_raw("playlist:日本語"),
                crate::model::Playlist {
                    id: "playlist:日本語".into(),
                    name: "Long playlist".into(),
                    description: String::new(),
                    tracks: songs,
                    cover: String::new(),
                },
            );
            let playback = crate::model::Playback::default();
            let rows = music_rows(&lib, &playback);
            let playlist_row = rows
                .playlist(lib.playlist("playlist:日本語").expect("the playlist"))
                .into_resource();
            assert_eq!(
                playlist_row.children.len(),
                OPEN_PLAYLIST_TRACK_LIMIT,
                "the contextual page does not materialize every membership"
            );
            assert_eq!(
                parse_playlist_resource_id(&playlist_resource_id("playlist:日本語")),
                Some("playlist:日本語".to_owned())
            );
            let id = playlist_track_resource_id("playlist:日本語", &target);
            assert_eq!(
                parse_playlist_track_resource_id(&id),
                Some(("playlist:日本語".to_owned(), target.clone())),
                "membership identity is Unicode-safe and reversible"
            );
            let direct = rows
                .row(&id)
                .expect("late membership resolves directly")
                .into_resource();
            let open = direct
                .actions
                .iter()
                .find(|action| action.id == "open")
                .unwrap();
            assert_eq!(
                MusicEffect::from_payload(&spec_encoding_of(open).payload).unwrap(),
                MusicEffect::Open {
                    place: Place::Membership {
                        playlist: "playlist:日本語".to_owned(),
                        track: target,
                    }
                },
                "the lazy row opens the exact membership"
            );
        }

        {
            let mut library = library();
            let playlist = first_playlist(&library);
            let track = playlist.track_ids()[0].clone();
            let remove = Library::remove_from_playlist(&playlist.id, &track);
            edit(&mut library, &remove).expect("live membership removes");
            assert!(
                edit(&mut library, &remove).is_err(),
                "a removed song is gone"
            );

            let playlist = first_playlist(&library);
            let track = playlist.track_ids()[0].clone();
            let moved = Library::move_in_playlist(&playlist.id, &track, 1);
            edit(&mut library, &moved).expect("live membership moves");
            assert_eq!(first_playlist(&library).track_ids()[1], track);
        }

        {
            let mut a = library();
            let mut b = a.fork();
            let playlist = first_playlist(&a);
            let ids = playlist.track_ids();
            let moved = ids[0].clone();
            let removed = ids[1].clone();
            edit(
                &mut a,
                &Library::move_in_playlist(&playlist.id, &moved, ids.len() - 1),
            )
            .unwrap();
            edit(
                &mut b,
                &Library::remove_from_playlist(&playlist.id, &removed),
            )
            .unwrap();
            edit(&mut b, &Library::like(&moved, true)).unwrap();
            a.sync_with(&mut b).unwrap();
            assert_eq!(a.state().unwrap(), b.state().unwrap());
            let merged = a
                .state()
                .unwrap()
                .playlist(&playlist.id)
                .unwrap()
                .track_ids();
            assert!(!merged.contains(&removed));
            assert_eq!(merged.last(), Some(&moved));
            assert_eq!(
                merged.iter().filter(|id| **id == moved).count(),
                1,
                "a moved song stays one membership"
            );
        }
    }
}
