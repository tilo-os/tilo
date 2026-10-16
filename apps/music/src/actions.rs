//! Music's effects and the rows it publishes for what the window shows.
//!
//! Every change to the shared library is the [`Library`](crate::model::Library)
//! resource's: its verbs (making and deleting a playlist, bringing in a
//! catalogue song) and field edits (likes, saved albums, followed artists,
//! recommendation feedback, a playlist's name, cover and tracks). The
//! on-screen affordances, the palette and agents all submit those.
//!
//! What Music does that changes no document is a [`MusicEffect`]: what is
//! shown, and this device's player, queue and offline copies. The window
//! publishes the library's tracks, albums, artists, playlists and places as
//! rows carrying the effects that apply to each, beside this device's
//! player and queue.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Effect};

use tilo_actions::{ActionSpec, Resource};
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
    #[effect(device_only)]
    Sources,
    /// Enable full Spotify library and device controls
    ///
    /// Explicitly requests editing, playlist, followed-artist and remote-device permissions.
    #[effect(job, device_only)]
    ConnectSpotifyLibrary,
    /// Connect Spotify
    ///
    /// Requests profile and saved-library read access, without editing or remote control.
    #[effect(job, device_only)]
    ConnectSpotify,
    /// Authorize Spotify playback
    ///
    /// Separately grants native streaming for the same Premium account.
    #[effect(job, device_only)]
    ConnectSpotifyPlayback,
    /// Inspect Spotify account eligibility
    /// Reads existing grants only; returns product classifications and account matching.
    #[effect(job, device_only, returns, idempotent, kind = "read")]
    SpotifyAccountStatus,
    /// Refresh Spotify devices
    #[effect(job, device_only, returns)]
    SpotifyDevices,
    /// Show Spotify's remote queue
    ///
    /// Reads a provider snapshot; never replaces this device's queue.
    #[effect(job, device_only, returns)]
    SpotifyQueue,
    /// Load more Spotify library collections
    #[effect(job, device_only, returns)]
    SpotifyLibraryMore,
    /// Name a new private Spotify playlist
    #[effect(device_only)]
    NewSpotifyPlaylist,
    /// Create private Spotify playlist
    ///
    /// Creates an empty private playlist. Refresh before retrying an uncertain result.
    #[effect(job, device_only, returns)]
    SpotifyCreatePlaylist {
        #[param("Playlist name")]
        name: String,
    },
    /// Transfer Spotify playback
    #[effect(job, device_only, returns)]
    SpotifyTransfer {
        #[param("Spotify Connect device ID")]
        device: String,
    },
    /// Play on a Spotify device
    #[effect(job, device_only, returns)]
    SpotifyRemotePlay {
        #[param("Spotify Connect device ID")]
        device: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Pause a Spotify device
    #[effect(job, device_only, returns)]
    SpotifyRemotePause {
        #[param("Spotify Connect device ID")]
        device: String,
    },
    /// Save to Spotify library
    #[effect(job, device_only, returns)]
    SpotifySave {
        #[param("Spotify track, album, artist or playlist URI")]
        uri: String,
    },
    /// Remove from Spotify library
    #[effect(job, device_only, returns)]
    SpotifyUnsave {
        #[param("Spotify track, album, artist or playlist URI")]
        uri: String,
    },
    /// Rename Spotify playlist
    #[effect(job, device_only, returns)]
    SpotifyRenamePlaylist {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("New name")]
        name: String,
    },
    /// Add to Spotify playlist
    #[effect(job, device_only, returns)]
    SpotifyPlaylistAdd {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Remove from Spotify playlist
    ///
    /// Removes every occurrence of the track from this Spotify playlist.
    #[effect(job, device_only, returns)]
    SpotifyPlaylistRemove {
        #[param("Spotify playlist URI")]
        playlist: String,
        #[param("Spotify track URI")]
        track: String,
    },
    /// Disconnect Spotify
    ///
    /// Forgets the local grant. Revoke Tilo in Spotify account settings to remove provider-side access.
    #[effect(job, device_only)]
    DisconnectSpotify,
    /// Browse Spotify
    #[effect(job, device_only)]
    BrowseSpotify,
    /// Browse `YouTube` Music
    #[effect(job, device_only)]
    BrowseYouTube,

    /// Search the remote catalogue
    /// Results join the canonical search index after the worker answers.
    #[effect(job, device_only, kind = "read")]
    Search { query: String },
    /// Inspect playback
    /// Device-local transport observations, with no account or credential data.
    #[effect(row, device_only, returns, idempotent, kind = "read")]
    PlaybackStatus,
    /// Open audio file
    ///
    /// Plays a local audio file the person chooses on this device.
    #[effect(job, device_only, returns)]
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
    label.clone_into(&mut spec.label);
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
// Resource tree
// ===========================================================================

/// One track surfaced under a playlist resource: its id plus a one-line label.
#[derive(Debug, Clone)]
pub struct TrackRef {
    pub id: String,
    pub label: String,
    /// Who performs the song: the row's secondary line in search.
    pub artist: String,
    pub liked: bool,
}

/// One playlist surfaced as a resource, with its ordered tracks.
#[derive(Debug, Clone)]
pub struct PlaylistResource {
    pub id: String,
    pub name: String,
    pub tracks: Vec<TrackRef>,
}

#[derive(Debug, Clone)]
pub struct CatalogTrackResource {
    pub id: String,
    pub label: String,
    /// Canonical parent targets retained on the track resource, so an agent
    /// can navigate from a song to its exact album or artist without parsing a
    /// display label or guessing an id.
    pub album_id: String,
    pub artist_id: String,
    pub offline: bool,
    pub liked: bool,
}

#[derive(Debug, Clone)]
pub struct CatalogAlbumResource {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct CatalogArtistResource {
    pub id: String,
    pub label: String,
    pub followed: bool,
}

/// One playlist row addressable independently from the bounded resource page.
#[derive(Debug, Clone)]
pub struct PlaylistMembershipResource {
    pub playlist: String,
    pub track: TrackRef,
}

/// One addressable occurrence in the local playback queue. The queue permits
/// duplicates, therefore `index` is part of the identity rather than merely
/// presentational numbering.
#[derive(Debug, Clone)]
pub struct QueueItemResource {
    pub index: usize,
    pub track: String,
    pub label: String,
    /// Who performs the song: the row's secondary line in search.
    pub artist: String,
    pub current: bool,
}

/// The presentation context Music publishes its rows from: the user's
/// playlists and their tracks currently in the model cache. Everything here is
/// derived from state the document already holds; nothing is fabricated.
#[derive(Debug, Clone, Default)]
pub struct MusicResourceCtx {
    pub local_file: Option<CatalogTrackResource>,
    pub playlists: Vec<PlaylistResource>,
    pub tracks: Vec<CatalogTrackResource>,
    pub albums: Vec<CatalogAlbumResource>,
    pub artists: Vec<CatalogArtistResource>,
    pub membership: Option<PlaylistMembershipResource>,
    /// Bounded contextual queue rows. Direct resource resolution rebuilds only
    /// the exact requested row, so a long queue never gets cloned merely to
    /// open Ctrl+K.
    pub queue: Vec<QueueItemResource>,
    pub playback: crate::model::Playback,
}

impl MusicResourceCtx {}

impl MusicResourceCtx {
    /// Just what resolving the library row `id` needs, against the whole
    /// library rather than the published page: a search hit or an agent may
    /// name any track, album, artist, playlist or membership.
    pub fn for_document_id(lib: &crate::model::Library, id: &str) -> Option<Self> {
        let track_label = |track: &crate::model::Track| {
            let artist = lib.artist_name(&track.artist_id);
            if artist.is_empty() {
                track.title.clone()
            } else {
                format!("{} — {artist}", track.title)
            }
        };
        let catalog_track = |track: &crate::model::Track| CatalogTrackResource {
            id: track.id.clone(),
            label: track_label(track),
            album_id: track.album_id.clone(),
            artist_id: track.artist_id.clone(),
            offline: false,
            liked: lib.is_liked(&track.id),
        };
        let mut ctx = Self::default();
        if parse_view_resource_id(id).is_some() {
            return Some(ctx);
        }
        if let Some(track_id) = id.strip_prefix("music.track:").filter(|id| !id.is_empty()) {
            ctx.tracks.push(catalog_track(lib.track(track_id)?));
            return Some(ctx);
        }
        if let Some(album_id) = id.strip_prefix("music.album:").filter(|id| !id.is_empty()) {
            let album = lib.album(album_id)?;
            ctx.albums.push(CatalogAlbumResource {
                id: album.id.clone(),
                label: album.title.clone(),
            });
            return Some(ctx);
        }
        if let Some(artist_id) = id.strip_prefix("music.artist:").filter(|id| !id.is_empty()) {
            let artist = lib.artist(artist_id)?;
            ctx.artists.push(CatalogArtistResource {
                id: artist.id.clone(),
                label: artist.name.clone(),
                followed: lib.is_artist_followed(&artist.id),
            });
            return Some(ctx);
        }
        if let Some((playlist_id, track_id)) = parse_playlist_track_resource_id(id) {
            let playlist = lib.playlist(&playlist_id)?;
            if !playlist.contains(&track_id) {
                return None;
            }
            let track = lib.track(&track_id)?;
            ctx.membership = Some(PlaylistMembershipResource {
                playlist: playlist.id.clone(),
                track: TrackRef {
                    id: track.id.clone(),
                    label: track_label(track),
                    artist: lib.artist_name(&track.artist_id).to_owned(),
                    liked: lib.is_liked(&track.id),
                },
            });
            return Some(ctx);
        }
        let playlist_id = parse_playlist_resource_id(id)?;
        let playlist = lib.playlist(&playlist_id)?;
        ctx.playlists.push(PlaylistResource {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
            tracks: Vec::new(),
        });
        Some(ctx)
    }
}

/// The library as this window shows it: playlists, tracks, albums,
/// artists and navigation views, each with the effects that apply to it.
/// A bounded page; [`resource_by_id`] resolves the rest.
pub(crate) fn library_rows(ctx: &MusicResourceCtx) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut playlists = ctx.playlists.iter().collect::<Vec<_>>();
    playlists.sort_by(|a, b| a.id.cmp(&b.id));
    for playlist in playlists.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        children.push(playlist_resource(playlist, OPEN_PLAYLIST_TRACK_LIMIT));
    }
    let mut tracks = ctx.tracks.iter().collect::<Vec<_>>();
    tracks.sort_by(|a, b| a.id.cmp(&b.id));
    for track in tracks.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        children.push(catalog_track_resource(track));
    }
    let mut albums = ctx.albums.iter().collect::<Vec<_>>();
    albums.sort_by(|a, b| a.id.cmp(&b.id));
    for album in albums.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        children.push(catalog_album_resource(album));
    }
    let mut artists = ctx.artists.iter().collect::<Vec<_>>();
    artists.sort_by(|a, b| a.id.cmp(&b.id));
    for artist in artists.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        children.push(catalog_artist_resource(artist));
    }
    for &(id, label) in NAVIGATION_VIEWS {
        children.push(navigation_view_resource(id, label));
    }
    children
}

/// Device-local player, queue and open file. Published beside the framework
/// root when hosted — never a second copy of the library document door.
pub(crate) fn live_device_resources(ctx: &MusicResourceCtx) -> Vec<Resource> {
    let mut resources = vec![
        with_transport(
            Resource::new("music.player:player", "music.player", "Player"),
            &ctx.playback,
        ),
        queue_resource(ctx),
    ];
    if let Some(file) = &ctx.local_file {
        resources.push(local_file_resource(file, &ctx.playback));
    }
    resources
}

fn playlist_label(p: &PlaylistResource) -> String {
    let n = p.name.trim();
    if n.is_empty() {
        "Untitled playlist".to_string()
    } else {
        n.to_string()
    }
}

fn navigation_view_resource(id: &str, label: &str) -> Resource {
    Resource::new(view_resource_id(id), "music.view", label).with_action(bound(
        &MusicEffect::Open {
            place: Place::View(id.to_owned()),
        },
        &format!("Open {label}"),
    ))
}

/// The player's transport, bound to the player row: what is offered right
/// now, and why the rest is not.
fn with_transport(mut root: Resource, playback: &crate::model::Playback) -> Resource {
    root = root.with_action(MusicEffect::PlaybackStatus.bound());
    if playback.current.is_some() {
        root = root.with_action(bound(
            &MusicEffect::PlayPause,
            if playback.playing { "Pause" } else { "Play" },
        ));
    }
    root = root
        .with_action(MusicEffect::ToggleMute.bound())
        .with_action(bound(
            &MusicEffect::ToggleShuffle,
            if playback.shuffle {
                "Turn shuffle off"
            } else {
                "Turn shuffle on"
            },
        ))
        .with_action(MusicEffect::CycleRepeat.bound());
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
    let previous = MusicEffect::Previous.bound();
    root = root.with_action(match unavailable {
        Some((code, reason)) => previous.unavailable(code, reason),
        None => previous,
    });
    // Music's on-screen shuffle permutation is intentionally device-local so
    // it never leaks a random, non-replayable decision into the CRDT. A remote
    // caller can still select a concrete track, but a generic "next" would be
    // dishonest while that local permutation is active.
    let next = MusicEffect::Next.bound();
    root.with_action(match unavailable {
        Some((code, reason)) => next.unavailable(code, reason),
        None if playback.shuffle => next.unavailable(
            "local_shuffle_order",
            "Choose a concrete track while this device's shuffle order is active",
        ),
        None => next,
    })
}

fn track_id_from_resource(address: &str) -> Option<&str> {
    address
        .strip_prefix("music.track:")
        .filter(|id| !id.is_empty())
}

/// The playback `effect` leads to from `playback` on this device, for the
/// transport and queue effects that only change it; `None` for any other
/// effect. `known` says whether a track can be played here.
pub(crate) fn next_playback(
    effect: &MusicEffect,
    playback: &crate::model::Playback,
    known: &dyn Fn(&str) -> bool,
) -> Option<Result<crate::model::Playback, String>> {
    let mut next = playback.clone();
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
            }
            Ok(())
        }
        MusicEffect::QueueAdd { track } => known_track(track).map(|track| next.queue.push(track)),
        MusicEffect::QueueRemove { track } => {
            known_track(track).and_then(|track| remove_from_queue(&mut next, &track))
        }
        _ => return None,
    };
    Some(result.map(|()| next))
}

/// Take `track`'s first occurrence out of `playback`'s queue; removing the
/// current one moves on to what follows it, or stops at the end.
fn remove_from_queue(playback: &mut crate::model::Playback, track: &str) -> Result<(), String> {
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
    Ok(())
}

fn open_queue() -> ActionSpec {
    bound(
        &MusicEffect::Open {
            place: Place::View("queue".into()),
        },
        "Open queue",
    )
}

/// A song as search shows it: its title as the row's label and its artist
/// as the secondary line, one fact each.
fn song_resource(id: String, kind: &str, title: &str, artist: &str) -> Resource {
    let resource = Resource::new(id, kind, title);
    if artist.is_empty() {
        return resource;
    }
    resource.with_presentation(tilo_actions::ResourcePresentation {
        detail: Some(artist.to_string()),
        ..tilo_actions::ResourcePresentation::default()
    })
}

fn queue_item_resource(item: &QueueItemResource, total: usize) -> Resource {
    let index = item.index;
    let track = item.track.clone();
    let move_up = MusicEffect::MoveUp {
        index,
        track: track.clone(),
    }
    .bound();
    let move_down = MusicEffect::MoveDown {
        index,
        track: track.clone(),
    }
    .bound();
    song_resource(
        queue_item_resource_id(index, &track),
        "music.queue-item",
        &item.label,
        &item.artist,
    )
    .with_action(open_queue())
    .with_action(bound(
        &MusicEffect::Play {
            track: track.clone(),
            queued: Some(index),
        },
        if item.current {
            "Restart track"
        } else {
            "Play queue item"
        },
    ))
    .with_action(MusicEffect::Remove { index, track }.bound())
    .with_action(if index > 0 {
        move_up
    } else {
        move_up.unavailable(
            "first_queue_item",
            "This item is already first in the queue",
        )
    })
    .with_action(if index + 1 < total {
        move_down
    } else {
        move_down.unavailable("last_queue_item", "This item is already last in the queue")
    })
}

fn queue_resource(ctx: &MusicResourceCtx) -> Resource {
    let total = ctx.playback.queue.len();
    let mut resource =
        Resource::new(QUEUE_RESOURCE_ID, "music.queue", "Queue").with_action(open_queue());
    for item in &ctx.queue {
        resource = resource.with_child(queue_item_resource(item, total));
    }
    resource
}

fn playlist_resource(playlist: &PlaylistResource, track_limit: usize) -> Resource {
    let mut resource = Resource::new(
        playlist_resource_id(&playlist.id),
        "music.playlist",
        playlist_label(playlist),
    )
    .with_action(bound(
        &MusicEffect::Open {
            place: Place::Playlist(playlist.id.clone()),
        },
        "Open playlist",
    ));
    for track in playlist.tracks.iter().take(track_limit) {
        resource = resource.with_child(playlist_track_resource(&playlist.id, track));
    }
    resource
}

fn playlist_track_resource(playlist: &str, track: &TrackRef) -> Resource {
    song_resource(
        playlist_track_resource_id(playlist, &track.id),
        "music.playlist-track",
        &track.label,
        &track.artist,
    )
    .with_action(bound(
        &MusicEffect::Open {
            place: Place::Membership {
                playlist: playlist.to_string(),
                track: track.id.clone(),
            },
        },
        "Open in playlist",
    ))
    .with_action(like_spec(&track.id, track.liked))
}

/// Liking or unliking `track`, which is `liked` now.
fn like_spec(track: &str, liked: bool) -> ActionSpec {
    bound(
        &MusicEffect::ToggleLike {
            track: track.to_owned(),
        },
        if liked { "Unlike song" } else { "Like song" },
    )
}

pub(crate) fn catalog_track_resource(track: &CatalogTrackResource) -> Resource {
    let tracks = vec![track.id.clone()];
    let mut resource = Resource::new(
        track_resource_id(&track.id),
        "music.track",
        track.label.clone(),
    )
    .with_action(bound(
        &MusicEffect::Play {
            track: track.id.clone(),
            queued: None,
        },
        "Play track",
    ))
    .with_action(if track.id.starts_with("spotify:") {
        MusicEffect::KeepOffline { tracks }.bound().unavailable(
            "spotify_offline_unsupported",
            "Spotify protected audio cannot be downloaded through Tilo",
        )
    } else if track.offline {
        MusicEffect::RemoveOffline { tracks }.bound()
    } else {
        MusicEffect::KeepOffline { tracks }.bound()
    })
    .with_action(like_spec(&track.id, track.liked))
    .with_action(
        MusicEffect::AddToQueue {
            track: track.id.clone(),
        }
        .bound(),
    )
    .with_action(
        MusicEffect::PlayNext {
            track: track.id.clone(),
        }
        .bound(),
    );
    if track.id.starts_with("spotify:track:") {
        resource = resource
            .with_action(
                MusicEffect::SpotifySave {
                    uri: track.id.clone(),
                }
                .bound(),
            )
            .with_action(
                MusicEffect::SpotifyUnsave {
                    uri: track.id.clone(),
                }
                .bound(),
            );
    }
    if !track.album_id.is_empty() {
        resource = resource.with_action(
            MusicEffect::OpenAlbum {
                album: track.album_id.clone(),
            }
            .bound(),
        );
    }
    if !track.artist_id.is_empty() {
        resource = resource.with_action(
            MusicEffect::OpenArtist {
                artist: track.artist_id.clone(),
            }
            .bound(),
        );
    }
    resource
}

fn catalog_album_resource(album: &CatalogAlbumResource) -> Resource {
    Resource::new(
        album_resource_id(&album.id),
        "music.album",
        album.label.clone(),
    )
    .with_action(bound(
        &MusicEffect::Open {
            place: Place::Album(album.id.clone()),
        },
        "Open album",
    ))
}

/// An album, artist or playlist the live catalogue found and the library
/// does not hold, labelled `label`, as the row `id`: opening its page is
/// all it offers. `None` for any other row.
pub fn live_catalogue_resource(id: &str, label: &str) -> Option<Resource> {
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
    Some(Resource::new(id, kind, label).with_action(bound(&effect, "Open")))
}

fn catalog_artist_resource(artist: &CatalogArtistResource) -> Resource {
    let resource = Resource::new(
        artist_resource_id(&artist.id),
        "music.artist",
        artist.label.clone(),
    )
    .with_action(bound(
        &MusicEffect::Open {
            place: Place::Artist(artist.id.clone()),
        },
        "Open artist",
    ));
    if artist.id.starts_with("spotify:artist:") {
        resource.with_action(bound(
            &if artist.followed {
                MusicEffect::SpotifyUnsave {
                    uri: artist.id.clone(),
                }
            } else {
                MusicEffect::SpotifySave {
                    uri: artist.id.clone(),
                }
            },
            if artist.followed {
                "Unfollow artist"
            } else {
                "Follow artist"
            },
        ))
    } else {
        resource
    }
}

/// Resolve one catalog resource without first constructing the entire library
/// tree. Search rows carry canonical resource/action addresses; at invocation
/// time the host calls this against the current model, so deleted or
/// otherwise stale targets disappear instead of gaining authority from an old
/// palette snapshot.
pub fn resource_by_id(ctx: &MusicResourceCtx, id: &str) -> Option<Resource> {
    if id == LOCAL_FILE_RESOURCE_ID {
        return ctx
            .local_file
            .as_ref()
            .map(|file| local_file_resource(file, &ctx.playback));
    }
    if let Some(view) = parse_view_resource_id(id) {
        return known_view_label(view).map(|label| navigation_view_resource(view, label));
    }
    if let Some(membership) = &ctx.membership
        && id == playlist_track_resource_id(&membership.playlist, &membership.track.id)
    {
        return Some(playlist_track_resource(
            &membership.playlist,
            &membership.track,
        ));
    }
    if id == "music.player:player" {
        return Some(with_transport(
            Resource::new("music.player:player", "music.player", "Player"),
            &ctx.playback,
        ));
    }
    if id == QUEUE_RESOURCE_ID {
        return Some(queue_resource(ctx));
    }
    if let Some((index, track)) = parse_queue_item_resource_id(id) {
        let item = ctx
            .queue
            .iter()
            .find(|item| item.index == index && item.track == track)?;
        return Some(queue_item_resource(item, ctx.playback.queue.len()));
    }
    if let Some(track_id) = id.strip_prefix("music.track:").filter(|id| !id.is_empty()) {
        return ctx
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(catalog_track_resource);
    }
    if let Some(album_id) = id.strip_prefix("music.album:").filter(|id| !id.is_empty()) {
        return ctx
            .albums
            .iter()
            .find(|album| album.id == album_id)
            .map(catalog_album_resource);
    }
    if let Some(artist_id) = id.strip_prefix("music.artist:").filter(|id| !id.is_empty()) {
        return ctx
            .artists
            .iter()
            .find(|artist| artist.id == artist_id)
            .map(catalog_artist_resource);
    }
    let playlist_id = parse_playlist_resource_id(id)?;
    ctx.playlists
        .iter()
        .find(|playlist| playlist.id == playlist_id)
        .map(|playlist| playlist_resource(playlist, 0))
}

impl crate::app::MusicApp {
    /// The row `id` the surface shows — the player, the queue and its
    /// entries, a track, album, artist, playlist or membership, a view, the
    /// open file — resolved against what it shows now, so a stale address
    /// from an old palette snapshot finds nothing.
    pub fn publish_resource(&self, id: &str) -> Option<Resource> {
        let ctx = self.music_resource_ctx_for_id(id)?;
        resource_by_id(&ctx, id)
    }
}

#[cfg(test)]
impl crate::app::MusicApp {
    /// What the palette and agents see of `doc` in a window showing this
    /// surface: the library as the framework publishes it, with the
    /// surface's live player, queue and open file beside its rows.
    pub(crate) fn palette_tree(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        use ui::tilo_resource::Resource as _;
        let state = crate::model::Library::read(doc).unwrap_or_default();
        let mut root =
            ui::tilo_resource::publish::<crate::model::Library>("music.library", "Library", &state);
        let ctx = self.music_resource_ctx();
        let rows = library_rows(&ctx)
            .into_iter()
            .chain(live_device_resources(&ctx));
        for row in rows {
            match root.children.iter_mut().find(|child| child.id == row.id) {
                Some(item) => {
                    item.actions.extend(row.actions);
                    item.children.extend(row.children);
                }
                None => root.children.push(row),
            }
        }
        vec![root]
    }
}

/// A device-local source has no collaborative track/catalog identity.
pub const LOCAL_FILE_RESOURCE_ID: &str = "music.local-file:current";

fn local_file_resource(file: &CatalogTrackResource, playback: &crate::model::Playback) -> Resource {
    let resource = Resource::new(
        LOCAL_FILE_RESOURCE_ID,
        "music.local-file",
        file.label.clone(),
    );
    if playback.current.as_deref() == Some(&file.id) {
        return with_transport(resource, playback);
    }
    resource.with_action(bound(
        &MusicEffect::Play {
            track: file.id.clone(),
            queued: None,
        },
        "Play file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::MusicApp;
    use crate::model::{Library, Playlist};
    use automerge::AutoCommit;
    use tilo_actions::ActionKind;
    use tilo_actions::{
        ActionRegistry, ArgValue, ExecutionMode, OperationKind, ParamType, ResultSpec,
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
    fn seeded() -> (MusicApp, AutoCommit) {
        let mut app = MusicApp::new();
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

    /// The tree the palette sees for the rows in `ctx`: the library root the
    /// framework publishes and the window's rows.
    fn published_tree(ctx: &MusicResourceCtx) -> Vec<Resource> {
        let mut root =
            ui::tilo_resource::publish::<Library>("music.library", "Library", &Library::default());
        root.children.clear();
        root.children.extend(library_rows(ctx));
        root.children.extend(live_device_resources(ctx));
        vec![root]
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

        // Phase `contextual_resources_are_bounded_but_late_catalog_items_resolve_lazily`.
        {
            let mut ctx = MusicResourceCtx::default();
            for number in 0..(OPEN_RESOURCE_CATEGORY_LIMIT + 9) {
                let id = format!("track-{number:03}");
                ctx.tracks.push(CatalogTrackResource {
                    label: format!("Track {number}"),
                    id,
                    album_id: String::new(),
                    artist_id: String::new(),
                    offline: false,
                    liked: false,
                });
            }
            let resources = published_tree(&ctx);
            let root = &resources[0];
            assert_eq!(
                root.children
                    .iter()
                    .filter(|resource| resource.kind == "music.track")
                    .count(),
                OPEN_RESOURCE_CATEGORY_LIMIT,
                "opening Ctrl+K never clones every catalog track"
            );

            let late_id = track_resource_id("track-024");
            assert!(
                !root
                    .children
                    .iter()
                    .any(|resource| resource.id.as_str() == late_id),
                "the late track is intentionally absent from the contextual page"
            );
            let late = resource_by_id(&ctx, &late_id).expect("late track resolves by stable id");
            assert_eq!(late.id.as_str(), late_id);
            assert!(late.actions.iter().any(|action| action.id == "play"));
            assert!(
                resource_by_id(&ctx, "music.track:deleted").is_none(),
                "a stale address never resolves into new authority"
            );
        }

        // Phase `track_resource_exposes_exact_parent_navigation_targets`.
        {
            let track = CatalogTrackResource {
                id: "track-1".into(),
                label: "Song".into(),
                album_id: "album-9".into(),
                artist_id: "artist-2".into(),
                offline: false,
                liked: false,
            };
            let resource = catalog_track_resource(&track);
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

        // Phase `document_mutations_advertise_write_permission_and_views_advertise_read`.
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

        // Phase `every_top_level_destination_is_a_safe_canonical_view_resource`.
        {
            let ctx = MusicResourceCtx::default();
            let root = published_tree(&ctx)
                .into_iter()
                .next()
                .expect("library root");
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
                    resource_by_id(&ctx, &resource_id).map(|r| r.id.as_str().to_string()),
                    Some(resource_id)
                );
            }
            assert!(parse_view_resource_id("music.view:../../etc/passwd").is_none());
            assert!(anchor_effect("music.view:../../etc/passwd").is_none());
        }

        // Phase `published_resource_action_surface_validates_with_music_handlers`.
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
        // Phase `portable_playlist_cover_replays_to_a_peer_without_a_local_path`.
        {
            let mut a = library();
            let mut b = a.fork();
            let playlist = first_playlist(&a).id;
            // A tiny valid PNG, representative of the normalized portal output.
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

        // Phase `playlist_cover_edit_refuses_local_unsafe_and_malformed_values`.
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

        // Phase `new_playlist_verb_creates_a_playlist`.
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
        // Phase `playback_and_queue_are_local_effects_with_typed_arguments`.
        {
            let mut ctx = MusicResourceCtx::default();
            ctx.playback.current = Some("track-1".into());
            ctx.playback.queue.push("track-1".into());
            let resources = published_tree(&ctx);
            let player = resources[0]
                .children
                .iter()
                .find(|resource| resource.kind == "music.player")
                .unwrap();
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
                    !action.contract.reversible,
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
            let track = CatalogTrackResource {
                id: "track-2".into(),
                label: "Track 2".into(),
                album_id: String::new(),
                artist_id: String::new(),
                offline: false,
                liked: false,
            };
            let play = catalog_track_resource(&track)
                .actions
                .into_iter()
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
            let offline = catalog_track_resource(&track)
                .actions
                .into_iter()
                .find(|action| action.id == "keep-offline")
                .unwrap();
            assert_eq!(
                offline.contract.execution,
                ExecutionMode::Background { cancellable: true },
                "offline transfer is a bounded cancellable device effect"
            );
        }

        // Phase `queue_occurrences_are_canonical_and_duplicate_safe`.
        {
            let mut ctx = MusicResourceCtx::default();
            ctx.playback.queue = vec!["track-1".into(), "track-1".into(), "track-2".into()];
            ctx.playback.current = Some("track-1".into());
            ctx.queue = vec![
                QueueItemResource {
                    index: 0,
                    track: "track-1".into(),
                    label: "First track".into(),
                    artist: String::new(),
                    current: true,
                },
                QueueItemResource {
                    index: 1,
                    track: "track-1".into(),
                    label: "Second occurrence".into(),
                    artist: String::new(),
                    current: false,
                },
                QueueItemResource {
                    index: 2,
                    track: "track-2".into(),
                    label: "Third track".into(),
                    artist: String::new(),
                    current: false,
                },
            ];
            let root = published_tree(&ctx).pop().unwrap();
            let queue = root
                .children
                .iter()
                .find(|resource| resource.id.as_str() == QUEUE_RESOURCE_ID)
                .expect("local queue has a canonical resource");
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

        // Phase `resource_arguments_refuse_wrong_kind_and_stale_tracks`.
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

        // Phase `stale_library_edits_are_refused`.
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
        // Phase `late_playlist_membership_resolves_lazily_with_canonical_actions`.
        {
            let playlist = PlaylistResource {
                id: "playlist:日本語".into(),
                name: "Long playlist".into(),
                tracks: (0..(OPEN_PLAYLIST_TRACK_LIMIT + 8))
                    .map(|number| TrackRef {
                        id: format!("track:{number}:🎧"),
                        label: format!("Track {number}"),
                        artist: String::new(),
                        liked: false,
                    })
                    .collect(),
            };
            let target = playlist.tracks.last().cloned().expect("late membership");
            let ctx = MusicResourceCtx {
                playlists: vec![playlist.clone()],
                membership: Some(PlaylistMembershipResource {
                    playlist: playlist.id.clone(),
                    track: target.clone(),
                }),
                ..Default::default()
            };
            let snapshot = published_tree(&ctx);
            let playlist_snapshot = snapshot[0]
                .children
                .iter()
                .find(|resource| resource.id.as_str() == playlist_resource_id(&playlist.id))
                .expect("playlist remains present after the fixed navigation resources");
            assert_eq!(
                playlist_snapshot.children.len(),
                OPEN_PLAYLIST_TRACK_LIMIT,
                "the contextual page does not materialize every membership"
            );
            assert_eq!(
                parse_playlist_resource_id(&playlist_resource_id(&playlist.id)),
                Some(playlist.id.clone())
            );
            let id = playlist_track_resource_id(&playlist.id, &target.id);
            assert_eq!(
                parse_playlist_track_resource_id(&id),
                Some((playlist.id.clone(), target.id.clone())),
                "membership identity is Unicode-safe and reversible"
            );
            let direct = resource_by_id(&ctx, &id).expect("late membership resolves directly");
            let open = direct
                .actions
                .iter()
                .find(|action| action.id == "open")
                .unwrap();
            assert_eq!(
                MusicEffect::from_payload(&spec_encoding_of(open).payload).unwrap(),
                MusicEffect::Open {
                    place: Place::Membership {
                        playlist: playlist.id.clone(),
                        track: target.id,
                    }
                },
                "the lazy row opens the exact membership"
            );
        }

        // Phase `stale_membership_remove_is_refused_and_a_move_lands_once`.
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

        // Phase `divergent_playlist_edits_converge_by_stable_membership`.
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
