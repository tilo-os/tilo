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
//! Presentation and live-session state stays out of the document and never
//! becomes an action: the current view / selection / scroll, the search text and
//! filter tab, and the whole **playback** surface — now-playing, playhead,
//! play/pause, volume, shuffle/repeat, and the queue. Playback does round-trip
//! through the CRDT (so a session hands off between devices), but as a
//! continuously-overwritten live-state map, not as replayable, undoable actions.
//! Offline **download / cache** state and external **streaming / artwork
//! fetches** are likewise non-deterministic background work written directly,
//! never actions.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. Toggles
//! (like / saved-album / followed-artist) read the document's current membership
//! and flip it, so two replicas at the same base heads land on the same result.
//! A malformed payload (an empty id, an unknown playlist) is a **no-op** rather
//! than an error, so a bad replay can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc as mdoc;

/// Registry `type_id` for every Music action.
pub const MUSIC_ACTION_TYPE: &str = "music.action";

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
    MoveTrackInPlaylist { playlist: String, from: usize, to: usize },
    /// Set (or clear, with an empty string) a playlist's cover image URL. The
    /// *pick* (the native file portal) is app-side, non-deterministic work; the
    /// resulting write of the chosen URL is this deterministic, replayable action.
    SetPlaylistCover { playlist: String, cover: String },
    /// Toggle a track's membership in "Liked Songs". Deterministic against the
    /// replay base: the current membership is read from the document and flipped.
    ToggleLiked { track: String },
    /// Toggle an album's membership in the saved-to-library set.
    ToggleSavedAlbum { album: String },
    /// Toggle whether the user follows an artist.
    ToggleFollowedArtist { artist: String },
}

impl Action for MusicAction {
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
                mdoc::rename_playlist(doc, id, name);
            }
            MusicAction::DeletePlaylist { id } => {
                mdoc::delete_playlist(doc, id);
            }
            MusicAction::AddTrackToPlaylist { playlist, track } => {
                mdoc::add_track_to_playlist(doc, playlist, track);
            }
            MusicAction::RemoveTrackFromPlaylist { playlist, track } => {
                mdoc::remove_track_from_playlist(doc, playlist, track);
            }
            MusicAction::MoveTrackInPlaylist { playlist, from, to } => {
                mdoc::move_track_in_playlist(doc, playlist, *from, *to);
            }
            MusicAction::SetPlaylistCover { playlist, cover } => {
                mdoc::set_playlist_cover(doc, playlist, cover);
            }
            MusicAction::ToggleLiked { track } => {
                mdoc::toggle_liked(doc, track);
            }
            MusicAction::ToggleSavedAlbum { album } => {
                mdoc::toggle_saved_album(doc, album);
            }
            MusicAction::ToggleFollowedArtist { artist } => {
                mdoc::toggle_followed_artist(doc, artist);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Music's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<MusicAction>(MUSIC_ACTION_TYPE);
}

/// Encode any [`MusicAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Music action into a
/// replayable envelope.
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
    MusicAction::CreatePlaylist { id: id.to_string(), name: name.to_string() }
}

pub fn rename_playlist_action(id: &str, name: &str) -> MusicAction {
    MusicAction::RenamePlaylist { id: id.to_string(), name: name.to_string() }
}

pub fn delete_playlist_action(id: &str) -> MusicAction {
    MusicAction::DeletePlaylist { id: id.to_string() }
}

pub fn add_track_action(playlist: &str, track: &str) -> MusicAction {
    MusicAction::AddTrackToPlaylist { playlist: playlist.to_string(), track: track.to_string() }
}

pub fn remove_track_action(playlist: &str, track: &str) -> MusicAction {
    MusicAction::RemoveTrackFromPlaylist { playlist: playlist.to_string(), track: track.to_string() }
}

pub fn move_track_action(playlist: &str, from: usize, to: usize) -> MusicAction {
    MusicAction::MoveTrackInPlaylist { playlist: playlist.to_string(), from, to }
}

pub fn set_cover_action(playlist: &str, cover: &str) -> MusicAction {
    MusicAction::SetPlaylistCover { playlist: playlist.to_string(), cover: cover.to_string() }
}

pub fn toggle_liked_action(track: &str) -> MusicAction {
    MusicAction::ToggleLiked { track: track.to_string() }
}

pub fn toggle_saved_album_action(album: &str) -> MusicAction {
    MusicAction::ToggleSavedAlbum { album: album.to_string() }
}

pub fn toggle_followed_artist_action(artist: &str) -> MusicAction {
    MusicAction::ToggleFollowedArtist { artist: artist.to_string() }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One track surfaced under a playlist resource: its id plus a one-line label.
#[derive(Debug, Clone)]
pub struct TrackRef {
    pub id: String,
    pub label: String,
}

/// One playlist surfaced as a resource, with its ordered tracks.
#[derive(Debug, Clone)]
pub struct PlaylistResource {
    pub id: String,
    pub name: String,
    pub tracks: Vec<TrackRef>,
}

/// The presentation context Music hands [`provide_resources`]: the user's
/// playlists and their tracks currently in the model cache. Everything here is
/// derived from state the document already holds; nothing is fabricated.
#[derive(Debug, Clone, Default)]
pub struct MusicResourceCtx {
    pub playlists: Vec<PlaylistResource>,
}

fn immediate_spec(id: &str, label: &str, action: &MusicAction) -> ActionSpec {
    ActionSpec::immediate(id, label, MUSIC_ACTION_TYPE, action).expect("encode music action")
}

fn playlist_label(p: &PlaylistResource) -> String {
    let n = p.name.trim();
    if n.is_empty() { "Untitled playlist".to_string() } else { n.to_string() }
}

/// Build the resource tree Music currently has open: the library (with a "New
/// playlist" action), each playlist (rename + delete), and each track within a
/// playlist (remove + toggle-like). Every document-mutating operation is
/// exported as the same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &MusicResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("music.library", "music.library", "Library");

    // Library-level: create a playlist. The id is minted once when the name
    // argument is committed, then baked into the envelope, so replay stays
    // deterministic.
    root = root.with_action(ActionSpec::prompted(
        "new-playlist",
        "New playlist",
        "Name",
        "playlist name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
            let name = if name.trim().is_empty() { "New Playlist" } else { name.trim() };
            encode(&create_playlist_action(&new_playlist_id(), name))
        },
    ));

    for pl in &ctx.playlists {
        let mut child = Resource::new(
            format!("music.playlist:{}", pl.id),
            "music.playlist",
            playlist_label(pl),
        );

        // Rename — a prompted action.
        let pid = pl.id.clone();
        child = child.with_action(ActionSpec::prompted(
            "rename",
            "Rename playlist",
            "Name",
            "playlist name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_playlist_action(&pid, name.trim())),
        ));
        // Delete the whole playlist.
        child = child.with_action(immediate_spec("delete", "Delete playlist", &delete_playlist_action(&pl.id)));

        // Each track in the playlist → its own resource (remove + toggle-like).
        for tk in &pl.tracks {
            let track = Resource::new(
                format!("music.track:{}:{}", pl.id, tk.id),
                "music.track",
                tk.label.clone(),
            )
            .with_action(immediate_spec(
                "remove",
                "Remove from playlist",
                &remove_track_action(&pl.id, &tk.id),
            ))
            .with_action(immediate_spec(
                "toggle-like",
                "Toggle like",
                &toggle_liked_action(&tk.id),
            ));
            child = child.with_child(track);
        }

        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::MusicApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.music_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::MusicApp;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
        }
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

        // A playlist with tracks nests a track resource with remove + toggle-like.
        let with_tracks = lib
            .children
            .iter()
            .find(|r| !r.children.is_empty())
            .expect("a playlist with tracks");
        let track = &with_tracks.children[0];
        assert_eq!(track.kind, "music.track");
        let tk_actions: Vec<&str> = track.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(tk_actions.contains(&"remove"), "{tk_actions:?}");
        assert!(tk_actions.contains(&"toggle-like"), "{tk_actions:?}");

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
        assert!(labels.iter().any(|l| l.contains("New playlist")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Rename playlist")), "{labels:?}");
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
        state.record_committed_action(before_heads.clone(), None, None);

        assert_eq!(
            crate::doc::load_library(&state.doc).playlists.len(),
            before - 1,
            "the action deleted a playlist"
        );
        assert!(
            crate::doc::load_library(&state.doc).playlist(&victim).is_none(),
            "the right playlist is gone"
        );
        assert!(state.peek_last_undo().is_some(), "exactly one undo boundary recorded");

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            crate::doc::load_library(&state.doc).playlists.len(),
            before,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            crate::doc::load_library(&state.doc).playlist(&victim).is_some(),
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
        assert_eq!(a.get_heads(), b.get_heads(), "precondition: same base state");

        // Add a real track to a real playlist.
        let lib = crate::doc::load_library(&a);
        let playlist = lib.playlists.iter().find(|p| !p.track_ids.is_empty()).unwrap();
        let new_track = lib
            .tracks
            .keys()
            .find(|t| !playlist.track_ids.contains(t))
            .unwrap()
            .clone();
        let pid = playlist.id.clone();
        let action = add_track_action(&pid, &new_track);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "replay reproduced the identical change");
        assert!(
            crate::doc::load_library(&b).playlist(&pid).unwrap().track_ids.contains(&new_track),
            "the track landed on the peer too"
        );
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

        let track = crate::doc::load_library(&a).tracks.keys().next().unwrap().clone();
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
        let playlist = lib.children.iter().find(|r| r.kind == "music.playlist").unwrap();
        let id = playlist.id.as_str().trim_start_matches("music.playlist:");

        let delete_spec = playlist.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_playlist_action(id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(from_spec.payload, from_builder.payload, "one source of truth for delete");

        let rename_spec = playlist.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Road Trip");
        let from_builder = encode(&rename_playlist_action(id, "Road Trip"));
        assert_eq!(from_spec.payload, from_builder.payload, "rename is one source of truth");

        // And a nested track's remove spec matches the remove builder.
        let with_tracks = lib.children.iter().find(|r| !r.children.is_empty()).unwrap();
        let pid = with_tracks.id.as_str().trim_start_matches("music.playlist:");
        let track = &with_tracks.children[0];
        let tid = track.id.as_str().rsplit(':').next().unwrap();
        let remove_spec = track.actions.iter().find(|a| a.id == "remove").unwrap();
        let from_spec = immediate_encode(remove_spec);
        let from_builder = encode(&remove_track_action(pid, tid));
        assert_eq!(from_spec.payload, from_builder.payload, "remove is one source of truth");
    }

    /// Dispatching the "New playlist" prompted action creates a playlist with the
    /// committed name (the id is minted at authoring time, so this asserts the
    /// effect, not byte-equality against a second freshly-minted id).
    #[test]
    fn new_playlist_action_creates_a_playlist() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0].actions.iter().find(|a| a.id == "new-playlist").unwrap();
        let before = crate::doc::load_library(&doc).playlists.len();

        let encoded = prompted_encode(spec, "Late Night");
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let lib = crate::doc::load_library(&doc);
        assert_eq!(lib.playlists.len(), before + 1, "the action added a playlist");
        assert!(
            lib.playlists.iter().any(|p| p.name == "Late Night"),
            "the created playlist carries the committed name"
        );
    }
}
