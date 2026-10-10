//! Music's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Music performs on the shared library —
//! creating, renaming, deleting a playlist; adding, removing or reordering a
//! track in one; liking a song; saving an album; following an artist; setting a
//! playlist cover — is a [`MusicAction`]: a small serializable value that
//! deterministically mutates the CRDT (see [`crate::doc`], where the library,
//! playlists and membership sets live). Because that store already lives in the
//! shared document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (a context-menu row, the
//! playlist trash button, the like heart, the create/rename dialog) dispatch and
//! what the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the library owns its playlists, and
//! each playlist owns its tracks. Each exports the [`ActionSpec`]s Ctrl+K offers.
//!
//! ## What is *not* an action
//!
//! Presentation stays local: selection, scroll, playback and cache/download
//! policy.
//! The stable top-level destinations are still exported as idempotent `View`
//! operations, so history, search hosts and agents can address a screen without
//! guessing a local widget path. Stream resolution, audio output, artwork
//! fetching and the native cover picker are device effects too. Playback and
//! cache choices stay local effects; a selected playlist cover is portable
//! user-authored library data and is written through the action types below.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. Membership writes
//! (like / saved-album / followed-artist) carry an explicit desired value, so
//! delayed delivery cannot invert a user's intent.
//! A malformed or stale payload is refused before it changes the document. This
//! prevents an external caller from recording a successful-looking history
//! entry that actually wrote a dangling library reference.

use std::collections::BTreeSet;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Effect};
use uuid::Uuid;

use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ParamType, PermissionRequirement, Resource,
};

use crate::doc as mdoc;

/// Registry `type_id` for every Music action.
pub const MUSIC_ACTION_TYPE: &str = "music.action";
/// The maximum decoded size of a portable embedded playlist cover.
pub const MAX_PORTABLE_COVER_BYTES: usize = 256 * 1024;
/// Portal normalization never persists an image wider or taller than this.
pub const MAX_PORTABLE_COVER_EDGE: u32 = 512;
/// The contextual Ctrl+K snapshot is deliberately small. The full library is
/// still discoverable through the paged search provider and every returned
/// canonical address is resolved through [`ResourceProvider::resource`].
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

pub fn open_view_address(id: &str) -> Option<ActionAddress> {
    known_view_label(id).map(|_| ActionAddress::new(view_resource_id(id), "open"))
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

pub fn playlist_resource_id(id: &str) -> String {
    format!("music.playlist:{id}")
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

pub fn play_track_address(id: &str) -> ActionAddress {
    ActionAddress::new(track_resource_id(id), "play")
}

pub fn open_album_address(id: &str) -> ActionAddress {
    ActionAddress::new(album_resource_id(id), "open")
}

pub fn open_artist_address(id: &str) -> ActionAddress {
    ActionAddress::new(artist_resource_id(id), "open")
}

pub fn open_playlist_address(id: &str) -> ActionAddress {
    ActionAddress::new(playlist_resource_id(id), "open")
}

/// Open a particular playlist membership in its owning playlist. Unlike
/// `remove`, this is the safe default action for a membership search result.
pub fn open_playlist_membership_address(playlist: &str, track: &str) -> ActionAddress {
    ActionAddress::new(playlist_track_resource_id(playlist, track), "open")
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
    #[effect(job, device_only, permission = "read")]
    Search { query: String },
    /// Inspect playback
    /// Device-local transport observations, with no account or credential data.
    #[effect(row, device_only, returns, idempotent, permission = "read")]
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
    #[effect(row)]
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

/// Every collaborative operation Music exports on its shared library.
///
/// Each variant is a whole-operation write carrying enough payload to replay
/// deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MusicAction {
    SetRecommendationInput {
        input: crate::recommendations::Input,
        enabled: bool,
    },
    SetRecommendationDismissed {
        track: String,
        dismissed: bool,
    },
    SetRecommendationArtist {
        artist: String,
        preferred: bool,
    },
    /// Create a new empty playlist. `id` is minted at authoring time and carried
    /// here so replay reproduces the byte-identical row; `apply` never mints one.
    CreatePlaylist {
        id: String,
        name: String,
    },
    /// Rename a playlist.
    RenamePlaylist {
        id: String,
        name: String,
    },
    /// Delete a playlist by id.
    DeletePlaylist {
        id: String,
    },
    /// Set a stable playlist membership to an absolute state and sparse rank.
    SetPlaylistMembership {
        playlist: String,
        track: String,
        present: bool,
        rank: i64,
    },
    /// Set a stable playlist membership's absolute sparse order key.
    SetTrackRank {
        playlist: String,
        track: String,
        rank: i64,
    },
    /// Set (or clear, with an empty string) a playlist's portable cover value.
    /// The *pick* (the native file portal) is app-side, non-deterministic work;
    /// it is normalized into an HTTPS URL or bounded PNG data URL before this
    /// deterministic, replayable action is written to the shared document.
    SetPlaylistCover {
        playlist: String,
        cover: String,
    },
    /// Set a track's membership in "Liked Songs" to an explicit value.
    SetLiked {
        track: String,
        liked: bool,
    },
    /// Set an album's saved-to-library membership to an explicit value.
    SetSavedAlbum {
        album: String,
        saved: bool,
    },
    /// Set whether the user follows an artist to an explicit value.
    SetFollowedArtist {
        artist: String,
        followed: bool,
    },
    /// Add a catalogue track, with its album and artist, to the library: a
    /// streamed track once it is played, so it stays playable, likeable and
    /// listable on every device. Idempotent: an artist, album or track
    /// already in the library is kept as it is. Content from outside the
    /// library, so no undo step takes it back.
    ImportTrack(Box<crate::model::CatalogueTrack>),
}

fn recommendation_history(action: &MusicAction) -> Option<tilo_actions::ActionContext> {
    let (label, id, kind) = match action {
        MusicAction::SetRecommendationInput { .. } => (
            ui::i18n!("music-recommendations-change-inputs"),
            "library",
            "music.recommendation-preferences",
        ),
        MusicAction::SetRecommendationDismissed { track, dismissed } => (
            ui::i18n!(if *dismissed {
                "music-recommendations-dismiss"
            } else {
                "music-recommendations-restore"
            }),
            track.as_str(),
            "music.recommendation",
        ),
        MusicAction::SetRecommendationArtist { artist, preferred } => (
            ui::i18n!(if *preferred {
                "music-recommendations-prefer"
            } else {
                "music-recommendations-unprefer"
            }),
            artist.as_str(),
            "music.recommendation-artist",
        ),
        _ => return None,
    };
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.music", "Music", action);
    context.action_label = label;
    context.resource = Some(tilo_actions::ActionResource {
        id: format!("{kind}:{id}"),
        kind: kind.to_owned(),
        label: if id == "library" {
            ui::i18n!("music-recommendations-title")
        } else {
            id.to_owned()
        },
    });
    Some(context)
}

/// Durable, browseable history metadata for a library mutation.
pub fn history_context(action: &MusicAction) -> tilo_actions::ActionContext {
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.music", "Music", action);
    if let Some(context) = recommendation_history(action) {
        return context;
    }
    let (label, id, kind, resource_label): (&str, &str, &str, &str) = match action {
        MusicAction::SetRecommendationInput { .. }
        | MusicAction::SetRecommendationDismissed { .. }
        | MusicAction::SetRecommendationArtist { .. } => return context,
        MusicAction::CreatePlaylist { id, name } => ("Create playlist", id, "music.playlist", name),
        MusicAction::RenamePlaylist { id, name } => ("Rename playlist", id, "music.playlist", name),
        MusicAction::DeletePlaylist { id } => ("Delete playlist", id, "music.playlist", id),
        MusicAction::SetPlaylistMembership {
            playlist,
            present: true,
            ..
        } => (
            "Add track to playlist",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::SetPlaylistMembership {
            playlist,
            present: false,
            ..
        } => (
            "Remove track from playlist",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::SetTrackRank { playlist, .. } => {
            ("Reorder playlist", playlist, "music.playlist", playlist)
        }
        MusicAction::SetPlaylistCover { playlist, .. } => (
            "Change playlist cover",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::SetLiked { track, liked } => (
            if *liked { "Like song" } else { "Unlike song" },
            track,
            "music.track",
            track,
        ),
        MusicAction::SetSavedAlbum { album, saved } => (
            if *saved {
                "Save album"
            } else {
                "Remove saved album"
            },
            album,
            "music.album",
            album,
        ),
        MusicAction::SetFollowedArtist { artist, followed } => (
            if *followed {
                "Follow artist"
            } else {
                "Unfollow artist"
            },
            artist,
            "music.artist",
            artist,
        ),
        MusicAction::ImportTrack(imported) => (
            "Add song to library",
            &imported.track.id,
            "music.track",
            &imported.track.title,
        ),
    };
    context.action_label = label.to_string();
    context.resource = Some(tilo_actions::ActionResource {
        id: format!("{kind}:{id}"),
        kind: kind.to_string(),
        label: resource_label.to_string(),
    });
    context
}

impl Action for MusicAction {
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive action matcher keeps every durable mutation's stale-target validation at the one replay authority boundary"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            MusicAction::SetRecommendationInput { .. } => {
                crate::recommendations::write(doc, self).map_err(ActionError::Refused)?;
            }
            MusicAction::SetRecommendationDismissed { track, dismissed } => {
                require(
                    !track.is_empty() && (!dismissed || mdoc::has_track(doc, track)),
                    "recommendation track is no longer in the library",
                )?;
                crate::recommendations::write(doc, self).map_err(ActionError::Refused)?;
            }
            MusicAction::SetRecommendationArtist { artist, preferred } => {
                require(
                    !artist.is_empty() && (!preferred || mdoc::has_artist(doc, artist)),
                    "recommendation artist is no longer in the library",
                )?;
                crate::recommendations::write(doc, self).map_err(ActionError::Refused)?;
            }
            MusicAction::CreatePlaylist { id, name } => {
                // An empty id is never produced by our own builder; no-op rather
                // than fabricate one (which would break replay determinism).
                if !id.is_empty() {
                    mdoc::create_playlist_with_id(doc, id, name);
                }
            }
            MusicAction::RenamePlaylist { id, name } => {
                require(
                    mdoc::has_playlist(doc, id),
                    "playlist is no longer in the library",
                )?;
                mdoc::rename_playlist(doc, id, name);
            }
            MusicAction::DeletePlaylist { id } => {
                require(
                    mdoc::has_playlist(doc, id),
                    "playlist is no longer in the library",
                )?;
                mdoc::delete_playlist(doc, id);
            }
            MusicAction::SetPlaylistMembership {
                playlist,
                track,
                present,
                rank,
            } => {
                require(
                    mdoc::has_playlist(doc, playlist) && mdoc::has_track(doc, track),
                    "playlist or track is no longer in the library",
                )?;
                if *present {
                    mdoc::add_track_to_playlist(doc, playlist, track, *rank);
                } else {
                    require(
                        mdoc::playlist_contains_track(doc, playlist, track),
                        "playlist membership is no longer present",
                    )?;
                    mdoc::remove_track_from_playlist(doc, playlist, track);
                }
            }
            MusicAction::SetTrackRank {
                playlist,
                track,
                rank,
            } => {
                require(
                    mdoc::has_playlist(doc, playlist),
                    "playlist is no longer in the library",
                )?;
                require(
                    mdoc::playlist_contains_track(doc, playlist, track),
                    "playlist membership is no longer present",
                )?;
                mdoc::set_track_rank(doc, playlist, track, *rank);
            }
            MusicAction::SetPlaylistCover { playlist, cover } => {
                require(
                    mdoc::has_playlist(doc, playlist),
                    "playlist is no longer in the library",
                )?;
                require(
                    portable_cover_value(cover),
                    "playlist covers must be HTTPS URLs or bounded embedded PNG data",
                )?;
                mdoc::set_playlist_cover(doc, playlist, cover);
            }
            MusicAction::SetLiked { track, liked } => {
                require(
                    mdoc::has_track(doc, track),
                    "track is no longer in the library",
                )?;
                mdoc::set_liked(doc, track, *liked);
            }
            MusicAction::SetSavedAlbum { album, saved } => {
                require(
                    mdoc::has_album(doc, album),
                    "album is no longer in the library",
                )?;
                mdoc::set_saved_album(doc, album, *saved);
            }
            MusicAction::SetFollowedArtist { artist, followed } => {
                require(
                    !artist.starts_with("spotify:artist:"),
                    "Spotify following requires a provider-authorized effect",
                )?;
                require(
                    mdoc::has_artist(doc, artist),
                    "artist is no longer in the library",
                )?;
                mdoc::set_followed_artist(doc, artist, *followed);
            }
            MusicAction::ImportTrack(imported) => {
                let crate::model::CatalogueTrack {
                    track,
                    album,
                    artist,
                } = imported.as_ref();
                require(
                    !track.id.is_empty()
                        && track.album_id == album.id
                        && track.artist_id == artist.id
                        && album.artist_id == artist.id
                        && !album.id.is_empty()
                        && !artist.id.is_empty(),
                    "a track is imported with its own album and artist",
                )?;
                mdoc::import_track(doc, track, album, artist);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            MusicAction::ImportTrack(_) => tilo_actions::ChangeKind::External,
            _ => tilo_actions::ChangeKind::Content,
        }
    }
}

fn require(condition: bool, message: &str) -> Result<(), ActionError> {
    if condition {
        Ok(())
    } else {
        Err(ActionError::Refused(message.into()))
    }
}

fn invalid_arguments(reason: &str) -> EncodedAction {
    EncodedAction {
        type_id: INVALID_ARGUMENTS_TYPE.to_string(),
        payload: reason.as_bytes().to_vec(),
    }
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

/// Register Music's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<MusicAction>(MUSIC_ACTION_TYPE);
}

/// Encode any [`MusicAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Music action into a
/// replayable envelope.
///
/// # Panics
///
/// Never in practice: every [`MusicAction`] variant holds only `String`/`usize`
/// fields, which serialize losslessly.
#[allow(
    clippy::expect_used,
    reason = "MusicAction holds only String/usize fields, which serde_json can always serialize"
)]
pub fn encode(action: &MusicAction) -> EncodedAction {
    EncodedAction::encode(MUSIC_ACTION_TYPE, action).expect("MusicAction is always serializable")
}

/// Mint a fresh playlist id in the app's stable format (`pl_<uuid>`). Called at
/// *authoring* time — the palette closure and the on-screen create handler both
/// mint here, then bake the id into the envelope so replay stays deterministic.
pub fn new_playlist_id() -> String {
    format!("pl_{}", Uuid::new_v4().simple())
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn create_playlist_action(id: &str, name: &str) -> MusicAction {
    MusicAction::CreatePlaylist {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn rename_playlist_action(id: &str, name: &str) -> MusicAction {
    MusicAction::RenamePlaylist {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_playlist_action(id: &str) -> MusicAction {
    MusicAction::DeletePlaylist { id: id.to_string() }
}

pub fn add_track_action(playlist: &str, track: &str, rank: i64) -> MusicAction {
    MusicAction::SetPlaylistMembership {
        playlist: playlist.to_string(),
        track: track.to_string(),
        present: true,
        rank,
    }
}

pub fn remove_track_action(playlist: &str, track: &str) -> MusicAction {
    MusicAction::SetPlaylistMembership {
        playlist: playlist.to_string(),
        track: track.to_string(),
        present: false,
        rank: 0,
    }
}

pub fn move_track_action(playlist: &str, track: &str, rank: i64) -> MusicAction {
    MusicAction::SetTrackRank {
        playlist: playlist.to_string(),
        track: track.to_string(),
        rank,
    }
}

pub fn set_cover_action(playlist: &str, cover: &str) -> MusicAction {
    MusicAction::SetPlaylistCover {
        playlist: playlist.to_string(),
        cover: cover.to_string(),
    }
}

pub fn set_liked_action(track: &str, liked: bool) -> MusicAction {
    MusicAction::SetLiked {
        track: track.to_string(),
        liked,
    }
}

pub fn set_saved_album_action(album: &str, saved: bool) -> MusicAction {
    MusicAction::SetSavedAlbum {
        album: album.to_string(),
        saved,
    }
}

pub fn import_track_action(
    track: &crate::model::Track,
    album: &crate::model::Album,
    artist: &crate::model::Artist,
) -> MusicAction {
    MusicAction::ImportTrack(Box::new(crate::model::CatalogueTrack {
        track: track.clone(),
        album: album.clone(),
        artist: artist.clone(),
    }))
}

pub fn set_followed_artist_action(artist: &str, followed: bool) -> MusicAction {
    MusicAction::SetFollowedArtist {
        artist: artist.to_string(),
        followed,
    }
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
    pub rank: i64,
}

/// One playlist surfaced as a resource, with its ordered tracks.
#[derive(Debug, Clone)]
pub struct PlaylistResource {
    pub id: String,
    pub name: String,
    pub tracks: Vec<TrackRef>,
}

impl PlaylistResource {
    fn append_rank(&self) -> i64 {
        self.tracks
            .iter()
            .map(|track| track.rank)
            .max()
            .unwrap_or(-1_048_576)
            .saturating_add(1_048_576)
    }

    fn move_rank(&self, index: usize, to: usize) -> i64 {
        let mut remaining = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(candidate, _)| *candidate != index)
            .map(|(_, track)| track.rank)
            .collect::<Vec<_>>();
        let insert = to.min(remaining.len());
        let previous = insert
            .checked_sub(1)
            .and_then(|at| remaining.get(at))
            .copied();
        let next = remaining.get(insert).copied();
        remaining.clear();
        match (previous, next) {
            (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
                previous + (next - previous) / 2
            }
            (Some(previous), _) => previous.saturating_add(1),
            (None, Some(next)) => next.saturating_sub(1_048_576),
            (None, None) => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CatalogTrackResource {
    pub id: String,
    pub label: String,
    /// Who performs the song: the row's secondary line in search.
    pub artist: String,
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
    pub saved: bool,
}

#[derive(Debug, Clone)]
pub struct CatalogArtistResource {
    pub id: String,
    pub label: String,
    pub followed: bool,
}

/// One playlist row addressable independently from the bounded resource page.
/// `index`/`total` are captured together with the track identity, letting the
/// action refuse a stale reorder instead of operating on a recycled position.
#[derive(Debug, Clone)]
pub struct PlaylistMembershipResource {
    pub playlist: String,
    pub track: TrackRef,
    pub index: usize,
    pub total: usize,
    pub up_rank: i64,
    pub down_rank: i64,
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

impl MusicResourceCtx {
    /// Project a shared library into the contextual page the resource door
    /// publishes. Playback, queue and local-file stay on the widget: they are
    /// device-local and are not in the Automerge library.
    pub fn from_library(lib: &crate::model::Library) -> Self {
        let track_title = |track_id: &str| -> String {
            lib.track(track_id)
                .map_or_else(|| track_id.to_string(), |track| track.title.clone())
        };
        let track_artist = |track_id: &str| -> String {
            lib.track(track_id)
                .map(|track| lib.artist_name(&track.artist_id).to_string())
                .unwrap_or_default()
        };
        let mut playlists = lib.playlists.iter().collect::<Vec<_>>();
        playlists.sort_by(|a, b| a.id.cmp(&b.id));
        let playlists = playlists
            .into_iter()
            .take(OPEN_RESOURCE_CATEGORY_LIMIT)
            .map(|playlist| PlaylistResource {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                tracks: playlist
                    .track_ids
                    .iter()
                    .take(OPEN_PLAYLIST_TRACK_LIMIT)
                    .map(|tid| TrackRef {
                        id: tid.clone(),
                        label: track_title(tid),
                        artist: track_artist(tid),
                        liked: lib.is_liked(tid),
                        rank: playlist.track_ranks.get(tid).copied().unwrap_or_default(),
                    })
                    .collect(),
            })
            .collect();
        let mut tracks = lib.tracks.values().collect::<Vec<_>>();
        tracks.sort_by(|a, b| a.id.cmp(&b.id));
        let mut albums = lib.albums.values().collect::<Vec<_>>();
        albums.sort_by(|a, b| a.id.cmp(&b.id));
        let mut artists = lib.artists.values().collect::<Vec<_>>();
        artists.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            playlists,
            tracks: tracks
                .into_iter()
                .take(OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|track| CatalogTrackResource {
                    id: track.id.clone(),
                    label: track_title(&track.id),
                    artist: track_artist(&track.id),
                    album_id: track.album_id.clone(),
                    artist_id: track.artist_id.clone(),
                    // What this device keeps offline is not library state.
                    offline: false,
                    liked: lib.is_liked(&track.id),
                })
                .collect(),
            albums: albums
                .into_iter()
                .take(OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|album| CatalogAlbumResource {
                    id: album.id.clone(),
                    label: album.title.clone(),
                    saved: lib.is_album_saved(&album.id),
                })
                .collect(),
            artists: artists
                .into_iter()
                .take(OPEN_RESOURCE_CATEGORY_LIMIT)
                .map(|artist| CatalogArtistResource {
                    id: artist.id.clone(),
                    label: artist.name.clone(),
                    followed: lib.is_artist_followed(&artist.id),
                })
                .collect(),
            membership: None,
            local_file: None,
            queue: Vec::new(),
            playback: crate::model::Playback::default(),
        }
    }
}

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
            artist: lib.artist_name(&track.artist_id).to_owned(),
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
                saved: lib.is_album_saved(&album.id),
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
            let index = playlist.track_ids.iter().position(|id| id == &track_id)?;
            let track = lib.track(&track_id)?;
            ctx.membership = Some(PlaylistMembershipResource {
                playlist: playlist.id.clone(),
                track: TrackRef {
                    id: track.id.clone(),
                    label: track_label(track),
                    artist: lib.artist_name(&track.artist_id).to_owned(),
                    liked: lib.is_liked(&track.id),
                    rank: playlist
                        .track_ranks
                        .get(&track.id)
                        .copied()
                        .unwrap_or_default(),
                },
                index,
                total: playlist.track_ids.len(),
                up_rank: playlist
                    .rank_for_move(&track.id, index.saturating_sub(1))
                    .unwrap_or_default(),
                down_rank: playlist
                    .rank_for_move(&track.id, index.saturating_add(1))
                    .unwrap_or_default(),
            });
            return Some(ctx);
        }
        let playlist_id = id
            .strip_prefix("music.playlist:")
            .filter(|id| !id.is_empty())?;
        let playlist = lib.playlist(playlist_id)?;
        // `add-track` refuses a stale track argument, so it needs the tracks
        // that are current, once this playlist is asked for.
        ctx.tracks = lib.tracks.values().map(catalog_track).collect();
        ctx.playlists.push(PlaylistResource {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
            tracks: Vec::new(),
        });
        Some(ctx)
    }
}

fn known_track_ids(ctx: &MusicResourceCtx) -> BTreeSet<String> {
    let mut known_tracks = ctx
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(file) = &ctx.local_file {
        known_tracks.insert(file.id.clone());
    }
    known_tracks
}

/// The library's children for [`tilo_resource::publish`]: playlists, tracks,
/// albums, artists, and navigation views. The player, queue and open file are
/// the surface's live rows ([`live_device_resources`]).
pub(crate) fn document_children(ctx: &MusicResourceCtx) -> Vec<Resource> {
    let known_tracks = known_track_ids(ctx);
    let mut children = Vec::new();
    let mut playlists = ctx.playlists.iter().collect::<Vec<_>>();
    playlists.sort_by(|a, b| a.id.cmp(&b.id));
    for playlist in playlists.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        children.push(playlist_resource(
            playlist,
            &known_tracks,
            OPEN_PLAYLIST_TRACK_LIMIT,
        ));
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

#[allow(
    clippy::expect_used,
    reason = "action is always a MusicAction, which encodes losslessly (see encode()'s own allow)"
)]
pub(crate) fn immediate_spec(id: &str, label: &str, action: &MusicAction) -> ActionSpec {
    ActionSpec::immediate(id, label, MUSIC_ACTION_TYPE, action)
        .expect("encode music action")
        .requiring(PermissionRequirement::Write)
        .reversible()
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

fn playlist_resource(
    playlist: &PlaylistResource,
    known_tracks: &BTreeSet<String>,
    track_limit: usize,
) -> Resource {
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
    let playlist_id = playlist.id.clone();
    resource = resource.with_action(
        ActionSpec::prompted(
            "rename",
            "Rename playlist",
            "Name",
            "playlist name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_playlist_action(&playlist_id, name.trim())),
        )
        .requiring(PermissionRequirement::Write)
        .reversible(),
    );
    resource = resource.with_action(
        immediate_spec(
            "delete",
            "Delete playlist",
            &delete_playlist_action(&playlist.id),
        )
        .destructive(),
    );
    let add_playlist = playlist.id.clone();
    let add_rank = playlist.append_rank();
    let add_track_ids = known_tracks.clone();
    resource = resource.with_action(
        ActionSpec::typed(
            "add-track",
            "Add track to playlist",
            "track resource…",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "track",
                ParamType::Resource,
                "stable music.track resource address",
            )],
            move |args: &ActionArgs| {
                let Some(track) = args.text("track").and_then(track_id_from_resource) else {
                    return invalid_arguments("track must be a music.track resource");
                };
                if !add_track_ids.contains(track) {
                    return invalid_arguments("track is no longer in the library");
                }
                encode(&add_track_action(&add_playlist, track, add_rank))
            },
        )
        .requiring(PermissionRequirement::Write),
    );
    let cover_playlist = playlist.id.clone();
    resource = resource.with_action(
        ActionSpec::prompted(
            "set-cover-url",
            "Set playlist cover URL",
            "Cover URL",
            "https://… (blank clears)",
            ExecutionLocus::ClientLocal,
            move |cover| encode(&set_cover_action(&cover_playlist, cover.trim())),
        )
        .requiring(PermissionRequirement::Write)
        .reversible(),
    );
    for (index, track) in playlist.tracks.iter().take(track_limit).enumerate() {
        resource = resource.with_child(playlist_track_resource(
            &playlist.id,
            track,
            index,
            playlist.tracks.len(),
            playlist.move_rank(index, index.saturating_sub(1)),
            playlist.move_rank(index, index.saturating_add(1)),
        ));
    }
    resource
}

fn playlist_track_resource(
    playlist: &str,
    track: &TrackRef,
    index: usize,
    total: usize,
    up_rank: i64,
    down_rank: i64,
) -> Resource {
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
    .with_action(immediate_spec(
        "remove",
        "Remove from playlist",
        &remove_track_action(playlist, &track.id),
    ))
    .with_action(immediate_spec(
        "toggle-like",
        if track.liked {
            "Unlike song"
        } else {
            "Like song"
        },
        &set_liked_action(&track.id, !track.liked),
    ))
    .with_action(if index > 0 {
        immediate_spec(
            "move-up",
            "Move track up",
            &move_track_action(playlist, &track.id, up_rank),
        )
        .reversible()
    } else {
        immediate_spec(
            "move-up",
            "Move track up",
            &move_track_action(playlist, &track.id, track.rank),
        )
        .unavailable("first_track", "This track is already first in the playlist")
    })
    .with_action(if index + 1 < total {
        immediate_spec(
            "move-down",
            "Move track down",
            &move_track_action(playlist, &track.id, down_rank),
        )
        .reversible()
    } else {
        immediate_spec(
            "move-down",
            "Move track down",
            &move_track_action(playlist, &track.id, track.rank),
        )
        .unavailable("last_track", "This track is already last in the playlist")
    })
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
    .with_action(immediate_spec(
        "toggle-like",
        if track.liked {
            "Unlike song"
        } else {
            "Like song"
        },
        &set_liked_action(&track.id, !track.liked),
    ))
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
    .with_action(immediate_spec(
        "toggle-save",
        if album.saved {
            "Remove album from library"
        } else {
            "Save album to library"
        },
        &set_saved_album_action(&album.id, !album.saved),
    ))
}

/// An album, artist or playlist the live catalogue found and the library
/// does not hold, labelled `label`: opening its page at `address` is all it
/// offers. `None` for any other address.
pub fn live_catalogue_resource(address: &ActionAddress, label: &str) -> Option<Resource> {
    if address.action != "open" {
        return None;
    }
    let resource = address.resource.as_str();
    let (kind, place) = if let Some(id) = resource.strip_prefix("music.album:") {
        ("music.album", Place::Album(id.to_owned()))
    } else if let Some(id) = resource.strip_prefix("music.artist:") {
        ("music.artist", Place::Artist(id.to_owned()))
    } else {
        (
            "music.playlist",
            Place::Playlist(resource.strip_prefix("music.playlist:")?.to_owned()),
        )
    };
    Some(
        Resource::new(resource, kind, label)
            .with_action(bound(&MusicEffect::Open { place }, "Open")),
    )
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
        return resource.with_action(bound(
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
        ));
    }
    resource.with_action(immediate_spec(
        "toggle-follow",
        if artist.followed {
            "Unfollow artist"
        } else {
            "Follow artist"
        },
        &set_followed_artist_action(&artist.id, !artist.followed),
    ))
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
            membership.index,
            membership.total,
            membership.up_rank,
            membership.down_rank,
        ));
    }
    let mut known_tracks = ctx
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(file) = &ctx.local_file {
        known_tracks.insert(file.id.clone());
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
    let playlist_id = id
        .strip_prefix("music.playlist:")
        .filter(|id| !id.is_empty())?;
    ctx.playlists
        .iter()
        .find(|playlist| playlist.id == playlist_id)
        .map(|playlist| playlist_resource(playlist, &known_tracks, 0))
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
        let state = crate::doc::load_library(doc);
        let mut root = ui::tilo_resource::publish::<crate::resource::Music>(
            "music.library",
            "Library",
            &state,
        );
        root.children
            .extend(live_device_resources(&self.music_resource_ctx()));
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
    use automerge::AutoCommit;
    use tilo_actions::{
        ActionInput, ArgValue, ExecutionMode, LocalTransport, OperationKind, ResultSpec,
    };
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
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
        let mut doc = AutoCommit::new();
        crate::doc::seed(&mut doc, &crate::seed::sample_library());
        app.sync(&doc, &automerge::ROOT);
        (app, doc)
    }

    fn spec_encoding_of(spec: &ActionSpec) -> EncodedAction {
        ui::testing::action_contract::spec_encoding(spec, None)
    }

    fn contract_seed_doc() -> AutoCommit {
        seeded().1
    }

    /// What the palette sees of `doc`: the library as the framework
    /// publishes it, and the surface's live player, queue and open file.
    fn contract_resources_for(doc: &AutoCommit) -> Vec<Resource> {
        seeded().0.palette_tree(doc)
    }

    /// The tree the palette sees for the rows in `ctx`: the library the
    /// framework publishes and the surface's live rows.
    fn published_tree(ctx: &MusicResourceCtx) -> Vec<Resource> {
        let mut root = ui::tilo_resource::publish::<crate::resource::Music>(
            "music.library",
            "Library",
            &crate::model::Library::default(),
        );
        root.children.clear();
        root.children.extend(document_children(ctx));
        root.children.extend(live_device_resources(ctx));
        vec![root]
    }

    fn playlist_key(res: &Resource) -> &str {
        res.id
            .as_str()
            .strip_prefix("music.playlist:")
            .expect("a playlist resource")
    }

    /// A playlist with tracks, and a library track it does not hold yet.
    fn addable_track(doc: &AutoCommit) -> EncodedAction {
        let lib = crate::doc::load_library(doc);
        let playlist = lib
            .playlists
            .iter()
            .find(|p| !p.track_ids.is_empty())
            .expect("a playlist with tracks");
        let new_track = lib
            .tracks
            .keys()
            .find(|t| !playlist.track_ids.contains(*t))
            .expect("a track not already in the playlist");
        encode(&add_track_action(
            &playlist.id,
            new_track,
            playlist.append_rank(),
        ))
    }

    ui::action_contract_tests! {
        seed: contract_seed_doc(),
        registry: registry(),
        resources: |doc| contract_resources_for(doc),
        palette: ["New playlist", "Rename playlist", "Delete playlist", "Remove from playlist"],
        replay: [
            add_track => |doc| addable_track(doc),
            set_liked => |doc| {
                let lib = crate::doc::load_library(doc);
                let track = lib
                    .tracks
                    .keys()
                    .find(|track| !lib.is_liked(track))
                    .expect("an unliked track");
                encode(&set_liked_action(track, true))
            },
        ],
        undo: [
            delete_playlist => |doc| encode(&delete_playlist_action(
                &crate::doc::load_library(doc).playlists[0].id,
            )),
        ],
        encodes: [
            delete_playlist => ("music.playlist", "delete", |_, res| {
                encode(&delete_playlist_action(playlist_key(res)))
            }),
            rename_playlist => ("music.playlist", "rename", arg: "Road Trip", |_, res| {
                encode(&rename_playlist_action(playlist_key(res), "Road Trip"))
            }),
            remove_track => ("music.playlist-track", "remove", |_, res| {
                let (playlist, track) = parse_playlist_track_resource_id(res.id.as_str())
                    .expect("nested playlist tracks use canonical membership IDs");
                encode(&remove_track_action(&playlist, &track))
            }),
        ],
    }

    #[test]
    fn resource_publish_owns_the_library_door_and_live_extras_stay_separate() {
        let (app, doc) = seeded();
        let library = crate::doc::load_library(&doc);
        let published = ui::tilo_resource::publish::<crate::resource::Music>(
            "music.library",
            "Music",
            &library,
        );
        let root_actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"new-playlist"), "{root_actions:?}");
        assert!(
            MusicEffect::specs()
                .iter()
                .any(|effect| effect.id == "open-file"),
            "the picker is offered beside the library's actions"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "music.playlist"
                    && child.actions.iter().any(|action| action.id == "rename")),
            "playlist document actions ride Resource::children"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "music.view"),
            "navigation destinations are published children"
        );
        assert!(
            !published
                .children
                .iter()
                .any(|child| matches!(child.kind.as_str(), "music.player" | "music.queue")),
            "player/queue are device-local and must not duplicate under publish"
        );

        let extras = live_device_resources(&app.music_resource_ctx());
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "music.player")
        );
        assert!(extras.iter().any(|resource| resource.kind == "music.queue"));
        assert!(
            !extras
                .iter()
                .any(|resource| resource.kind == "music.library"),
            "hosted extras must not republish the library root"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn library_resources_publish_bounded_safe_canonical_views() {
        // Phase `ctrl_k_publishes_the_library_tree`.
        // Ctrl+K on Music yields the tree: the library with a "New playlist"
        // action, a `music.playlist` child per playlist exporting rename + delete,
        // and a `music.track` child per track in a playlist exporting
        // remove + toggle-like. Drives the real resource hook.
        {
            let (app, doc) = seeded();
            let resources = app.palette_tree(&doc);
            assert_eq!(resources.len(), 1, "one library root");
            let lib = &resources[0];
            assert_eq!(lib.kind, "music.library");
            let lib_actions: Vec<&str> = lib.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(lib_actions.contains(&"new-playlist"), "{lib_actions:?}");

            let playlist = lib
                .children
                .iter()
                .find(|r| r.kind == "music.playlist")
                .expect("a playlist resource");
            let pl_actions: Vec<&str> = playlist.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(pl_actions.contains(&"rename"), "{pl_actions:?}");
            assert!(pl_actions.contains(&"delete"), "{pl_actions:?}");
            assert_eq!(
                playlist
                    .actions
                    .iter()
                    .find(|a| a.id == "delete")
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

            // A playlist with tracks nests a membership resource with remove,
            // reorder and like. It is not a duplicate of the catalog track entity.
            let with_tracks = lib
                .children
                .iter()
                .find(|r| !r.children.is_empty())
                .expect("a playlist with tracks");
            let track = &with_tracks.children[0];
            assert_eq!(track.kind, "music.playlist-track");
            let tk_actions: Vec<&str> = track.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(tk_actions.contains(&"remove"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"toggle-like"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"move-up"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"move-down"), "{tk_actions:?}");

            let album = lib
                .children
                .iter()
                .find(|r| r.kind == "music.album")
                .expect("album resource");
            assert!(
                album
                    .actions
                    .iter()
                    .any(|action| action.id == "toggle-save")
            );
            let artist = lib
                .children
                .iter()
                .find(|r| r.kind == "music.artist")
                .expect("artist resource");
            assert!(
                artist
                    .actions
                    .iter()
                    .any(|action| action.id == "toggle-follow")
            );

            // The full flattened tree exports every action, including the nested
            // track ops that sort after the playlists.
            let flat: Vec<&str> = tilo_actions::flatten(&resources)
                .iter()
                .map(|f| f.action.label.as_str())
                .collect();
            assert!(flat.contains(&"New playlist"), "{flat:?}");
            assert!(flat.contains(&"Delete playlist"), "{flat:?}");
            assert!(flat.contains(&"Remove from playlist"), "{flat:?}");
        }

        // Phase `contextual_resources_are_bounded_but_late_catalog_items_resolve_lazily`.
        {
            let mut ctx = MusicResourceCtx::default();
            for number in 0..(OPEN_RESOURCE_CATEGORY_LIMIT + 9) {
                let id = format!("track-{number:03}");
                ctx.tracks.push(CatalogTrackResource {
                    label: format!("Track {number}"),
                    artist: String::new(),
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
                artist: String::new(),
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
                let permission = flat.action.contract.permission;
                match flat.action.kind {
                    OperationKind::Document => assert_eq!(
                        permission,
                        PermissionRequirement::Write,
                        "{} must require write permission",
                        flat.key
                    ),
                    OperationKind::View => assert_eq!(
                        permission,
                        PermissionRequirement::Read,
                        "{} must require read permission",
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
                assert_eq!(open.contract.permission, PermissionRequirement::Read);
                let encoded = spec_encoding_of(open);
                assert_eq!(
                    MusicEffect::from_payload(&encoded.payload).expect("effect decodes"),
                    MusicEffect::Open {
                        place: Place::View(id.to_string())
                    }
                );
                assert_eq!(
                    open_view_address(id).expect("known view address").resource,
                    resource_id
                );
                assert_eq!(
                    resource_by_id(&ctx, &resource_id).map(|r| r.id.as_str().to_string()),
                    Some(resource_id)
                );
            }
            assert!(parse_view_resource_id("music.view:../../etc/passwd").is_none());
            assert!(open_view_address("../../etc/passwd").is_none());
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn playlist_creation_and_covers_are_portable_and_validated() {
        // Phase `portable_playlist_cover_replays_to_a_peer_without_a_local_path`.
        {
            let (_app, seeded_doc) = seeded();
            let mut a = seeded_doc;
            a.commit();
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let playlist = crate::doc::load_library(&a).playlists[0].id.clone();
            // A tiny valid PNG, representative of the normalized portal output.
            let cover = tiny_portable_png();
            assert!(portable_cover_value(&cover));
            assert!(!cover.contains("file:"));
            assert!(!cover.contains("/tmp/"));

            let action = set_cover_action(&playlist, &cover);
            let envelope = tilo_actions::apply_local(&mut a, &action, encode(&action)).unwrap();
            tilo_actions::replay(&mut b, &envelope, &registry()).unwrap();

            assert_eq!(
                a.get_heads(),
                b.get_heads(),
                "cover replay converges exactly"
            );
            assert_eq!(
                crate::doc::load_library(&b)
                    .playlist(&playlist)
                    .unwrap()
                    .cover,
                cover,
                "the peer receives a portable value, not the picker's local path"
            );
        }

        // Phase `playlist_cover_action_refuses_local_unsafe_and_malformed_values`.
        {
            let (_app, mut doc) = seeded();
            let playlist = crate::doc::load_library(&doc).playlists[0].id.clone();
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
                let action = set_cover_action(&playlist, cover);
                assert!(
                    matches!(action.apply(&mut doc), Err(ActionError::Refused(_))),
                    "must refuse hostile/nonportable cover {cover:?}"
                );
            }
        }

        // Phase `new_playlist_action_creates_a_playlist`.
        // Dispatching the "New playlist" prompted action creates a playlist with the
        // committed name (the id is minted at authoring time, so this asserts the
        // effect, not byte-equality against a second freshly-minted id).
        {
            let (app, mut doc) = seeded();
            let resources = app.palette_tree(&doc);
            let spec = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "new-playlist")
                .unwrap();
            let before = crate::doc::load_library(&doc).playlists.len();

            let encoded = prompted_encode(spec, "Late Night");
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let lib = crate::doc::load_library(&doc);
            assert_eq!(
                lib.playlists.len(),
                before + 1,
                "the action added a playlist"
            );
            assert!(
                lib.playlists.iter().any(|p| p.name == "Late Night"),
                "the created playlist carries the committed name"
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
                    action.contract.permission,
                    PermissionRequirement::Write,
                    "{id}"
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
            let track = CatalogTrackResource {
                id: "track-2".into(),
                label: "Track 2".into(),
                artist: String::new(),
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
            let (mut app, doc) = seeded();
            let resources = app.palette_tree(&doc);
            let library = &resources[0];
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

            let playlist = library
                .children
                .iter()
                .find(|resource| resource.kind == "music.playlist")
                .unwrap();
            let add_track = playlist
                .actions
                .iter()
                .find(|action| action.id == "add-track")
                .unwrap();
            let encoded = add_track
                .build_args([(
                    String::from("track"),
                    ArgValue::Resource("music.track:removed".into()),
                )])
                .unwrap();
            assert_eq!(
                encoded.type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE
            );
        }

        // Phase `stale_replayed_library_actions_are_refused`.
        {
            let (_app, mut doc) = seeded();
            let stale = add_track_action("gone", "gone", 0);
            assert!(matches!(
                stale.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn playlist_membership_actions_resolve_lazily_and_converge() {
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
                        rank: i64::try_from(number).unwrap_or(i64::MAX / 1_048_576) * 1_048_576,
                    })
                    .collect(),
            };
            let target = playlist.tracks.last().cloned().expect("late membership");
            let index = playlist.tracks.len() - 1;
            let ctx = MusicResourceCtx {
                playlists: vec![playlist.clone()],
                membership: Some(PlaylistMembershipResource {
                    playlist: playlist.id.clone(),
                    track: target.clone(),
                    index,
                    total: playlist.tracks.len(),
                    up_rank: playlist.move_rank(index, index.saturating_sub(1)),
                    down_rank: playlist.move_rank(index, index.saturating_add(1)),
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
            let id = playlist_track_resource_id(&playlist.id, &target.id);
            assert_eq!(
                parse_playlist_track_resource_id(&id),
                Some((playlist.id.clone(), target.id.clone())),
                "membership identity is Unicode-safe and reversible"
            );
            let direct = resource_by_id(&ctx, &id).expect("late membership resolves directly");
            let remove = direct
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .unwrap();
            assert_eq!(
                spec_encoding_of(remove),
                encode(&remove_track_action(&playlist.id, &target.id)),
                "the lazy resource uses the exact same canonical remove action"
            );
            let up = direct
                .actions
                .iter()
                .find(|action| action.id == "move-up")
                .unwrap();
            assert_eq!(
                spec_encoding_of(up),
                encode(&move_track_action(
                    &playlist.id,
                    &target.id,
                    ctx.membership.as_ref().unwrap().up_rank
                )),
                "the lazy resource preserves the original membership index"
            );
        }

        // Phase `stale_membership_remove_is_refused_and_absolute_reorder_is_idempotent`.
        {
            let (_app, mut doc) = seeded();
            let playlist = crate::doc::load_library(&doc).playlists[0].clone();
            let track = playlist.track_ids[0].clone();
            let remove = remove_track_action(&playlist.id, &track);
            remove.apply(&mut doc).expect("live membership removes");
            assert!(matches!(
                remove.apply(&mut doc),
                Err(ActionError::Refused(_))
            ));

            let playlist = crate::doc::load_library(&doc).playlists[0].clone();
            let track = playlist.track_ids[0].clone();
            let rank = playlist.rank_for_move(&track, 1).unwrap();
            let move_action = move_track_action(&playlist.id, &track, rank);
            move_action.apply(&mut doc).expect("live membership moves");
            move_action
                .apply(&mut doc)
                .expect("absolute membership move safely replays");
        }

        // Phase `encoded_divergent_playlist_edits_converge_by_stable_membership`.
        {
            let (_app, mut base) = seeded();
            base.commit();
            let bytes = base.save();
            let mut a = AutoCommit::load(&bytes).unwrap();
            let mut b = AutoCommit::load(&bytes).unwrap();
            let playlist = crate::doc::load_library(&a).playlists[0].clone();
            let moved = playlist.track_ids[0].clone();
            let removed = playlist.track_ids[1].clone();
            let rank = playlist
                .rank_for_move(&moved, playlist.track_ids.len() - 1)
                .unwrap();

            let move_action = move_track_action(&playlist.id, &moved, rank);
            let remove_action = remove_track_action(&playlist.id, &removed);
            tilo_actions::apply_local(&mut a, &move_action, encode(&move_action)).unwrap();
            tilo_actions::apply_local(&mut b, &remove_action, encode(&remove_action)).unwrap();

            let a_bytes = a.save();
            let b_bytes = b.save();
            let mut left = AutoCommit::load(&a_bytes).unwrap();
            let mut right_for_left = AutoCommit::load(&b_bytes).unwrap();
            left.merge_incoming(&mut right_for_left).unwrap();
            let mut right = AutoCommit::load(&b_bytes).unwrap();
            let mut left_for_right = AutoCommit::load(&a_bytes).unwrap();
            right.merge_incoming(&mut left_for_right).unwrap();

            assert_eq!(left.get_heads(), right.get_heads());
            let left_playlist = crate::doc::load_library(&left)
                .playlist(&playlist.id)
                .unwrap()
                .clone();
            let right_playlist = crate::doc::load_library(&right)
                .playlist(&playlist.id)
                .unwrap()
                .clone();
            assert_eq!(left_playlist.track_ids, right_playlist.track_ids);
            assert!(!left_playlist.track_ids.contains(&removed));
            assert_eq!(left_playlist.track_ids.last(), Some(&moved));
        }
    }
}
