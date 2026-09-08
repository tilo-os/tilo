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
//! Presentation stays local: selection, scroll, search text, filter tabs,
//! playback and cache/download policy.
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
//! [`Action::apply`] never touches the RNG or the wall-clock. Toggles
//! (like / saved-album / followed-artist) read the document's current membership
//! and flip it, so two replicas at the same base heads land on the same result.
//! A malformed or stale payload is refused before it changes the document. This
//! prevents an external caller from recording a successful-looking history
//! entry that actually wrote a dangling library reference.

use std::collections::BTreeSet;

use automerge::AutoCommit;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::resource::INVALID_ARGUMENTS_TYPE;
use qos_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, PermissionRequirement,
    Resource, ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::doc as mdoc;

/// Registry `type_id` for every Music action.
pub const MUSIC_ACTION_TYPE: &str = "music.action";
pub const MUSIC_OPERATION_TYPE: &str = "music.operation";
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
    ("songs", "Songs"),
    ("albums", "Albums"),
    ("artists", "Artists"),
    ("playlists", "Playlists"),
    ("downloads", "Downloads"),
    ("liked", "Liked Songs"),
    ("search", "Search"),
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

/// Non-document operations exported by Music. Navigation changes only the
/// local presentation. Playback is intentionally absent: its durable controls
/// are collaborative [`MusicAction`] values, while stream resolution and audio
/// output remain device effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MusicOperation {
    /// Open one of the stable app-owned destinations in [`NAVIGATION_VIEWS`].
    /// The receiver validates `id` again before it changes local presentation.
    OpenView {
        id: String,
    },
    PlayTrack {
        id: String,
    },
    OpenAlbum {
        id: String,
    },
    OpenArtist {
        id: String,
    },
    OpenPlaylist {
        id: String,
    },
    /// Navigate to a playlist and reveal the still-current membership row.
    OpenPlaylistMembership {
        playlist: String,
        track: String,
    },
    /// Apply a fully specified transport snapshot on this device only. This is
    /// an effect (not a document action): opening a search hit must not make a
    /// peer start playing audio or inherit this device's volume/queue.
    SetPlayback {
        playback: crate::model::Playback,
    },
    /// Download durable, device-local media bytes or remove them from the
    /// device-local cache. Neither the bytes nor their paths enter Automerge.
    SetOffline {
        tracks: Vec<String>,
        keep: bool,
    },
}

/// Decode a host-routed Music operation.
///
/// # Errors
///
/// Returns the JSON decoding error for a payload addressed to Music's
/// operation type. Payloads for other operation types return `Ok(None)`.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<MusicOperation>, String> {
    if type_id != MUSIC_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn encode_operation(operation: &MusicOperation) -> EncodedAction {
    match serde_json::to_vec(operation) {
        Ok(payload) => EncodedAction {
            type_id: MUSIC_OPERATION_TYPE.to_string(),
            payload,
        },
        Err(error) => invalid_arguments(&error.to_string()),
    }
}

/// Every collaborative operation Music exports on its shared library.
///
/// Each variant is a whole-operation write carrying enough payload to replay
/// deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MusicAction {
    /// Create a new empty playlist. `id` is minted at authoring time and carried
    /// here so replay reproduces the byte-identical row; `apply` never mints one.
    CreatePlaylist { id: String, name: String },
    /// Rename a playlist.
    RenamePlaylist { id: String, name: String },
    /// Delete a playlist by id.
    DeletePlaylist { id: String },
    /// Append a track to a playlist (idempotent — a track already present is a
    /// no-op, matching the on-screen affordance).
    AddTrackToPlaylist { playlist: String, track: String },
    /// Remove the first occurrence of a track from a playlist.
    RemoveTrackFromPlaylist { playlist: String, track: String },
    /// Reorder a track within a playlist from `from` to `to` (clamped).
    MoveTrackInPlaylist {
        playlist: String,
        /// Identity of the membership expected at `from`. This prevents a
        /// stale resource from moving whichever row later occupied its index.
        track: String,
        from: usize,
        to: usize,
    },
    /// Set (or clear, with an empty string) a playlist's portable cover value.
    /// The *pick* (the native file portal) is app-side, non-deterministic work;
    /// it is normalized into an HTTPS URL or bounded PNG data URL before this
    /// deterministic, replayable action is written to the shared document.
    SetPlaylistCover { playlist: String, cover: String },
    /// Toggle a track's membership in "Liked Songs". Deterministic against the
    /// replay base: the current membership is read from the document and flipped.
    ToggleLiked { track: String },
    /// Toggle an album's membership in the saved-to-library set.
    ToggleSavedAlbum { album: String },
    /// Toggle whether the user follows an artist.
    ToggleFollowedArtist { artist: String },
}

/// Durable, browseable history metadata for a library mutation.
pub fn history_context(action: &MusicAction) -> qos_actions::ActionContext {
    let mut context = qos_actions::ActionContext::for_action("org.qos.Music", "Music", action);
    let (label, id, kind, resource_label): (&str, &str, &str, &str) = match action {
        MusicAction::CreatePlaylist { id, name } => ("Create playlist", id, "music.playlist", name),
        MusicAction::RenamePlaylist { id, name } => ("Rename playlist", id, "music.playlist", name),
        MusicAction::DeletePlaylist { id } => ("Delete playlist", id, "music.playlist", id),
        MusicAction::AddTrackToPlaylist { playlist, track: _ } => (
            "Add track to playlist",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::RemoveTrackFromPlaylist { playlist, track: _ } => (
            "Remove track from playlist",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::MoveTrackInPlaylist { playlist, .. } => {
            ("Reorder playlist", playlist, "music.playlist", playlist)
        }
        MusicAction::SetPlaylistCover { playlist, .. } => (
            "Change playlist cover",
            playlist,
            "music.playlist",
            playlist,
        ),
        MusicAction::ToggleLiked { track } => ("Toggle liked song", track, "music.track", track),
        MusicAction::ToggleSavedAlbum { album } => {
            ("Toggle saved album", album, "music.album", album)
        }
        MusicAction::ToggleFollowedArtist { artist } => {
            ("Toggle followed artist", artist, "music.artist", artist)
        }
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
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
            MusicAction::AddTrackToPlaylist { playlist, track } => {
                require(
                    mdoc::has_playlist(doc, playlist) && mdoc::has_track(doc, track),
                    "playlist or track is no longer in the library",
                )?;
                mdoc::add_track_to_playlist(doc, playlist, track);
            }
            MusicAction::RemoveTrackFromPlaylist { playlist, track } => {
                require(
                    mdoc::has_playlist(doc, playlist) && mdoc::has_track(doc, track),
                    "playlist or track is no longer in the library",
                )?;
                require(
                    mdoc::playlist_contains_track(doc, playlist, track),
                    "playlist membership is no longer present",
                )?;
                mdoc::remove_track_from_playlist(doc, playlist, track);
            }
            MusicAction::MoveTrackInPlaylist {
                playlist,
                track,
                from,
                to,
            } => {
                require(
                    mdoc::has_playlist(doc, playlist),
                    "playlist is no longer in the library",
                )?;
                require(
                    mdoc::playlist_track_at(doc, playlist, *from).as_deref() == Some(track),
                    "playlist membership moved or is no longer present",
                )?;
                mdoc::move_track_in_playlist(doc, playlist, *from, *to);
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
            MusicAction::ToggleLiked { track } => {
                require(
                    mdoc::has_track(doc, track),
                    "track is no longer in the library",
                )?;
                mdoc::toggle_liked(doc, track);
            }
            MusicAction::ToggleSavedAlbum { album } => {
                require(
                    mdoc::has_album(doc, album),
                    "album is no longer in the library",
                )?;
                mdoc::toggle_saved_album(doc, album);
            }
            MusicAction::ToggleFollowedArtist { artist } => {
                require(
                    mdoc::has_artist(doc, artist),
                    "artist is no longer in the library",
                )?;
                mdoc::toggle_followed_artist(doc, artist);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
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

pub fn add_track_action(playlist: &str, track: &str) -> MusicAction {
    MusicAction::AddTrackToPlaylist {
        playlist: playlist.to_string(),
        track: track.to_string(),
    }
}

pub fn remove_track_action(playlist: &str, track: &str) -> MusicAction {
    MusicAction::RemoveTrackFromPlaylist {
        playlist: playlist.to_string(),
        track: track.to_string(),
    }
}

pub fn move_track_action(playlist: &str, track: &str, from: usize, to: usize) -> MusicAction {
    MusicAction::MoveTrackInPlaylist {
        playlist: playlist.to_string(),
        track: track.to_string(),
        from,
        to,
    }
}

pub fn set_cover_action(playlist: &str, cover: &str) -> MusicAction {
    MusicAction::SetPlaylistCover {
        playlist: playlist.to_string(),
        cover: cover.to_string(),
    }
}

pub fn toggle_liked_action(track: &str) -> MusicAction {
    MusicAction::ToggleLiked {
        track: track.to_string(),
    }
}

pub fn toggle_saved_album_action(album: &str) -> MusicAction {
    MusicAction::ToggleSavedAlbum {
        album: album.to_string(),
    }
}

pub fn toggle_followed_artist_action(artist: &str) -> MusicAction {
    MusicAction::ToggleFollowedArtist {
        artist: artist.to_string(),
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
}

/// The presentation context Music hands [`provide_resources`]: the user's
/// playlists and their tracks currently in the model cache. Everything here is
/// derived from state the document already holds; nothing is fabricated.
#[derive(Debug, Clone, Default)]
pub struct MusicResourceCtx {
    pub playlists: Vec<PlaylistResource>,
    pub tracks: Vec<CatalogTrackResource>,
    pub albums: Vec<CatalogAlbumResource>,
    pub artists: Vec<CatalogArtistResource>,
    pub membership: Option<PlaylistMembershipResource>,
    pub playback: crate::model::Playback,
}

#[allow(
    clippy::expect_used,
    reason = "action is always a MusicAction, which encodes losslessly (see encode()'s own allow)"
)]
fn immediate_spec(id: &str, label: &str, action: &MusicAction) -> ActionSpec {
    ActionSpec::immediate(id, label, MUSIC_ACTION_TYPE, action)
        .expect("encode music action")
        .requiring(PermissionRequirement::Write)
        .reversible()
}

fn operation_spec(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &MusicOperation,
) -> ActionSpec {
    #[allow(
        clippy::expect_used,
        reason = "MusicOperation contains only serde-supported owned values and is covered by operation round-trip tests"
    )]
    let spec = ActionSpec::operation(id, label, kind, MUSIC_OPERATION_TYPE, operation)
        .expect("encode music operation");
    if kind == OperationKind::View {
        spec.requiring(PermissionRequirement::Read)
            .retry_policy(RetryPolicy::Idempotent)
    } else if kind == OperationKind::Effect {
        spec.requiring(PermissionRequirement::Write)
            .retry_policy(RetryPolicy::Idempotent)
    } else {
        spec
    }
}

fn local_playback_spec(
    id: &str,
    label: &str,
    previous: &crate::model::Playback,
    playback: &crate::model::Playback,
) -> ActionSpec {
    let spec = operation_spec(
        id,
        label,
        OperationKind::Effect,
        &MusicOperation::SetPlayback {
            playback: playback.clone(),
        },
    );
    if playback.playing && playback.current != previous.current {
        spec.execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["state"],
                    "properties": {
                        "state": { "enum": ["resolved", "playing"] }
                    }
                }),
            })
    } else {
        spec
    }
}

fn local_offline_spec(id: &str, label: &str, tracks: Vec<String>, keep: bool) -> ActionSpec {
    let spec = operation_spec(
        id,
        label,
        OperationKind::Effect,
        &MusicOperation::SetOffline { tracks, keep },
    );
    spec.execution(ExecutionMode::Background { cancellable: false })
        .returns(ResultSpec::Json {
            schema: serde_json::json!({
                "type": "object",
                "required": ["state"],
                "properties": {
                    "state": { "enum": ["cached", "removed"] }
                }
            }),
        })
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
    Resource::new(view_resource_id(id), "music.view", label).with_action(operation_spec(
        "open",
        &format!("Open {label}"),
        OperationKind::View,
        &MusicOperation::OpenView { id: id.to_string() },
    ))
}

fn with_playback_actions(
    root: Resource,
    playback: &crate::model::Playback,
    known_tracks: &BTreeSet<String>,
) -> Resource {
    let root = with_transport_settings(root, playback);
    let root = with_transport_navigation(root, playback);
    with_queue_actions(root, playback, known_tracks)
}

fn with_transport_settings(mut root: Resource, playback: &crate::model::Playback) -> Resource {
    if playback.current.is_some() {
        let mut toggled = playback.clone();
        toggled.playing = !toggled.playing;
        root = root.with_action(local_playback_spec(
            "play-pause",
            if playback.playing { "Pause" } else { "Play" },
            playback,
            &toggled,
        ));
        let seek = playback.clone();
        root = root.with_action(
            ActionSpec::typed_operation(
                "seek",
                "Seek playback",
                "milliseconds",
                OperationKind::Effect,
                MUSIC_OPERATION_TYPE,
                vec![ActionParam::required(
                    "position_ms",
                    ParamType::Integer,
                    "absolute playback position in milliseconds",
                )],
                move |args: &ActionArgs| {
                    let mut next = seek.clone();
                    next.position_ms = args.integer("position_ms").unwrap_or(0).max(0) as u64;
                    encode_operation(&MusicOperation::SetPlayback { playback: next })
                },
            )
            .requiring(PermissionRequirement::Write),
        );
    }
    let volume = playback.clone();
    root = root.with_action(
        ActionSpec::typed_operation(
            "set-volume",
            "Set volume",
            "0–100",
            OperationKind::Effect,
            MUSIC_OPERATION_TYPE,
            vec![
                ActionParam::required("volume", ParamType::Integer, "volume from 0 to 100")
                    .with_schema(serde_json::json!({"minimum": 0, "maximum": 100})),
            ],
            move |args: &ActionArgs| {
                let mut next = volume.clone();
                next.volume = args
                    .integer("volume")
                    .unwrap_or_else(|| i64::from(next.volume))
                    .clamp(0, 100) as u8;
                encode_operation(&MusicOperation::SetPlayback { playback: next })
            },
        )
        .requiring(PermissionRequirement::Write),
    );
    let mut muted = playback.clone();
    muted.volume = if muted.volume == 0 { 80 } else { 0 };
    root = root.with_action(local_playback_spec(
        "toggle-mute",
        "Toggle mute",
        playback,
        &muted,
    ));
    let mut shuffled = playback.clone();
    shuffled.shuffle = !shuffled.shuffle;
    root = root.with_action(local_playback_spec(
        "toggle-shuffle",
        if playback.shuffle {
            "Turn shuffle off"
        } else {
            "Turn shuffle on"
        },
        playback,
        &shuffled,
    ));
    let mut repeated = playback.clone();
    repeated.repeat = repeated.repeat.next();
    root = root.with_action(local_playback_spec(
        "cycle-repeat",
        "Change repeat mode",
        playback,
        &repeated,
    ));

    root
}

fn with_transport_navigation(mut root: Resource, playback: &crate::model::Playback) -> Resource {
    let mut previous = playback.clone();
    if previous.queue_index > 0 {
        previous.queue_index -= 1;
        previous.current = previous.queue.get(previous.queue_index).cloned();
    }
    previous.position_ms = 0;
    let previous_spec = local_playback_spec("previous", "Previous track", playback, &previous);
    root = root.with_action(if playback.current.is_none() {
        previous_spec.unavailable(
            "nothing_playing",
            "Choose a track before using playback controls",
        )
    } else if playback.queue.is_empty() {
        previous_spec.unavailable("empty_queue", "There are no queued tracks")
    } else {
        previous_spec
    });

    // Music's on-screen shuffle permutation is intentionally device-local so
    // it never leaks a random, non-replayable decision into the CRDT. A remote
    // caller can still select a concrete track, but a generic "next" would be
    // dishonest while that local permutation is active.
    let mut next = playback.clone();
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
    let next_spec = local_playback_spec("next", "Next queued track", playback, &next);
    root = root.with_action(if playback.current.is_none() {
        next_spec.unavailable(
            "nothing_playing",
            "Choose a track before using playback controls",
        )
    } else if playback.queue.is_empty() {
        next_spec.unavailable("empty_queue", "There are no queued tracks")
    } else if playback.shuffle {
        next_spec.unavailable(
            "local_shuffle_order",
            "Choose a concrete track while this device's shuffle order is active",
        )
    } else {
        next_spec
    });
    root
}

fn track_id_from_resource(address: &str) -> Option<&str> {
    address
        .strip_prefix("music.track:")
        .filter(|id| !id.is_empty())
}

fn with_queue_actions(
    mut root: Resource,
    playback: &crate::model::Playback,
    known_tracks: &BTreeSet<String>,
) -> Resource {
    let add_queue = playback.clone();
    let queue_track_ids = known_tracks.clone();
    root = root.with_action(
        ActionSpec::typed_operation(
            "queue-add",
            "Add track to queue",
            "track id…",
            OperationKind::Effect,
            MUSIC_OPERATION_TYPE,
            vec![ActionParam::required(
                "track",
                ParamType::Resource,
                "stable music.track resource address",
            )],
            move |args: &ActionArgs| {
                let Some(track) = args.text("track").and_then(track_id_from_resource) else {
                    return invalid_arguments("track must be a music.track resource");
                };
                if !queue_track_ids.contains(track) {
                    return invalid_arguments("track is no longer in the library");
                }
                let mut next = add_queue.clone();
                next.queue.push(track.to_string());
                encode_operation(&MusicOperation::SetPlayback { playback: next })
            },
        )
        .requiring(PermissionRequirement::Write),
    );
    let remove_queue = playback.clone();
    let remove_track_ids = known_tracks.clone();
    let remove = ActionSpec::typed_operation(
        "queue-remove",
        "Remove track from queue",
        "track id…",
        OperationKind::Effect,
        MUSIC_OPERATION_TYPE,
        vec![ActionParam::required(
            "track",
            ParamType::Resource,
            "stable music.track resource address",
        )],
        move |args: &ActionArgs| {
            let Some(track) = args.text("track").and_then(track_id_from_resource) else {
                return invalid_arguments("track must be a music.track resource");
            };
            if !remove_track_ids.contains(track) {
                return invalid_arguments("track is no longer in the library");
            }
            let mut next = remove_queue.clone();
            let Some(index) = next.queue.iter().position(|queued| queued == track) else {
                return invalid_arguments("track is not in the current queue");
            };
            next.queue.remove(index);
            if index < next.queue_index {
                next.queue_index -= 1;
            } else if index == next.queue_index {
                if next.queue.is_empty() {
                    next.current = None;
                    next.playing = false;
                    next.queue_index = 0;
                    next.position_ms = 0;
                } else {
                    next.queue_index = next.queue_index.min(next.queue.len() - 1);
                    next.current = next.queue.get(next.queue_index).cloned();
                    next.position_ms = 0;
                }
            }
            encode_operation(&MusicOperation::SetPlayback { playback: next })
        },
    )
    .requiring(PermissionRequirement::Write)
    .destructive();
    root = root.with_action(if playback.queue.is_empty() {
        remove.unavailable("empty_queue", "There are no queued tracks to remove")
    } else {
        remove
    });
    root
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
    .with_action(operation_spec(
        "open",
        "Open playlist",
        OperationKind::View,
        &MusicOperation::OpenPlaylist {
            id: playlist.id.clone(),
        },
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
                encode(&add_track_action(&add_playlist, track))
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
        ));
    }
    resource
}

fn playlist_track_resource(
    playlist: &str,
    track: &TrackRef,
    index: usize,
    total: usize,
) -> Resource {
    Resource::new(
        playlist_track_resource_id(playlist, &track.id),
        "music.playlist-track",
        track.label.clone(),
    )
    .with_action(operation_spec(
        "open",
        "Open in playlist",
        OperationKind::View,
        &MusicOperation::OpenPlaylistMembership {
            playlist: playlist.to_string(),
            track: track.id.clone(),
        },
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
        &toggle_liked_action(&track.id),
    ))
    .with_action(if index > 0 {
        immediate_spec(
            "move-up",
            "Move track up",
            &move_track_action(playlist, &track.id, index, index - 1),
        )
        .reversible()
    } else {
        immediate_spec(
            "move-up",
            "Move track up",
            &move_track_action(playlist, &track.id, index, index),
        )
        .unavailable("first_track", "This track is already first in the playlist")
    })
    .with_action(if index + 1 < total {
        immediate_spec(
            "move-down",
            "Move track down",
            &move_track_action(playlist, &track.id, index, index + 1),
        )
        .reversible()
    } else {
        immediate_spec(
            "move-down",
            "Move track down",
            &move_track_action(playlist, &track.id, index, index),
        )
        .unavailable("last_track", "This track is already last in the playlist")
    })
}

pub(crate) fn catalog_track_resource(
    track: &CatalogTrackResource,
    playback: &crate::model::Playback,
) -> Resource {
    let mut next = playback.clone();
    next.queue = vec![track.id.clone()];
    next.queue_index = 0;
    next.current = Some(track.id.clone());
    next.position_ms = 0;
    next.playing = true;
    let mut resource = Resource::new(
        track_resource_id(&track.id),
        "music.track",
        track.label.clone(),
    )
    .with_action(local_playback_spec("play", "Play track", playback, &next))
    .with_action(local_offline_spec(
        if track.offline {
            "remove-offline"
        } else {
            "keep-offline"
        },
        if track.offline {
            "Remove from offline cache"
        } else {
            "Keep offline"
        },
        vec![track.id.clone()],
        !track.offline,
    ))
    .with_action(immediate_spec(
        "toggle-like",
        if track.liked {
            "Unlike song"
        } else {
            "Like song"
        },
        &toggle_liked_action(&track.id),
    ))
    .with_action({
        let mut queued = playback.clone();
        queued.queue.push(track.id.clone());
        local_playback_spec("add-to-queue", "Add to queue", playback, &queued)
    })
    .with_action({
        let mut next = playback.clone();
        if next.queue.is_empty() || next.current.is_none() {
            next.queue = vec![track.id.clone()];
            next.current = Some(track.id.clone());
            next.queue_index = 0;
            next.playing = true;
            next.position_ms = 0;
        } else {
            let at = (next.queue_index + 1).min(next.queue.len());
            next.queue.insert(at, track.id.clone());
        }
        local_playback_spec("play-next", "Play next", playback, &next)
    });
    if !track.album_id.is_empty() {
        resource = resource.with_action(operation_spec(
            "open-album",
            "Open album",
            OperationKind::View,
            &MusicOperation::OpenAlbum {
                id: track.album_id.clone(),
            },
        ));
    }
    if !track.artist_id.is_empty() {
        resource = resource.with_action(operation_spec(
            "open-artist",
            "Open artist",
            OperationKind::View,
            &MusicOperation::OpenArtist {
                id: track.artist_id.clone(),
            },
        ));
    }
    resource
}

fn catalog_album_resource(album: &CatalogAlbumResource) -> Resource {
    Resource::new(
        album_resource_id(&album.id),
        "music.album",
        album.label.clone(),
    )
    .with_action(operation_spec(
        "open",
        "Open album",
        OperationKind::View,
        &MusicOperation::OpenAlbum {
            id: album.id.clone(),
        },
    ))
    .with_action(immediate_spec(
        "toggle-save",
        if album.saved {
            "Remove album from library"
        } else {
            "Save album to library"
        },
        &toggle_saved_album_action(&album.id),
    ))
}

fn catalog_artist_resource(artist: &CatalogArtistResource) -> Resource {
    Resource::new(
        artist_resource_id(&artist.id),
        "music.artist",
        artist.label.clone(),
    )
    .with_action(operation_spec(
        "open",
        "Open artist",
        OperationKind::View,
        &MusicOperation::OpenArtist {
            id: artist.id.clone(),
        },
    ))
    .with_action(immediate_spec(
        "toggle-follow",
        if artist.followed {
            "Unfollow artist"
        } else {
            "Follow artist"
        },
        &toggle_followed_artist_action(&artist.id),
    ))
}

/// Build the resource tree Music currently has open: the library (with a "New
/// playlist" action), each playlist (rename + delete), and each track within a
/// playlist (remove + toggle-like). Every document-mutating operation is
/// exported as the same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &MusicResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let known_tracks = ctx
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect::<BTreeSet<_>>();
    let mut root = Resource::new("music.library", "music.library", "Library");
    root = root.with_child(with_playback_actions(
        Resource::new("music.player:player", "music.player", "Player"),
        &ctx.playback,
        &known_tracks,
    ));

    // Library-level: create a playlist. The id is minted once when the name
    // argument is committed, then baked into the envelope, so replay stays
    // deterministic.
    root = root.with_action(
        ActionSpec::prompted(
            "new-playlist",
            "New playlist",
            "Name",
            "playlist name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let name = if name.trim().is_empty() {
                    "New Playlist"
                } else {
                    name.trim()
                };
                encode(&create_playlist_action(&new_playlist_id(), name))
            },
        )
        .requiring(PermissionRequirement::Write)
        .reversible(),
    );

    let mut playlists = ctx.playlists.iter().collect::<Vec<_>>();
    playlists.sort_by(|a, b| a.id.cmp(&b.id));
    for playlist in playlists.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        root = root.with_child(playlist_resource(
            playlist,
            &known_tracks,
            OPEN_PLAYLIST_TRACK_LIMIT,
        ));
    }

    let mut tracks = ctx.tracks.iter().collect::<Vec<_>>();
    tracks.sort_by(|a, b| a.id.cmp(&b.id));
    for track in tracks.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        root = root.with_child(catalog_track_resource(track, &ctx.playback));
    }
    let mut albums = ctx.albums.iter().collect::<Vec<_>>();
    albums.sort_by(|a, b| a.id.cmp(&b.id));
    for album in albums.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        root = root.with_child(catalog_album_resource(album));
    }
    let mut artists = ctx.artists.iter().collect::<Vec<_>>();
    artists.sort_by(|a, b| a.id.cmp(&b.id));
    for artist in artists.into_iter().take(OPEN_RESOURCE_CATEGORY_LIMIT) {
        root = root.with_child(catalog_artist_resource(artist));
    }

    // App destinations are first-class resources too. A palette, agent or
    // history item can therefore reopen the exact screen through the same
    // canonical operation broker used for albums, artists and playlists. Keep
    // them after document resources so Ctrl+K's unfiltered initial window still
    // prioritises the library actions users can immediately perform.
    for &(id, label) in NAVIGATION_VIEWS {
        root = root.with_child(navigation_view_resource(id, label));
    }

    vec![root]
}

/// Resolve one catalog resource without first constructing the entire library
/// tree. Search rows carry canonical resource/action addresses; at invocation
/// time the host calls this method against the current model, so deleted or
/// otherwise stale targets disappear instead of gaining authority from an old
/// palette snapshot.
pub fn resource_by_id(ctx: &MusicResourceCtx, id: &str) -> Option<Resource> {
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
        ));
    }
    let known_tracks = ctx
        .tracks
        .iter()
        .map(|track| track.id.clone())
        .collect::<BTreeSet<_>>();
    if id == "music.library" {
        return provide_resources(ctx, &AutoCommit::new())
            .into_iter()
            .next();
    }
    if id == "music.player:player" {
        return Some(with_playback_actions(
            Resource::new("music.player:player", "music.player", "Player"),
            &ctx.playback,
            &known_tracks,
        ));
    }
    if let Some(track_id) = id.strip_prefix("music.track:").filter(|id| !id.is_empty()) {
        return ctx
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| catalog_track_resource(track, &ctx.playback));
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

impl ResourceProvider for crate::app::MusicApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.music_resource_ctx(), doc)
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        let ctx = self.music_resource_ctx_for_id(id)?;
        resource_by_id(&ctx, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::MusicApp;
    use qos_actions::{ActionInput, ArgValue, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
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
        app.seed_sample_library();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    /// Ctrl+K on Music yields the tree: the library with a "New playlist"
    /// action, a `music.playlist` child per playlist exporting rename + delete,
    /// and a `music.track` child per track in a playlist exporting
    /// remove + toggle-like. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_library_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
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
            qos_actions::RiskLevel::Destructive
        );

        let player = lib
            .children
            .iter()
            .find(|r| r.kind == "music.player")
            .expect("a stable player resource");
        assert!(player.actions.iter().any(|action| action.id == "queue-add"));
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
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"New playlist"), "{flat:?}");
        assert!(flat.contains(&"Delete playlist"), "{flat:?}");
        assert!(flat.contains(&"Remove from playlist"), "{flat:?}");

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New playlist")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Rename playlist")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (_app, seeded_doc) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;

        // A real playlist to delete (the sample library seeds several).
        let victim = crate::doc::load_library(&state.doc).playlists[0].id.clone();
        let before_heads = state.doc.get_heads();
        let before = crate::doc::load_library(&state.doc).playlists.len();

        let action = delete_playlist_action(&victim);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            crate::doc::load_library(&state.doc).playlists.len(),
            before - 1,
            "the action deleted a playlist"
        );
        assert!(
            crate::doc::load_library(&state.doc)
                .playlist(&victim)
                .is_none(),
            "the right playlist is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            crate::doc::load_library(&state.doc).playlists.len(),
            before,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            crate::doc::load_library(&state.doc)
                .playlist(&victim)
                .is_some(),
            "the playlist is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn add_track_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Add a real track to a real playlist.
        let lib = crate::doc::load_library(&a);
        let playlist = lib
            .playlists
            .iter()
            .find(|p| !p.track_ids.is_empty())
            .unwrap();
        let taken = playlist.track_ids.clone();
        let new_track = lib
            .tracks
            .keys()
            .find(|t| !taken.contains(*t))
            .cloned()
            .expect("a track not already in the playlist");
        let pid = playlist.id.clone();
        let action = add_track_action(&pid, &new_track);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            crate::doc::load_library(&b)
                .playlist(&pid)
                .unwrap()
                .track_ids
                .contains(&new_track),
            "the track landed on the peer too"
        );
    }

    #[test]
    fn portable_playlist_cover_replays_to_a_peer_without_a_local_path() {
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
        let envelope = qos_actions::apply_local(&mut a, &action, encode(&action)).unwrap();
        qos_actions::replay(&mut b, &envelope, &registry()).unwrap();

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

    #[test]
    fn playlist_cover_action_refuses_local_unsafe_and_malformed_values() {
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

    /// A deterministic toggle (like) also converges byte-identically — each
    /// replica reads the shared membership and flips it.
    #[test]
    fn toggle_liked_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let track = crate::doc::load_library(&a)
            .tracks
            .keys()
            .next()
            .unwrap()
            .clone();
        let before = crate::doc::load_library(&a).is_liked(&track);
        let action = toggle_liked_action(&track);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "toggle replayed identically");
        assert_eq!(crate::doc::load_library(&a).is_liked(&track), !before);
        assert_eq!(crate::doc::load_library(&b).is_liked(&track), !before);
    }

    /// Single source of truth: the palette's "Delete playlist" spec and the
    /// builder the trash button calls encode byte-identically; so does the
    /// prompted "Rename" spec against the rename builder for the same name.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let lib = &resources[0];
        let playlist = lib
            .children
            .iter()
            .find(|r| r.kind == "music.playlist")
            .unwrap();
        let id = playlist.id.as_str().trim_start_matches("music.playlist:");

        let delete_spec = playlist.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_playlist_action(id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        let rename_spec = playlist.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Road Trip");
        let from_builder = encode(&rename_playlist_action(id, "Road Trip"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

        // And a nested track's remove spec matches the remove builder.
        let with_tracks = lib
            .children
            .iter()
            .find(|r| !r.children.is_empty())
            .unwrap();
        let pid = with_tracks
            .id
            .as_str()
            .trim_start_matches("music.playlist:");
        let track = &with_tracks.children[0];
        let (_, tid) = parse_playlist_track_resource_id(track.id.as_str())
            .expect("nested playlist tracks use canonical membership IDs");
        let remove_spec = track.actions.iter().find(|a| a.id == "remove").unwrap();
        let from_spec = immediate_encode(remove_spec);
        let from_builder = encode(&remove_track_action(pid, &tid));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "remove is one source of truth"
        );
    }

    /// Dispatching the "New playlist" prompted action creates a playlist with the
    /// committed name (the id is minted at authoring time, so this asserts the
    /// effect, not byte-equality against a second freshly-minted id).
    #[test]
    fn new_playlist_action_creates_a_playlist() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-playlist")
            .unwrap();
        let before = crate::doc::load_library(&doc).playlists.len();

        let encoded = prompted_encode(spec, "Late Night");
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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

    #[test]
    fn playback_and_queue_are_local_effects_with_typed_arguments() {
        let mut ctx = MusicResourceCtx::default();
        ctx.playback.current = Some("track-1".into());
        ctx.playback.queue.push("track-1".into());
        let resources = provide_resources(&ctx, &AutoCommit::new());
        let root = resources[0]
            .children
            .iter()
            .find(|resource| resource.kind == "music.player")
            .unwrap();
        for id in [
            "play-pause",
            "seek",
            "set-volume",
            "queue-add",
            "queue-remove",
        ] {
            let action = root.actions.iter().find(|action| action.id == id).unwrap();
            assert_eq!(action.kind, qos_actions::OperationKind::Effect, "{id}");
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
            root.actions
                .iter()
                .find(|a| a.id == "seek")
                .unwrap()
                .params()[0]
                .ty,
            ParamType::Integer
        );
        let track = CatalogTrackResource {
            id: "track-2".into(),
            label: "Track 2".into(),
            album_id: String::new(),
            artist_id: String::new(),
            offline: false,
            liked: false,
        };
        let play = catalog_track_resource(&track, &ctx.playback)
            .actions
            .into_iter()
            .find(|action| action.id == "play")
            .unwrap();
        assert_eq!(
            play.contract.execution,
            ExecutionMode::Background { cancellable: false }
        );
        assert!(matches!(play.contract.result, ResultSpec::Json { .. }));
    }

    #[test]
    fn resource_arguments_refuse_wrong_kind_and_stale_tracks() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let library = &resources[0];
        let player = library
            .children
            .iter()
            .find(|resource| resource.kind == "music.player")
            .unwrap();
        let queue_add = player
            .actions
            .iter()
            .find(|action| action.id == "queue-add")
            .unwrap();

        for address in ["music.album:not-a-track", "music.track:removed"] {
            let encoded = queue_add
                .build_args([(String::from("track"), ArgValue::Resource(address.into()))])
                .expect("generic resource shape binds before Music validates its kind/state");
            assert_eq!(
                encoded.type_id,
                qos_actions::resource::INVALID_ARGUMENTS_TYPE,
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
            qos_actions::resource::INVALID_ARGUMENTS_TYPE
        );
    }

    #[test]
    fn stale_replayed_library_actions_are_refused() {
        let (_app, mut doc) = seeded();
        let stale = MusicAction::AddTrackToPlaylist {
            playlist: "gone".into(),
            track: "gone".into(),
        };
        assert!(matches!(
            stale.apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
    }

    #[test]
    fn contextual_resources_are_bounded_but_late_catalog_items_resolve_lazily() {
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
        let resources = provide_resources(&ctx, &AutoCommit::new());
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

    #[test]
    fn track_resource_exposes_exact_parent_navigation_targets() {
        let track = CatalogTrackResource {
            id: "track-1".into(),
            label: "Song — Artist".into(),
            album_id: "album-9".into(),
            artist_id: "artist-2".into(),
            offline: false,
            liked: false,
        };
        let resource = catalog_track_resource(&track, &crate::model::Playback::default());
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
            let encoded = immediate_encode(action);
            let operation = decode_operation(&encoded.type_id, &encoded.payload)
                .expect("operation decodes")
                .expect("music operation");
            match operation {
                MusicOperation::OpenAlbum { id } | MusicOperation::OpenArtist { id } => {
                    assert_eq!(id, wanted);
                }
                other => panic!("unexpected parent action: {other:?}"),
            }
        }
    }

    #[test]
    fn document_mutations_advertise_write_permission_and_views_advertise_read() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        for flat in qos_actions::flatten(&resources) {
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

    #[test]
    fn every_top_level_destination_is_a_safe_canonical_view_resource() {
        let ctx = MusicResourceCtx::default();
        let root = provide_resources(&ctx, &AutoCommit::new())
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
            let encoded = immediate_encode(open);
            assert_eq!(
                decode_operation(&encoded.type_id, &encoded.payload).expect("operation decodes"),
                Some(MusicOperation::OpenView { id: id.to_string() })
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

    #[test]
    fn published_resource_action_surface_validates_with_music_handlers() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let operation_handlers =
            std::collections::HashSet::from([MUSIC_OPERATION_TYPE.to_string()]);
        let violations =
            qos_actions::validate_surface(&resources, &registry(), &operation_handlers);
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn late_playlist_membership_resolves_lazily_with_canonical_actions() {
        let playlist = PlaylistResource {
            id: "playlist:日本語".into(),
            name: "Long playlist".into(),
            tracks: (0..(OPEN_PLAYLIST_TRACK_LIMIT + 8))
                .map(|number| TrackRef {
                    id: format!("track:{number}:🎧"),
                    label: format!("Track {number}"),
                    liked: false,
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
            }),
            ..Default::default()
        };
        let snapshot = provide_resources(&ctx, &AutoCommit::new());
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
            immediate_encode(remove),
            encode(&remove_track_action(&playlist.id, &target.id)),
            "the lazy resource uses the exact same canonical remove action"
        );
        let up = direct
            .actions
            .iter()
            .find(|action| action.id == "move-up")
            .unwrap();
        assert_eq!(
            immediate_encode(up),
            encode(&move_track_action(
                &playlist.id,
                &target.id,
                index,
                index - 1
            )),
            "the lazy resource preserves the original membership index"
        );
    }

    #[test]
    fn stale_membership_remove_and_reorder_are_refused() {
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
        let move_action = move_track_action(&playlist.id, &track, 0, 1);
        move_action.apply(&mut doc).expect("live membership moves");
        assert!(matches!(
            move_action.apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
    }
}
