//! Automerge document bridge — the "our own library layer that syncs
//! between devices" part of the spec.
//!
//! The user-authored library (catalog, playlists, likes, saved albums and
//! followed artists) is mirrored into the shared `ui::State::doc`. Because
//! Automerge is a CRDT, two devices that start from the same base document
//! converge when they edit that library. Playback, downloaded bytes/cache
//! choices and credentials deliberately do **not** enter this document: they
//! are properties of the current device and its audio policy. The schema:
//!
//! ```text
//! root
//! ├── "library" (Map)
//! │   ├── "artists" (Map<id, {name, genre, monthly_listeners}>)
//! │   ├── "albums"  (Map<id, {title, artist_id, year}>)
//! │   └── "tracks"  (Map<id, {title, album_id, artist_id, track_no,
//! │                            duration_s, yt_id, plays}>)
//! ├── "playlists" (List<{id, name, description, tracks: List<str>}>)
//! ├── "liked"     (List<track_id>)
//! ├── "saved_albums" (List<album_id>)
//! └── "followed_artists" (List<artist_id>)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{Album, Artist, Library, Playback, Playlist, Track};

// ---- small scalar readers -------------------------------------------------

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
        _ => None,
    }
}

fn get_map(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

fn get_list(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}

fn library_entry_exists(doc: &AutoCommit, collection: &str, id: &str) -> bool {
    !id.is_empty()
        && get_map(doc, &ROOT, "library")
            .and_then(|library| get_map(doc, &library, collection))
            .is_some_and(|entries| doc.get(&entries, id).ok().flatten().is_some())
}

fn list_strings(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
    let n = doc.length(list);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if let Ok(Some((Value::Scalar(v), _))) = doc.get(list, i)
            && let ScalarValue::Str(s) = v.as_ref()
        {
            out.push(s.to_string());
        }
    }
    out
}

// ---- public API -----------------------------------------------------------

/// True once the document has been seeded with a library.
pub fn is_seeded(doc: &AutoCommit) -> bool {
    get_map(doc, &ROOT, "library").is_some()
}

/// Write the full library + playback into a fresh document. A failure to
/// create one of the top-level containers aborts the rest of that container's
/// seeding (an existing document from a corrupt or pathological state); a
/// failure on a single item within a container (an artist, an album) skips
/// just that item and continues with its siblings.
pub fn seed(doc: &mut AutoCommit, lib: &Library, _playback: &Playback) {
    let Ok(library) = doc.put_object(&ROOT, "library", ObjType::Map) else {
        return;
    };

    let Ok(artists) = doc.put_object(&library, "artists", ObjType::Map) else {
        return;
    };
    for a in lib.artists.values() {
        let Ok(m) = doc.put_object(&artists, a.id.as_str(), ObjType::Map) else {
            continue;
        };
        let _ = doc.put(&m, "name", a.name.as_str());
        let _ = doc.put(&m, "genre", a.genre.as_str());
        let _ = doc.put(&m, "monthly_listeners", a.monthly_listeners as i64);
    }

    let Ok(albums) = doc.put_object(&library, "albums", ObjType::Map) else {
        return;
    };
    for al in lib.albums.values() {
        let Ok(m) = doc.put_object(&albums, al.id.as_str(), ObjType::Map) else {
            continue;
        };
        let _ = doc.put(&m, "title", al.title.as_str());
        let _ = doc.put(&m, "artist_id", al.artist_id.as_str());
        let _ = doc.put(&m, "year", i64::from(al.year));
    }

    let Ok(tracks) = doc.put_object(&library, "tracks", ObjType::Map) else {
        return;
    };
    for t in lib.tracks.values() {
        write_track(doc, &tracks, t);
    }

    // Playlists.
    let Ok(playlists) = doc.put_object(&ROOT, "playlists", ObjType::List) else {
        return;
    };
    for (i, p) in lib.playlists.iter().enumerate() {
        write_playlist(doc, &playlists, i, p);
    }

    // Liked songs + saved albums.
    let Ok(liked) = doc.put_object(&ROOT, "liked", ObjType::List) else {
        return;
    };
    for (i, id) in lib.liked.iter().enumerate() {
        let _ = doc.insert(&liked, i, id.as_str());
    }
    let Ok(saved) = doc.put_object(&ROOT, "saved_albums", ObjType::List) else {
        return;
    };
    for (i, id) in lib.saved_albums.iter().enumerate() {
        let _ = doc.insert(&saved, i, id.as_str());
    }
    let Ok(followed) = doc.put_object(&ROOT, "followed_artists", ObjType::List) else {
        return;
    };
    for (i, id) in lib.followed_artists.iter().enumerate() {
        let _ = doc.insert(&followed, i, id.as_str());
    }
}

fn write_track(doc: &mut AutoCommit, tracks: &ObjId, t: &Track) {
    let Ok(m) = doc.put_object(tracks, t.id.as_str(), ObjType::Map) else {
        return;
    };
    let _ = doc.put(&m, "title", t.title.as_str());
    let _ = doc.put(&m, "album_id", t.album_id.as_str());
    let _ = doc.put(&m, "artist_id", t.artist_id.as_str());
    let _ = doc.put(&m, "track_no", i64::from(t.track_no));
    let _ = doc.put(&m, "duration_s", i64::from(t.duration_s));
    let _ = doc.put(&m, "yt_id", t.yt_id.as_str());
    let _ = doc.put(&m, "plays", t.plays as i64);
}

fn write_playlist(doc: &mut AutoCommit, playlists: &ObjId, index: usize, p: &Playlist) {
    let Ok(m) = doc.insert_object(playlists, index, ObjType::Map) else {
        return;
    };
    let _ = doc.put(&m, "id", p.id.as_str());
    let _ = doc.put(&m, "name", p.name.as_str());
    let _ = doc.put(&m, "description", p.description.as_str());
    let _ = doc.put(&m, "cover", p.cover.as_str());
    let Ok(list) = doc.put_object(&m, "tracks", ObjType::List) else {
        return;
    };
    for (i, tid) in p.track_ids.iter().enumerate() {
        let _ = doc.insert(&list, i, tid.as_str());
    }
}

/// A live (YouTube-Music) search result to merge into the library. Kept a plain
/// owned struct so the `crate::backend` / `crate::stream` types never leak into
/// the CRDT layer.
#[derive(Clone, Debug)]
pub struct LiveTrack {
    /// Library track id (stable, e.g. `yt:<videoId>`).
    pub track_id: String,
    pub title: String,
    pub artist_id: String,
    pub artist_name: String,
    pub album_id: String,
    pub album_title: String,
    pub duration_s: u32,
    /// The `YouTube` video id the stream is resolved from.
    pub yt_id: String,
}

fn get_or_create_map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    get_map(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Map).ok())
}

/// Merge a live search result — its artist, album and the track itself — into
/// the library so it shows up in the UI and becomes playable (its real
/// `yt_id` is what [`crate::backend`] resolves a stream from). Idempotent: an
/// existing artist / album / track with the same id is left as-is, so repeated
/// searches don't churn the document.
pub fn upsert_live_track(doc: &mut AutoCommit, t: &LiveTrack) {
    let Some(library) = get_or_create_map(doc, &ROOT, "library") else {
        return;
    };

    let Some(artists) = get_or_create_map(doc, &library, "artists") else {
        return;
    };
    if get_map(doc, &artists, &t.artist_id).is_none()
        && let Ok(m) = doc.put_object(&artists, t.artist_id.as_str(), ObjType::Map)
    {
        let _ = doc.put(&m, "name", t.artist_name.as_str());
        let _ = doc.put(&m, "genre", "");
        let _ = doc.put(&m, "monthly_listeners", 0i64);
    }

    let Some(albums) = get_or_create_map(doc, &library, "albums") else {
        return;
    };
    if get_map(doc, &albums, &t.album_id).is_none()
        && let Ok(m) = doc.put_object(&albums, t.album_id.as_str(), ObjType::Map)
    {
        let _ = doc.put(&m, "title", t.album_title.as_str());
        let _ = doc.put(&m, "artist_id", t.artist_id.as_str());
        let _ = doc.put(&m, "year", 0i64);
    }

    let Some(tracks) = get_or_create_map(doc, &library, "tracks") else {
        return;
    };
    if get_map(doc, &tracks, &t.track_id).is_none() {
        write_track(
            doc,
            &tracks,
            &Track {
                id: t.track_id.clone(),
                title: t.title.clone(),
                album_id: t.album_id.clone(),
                artist_id: t.artist_id.clone(),
                track_no: 0,
                duration_s: t.duration_s,
                yt_id: t.yt_id.clone(),
                plays: 0,
            },
        );
    }
}

/// Locate a playlist map object by its `id` field. Returns the playlist's
/// `ObjId` so callers can mutate it directly.
fn find_playlist_obj(doc: &AutoCommit, playlist_id: &str) -> Option<ObjId> {
    let playlists = get_list(doc, &ROOT, "playlists")?;
    let n = doc.length(&playlists);
    for i in 0..n {
        if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i)
            && get_str(doc, &pm, "id").as_deref() == Some(playlist_id)
        {
            return Some(pm);
        }
    }
    None
}

/// Whether a stable track id still names a library track in this document.
pub fn has_track(doc: &AutoCommit, id: &str) -> bool {
    library_entry_exists(doc, "tracks", id)
}

/// Whether a stable album id still names a library album in this document.
pub fn has_album(doc: &AutoCommit, id: &str) -> bool {
    library_entry_exists(doc, "albums", id)
}

/// Whether a stable artist id still names a library artist in this document.
pub fn has_artist(doc: &AutoCommit, id: &str) -> bool {
    library_entry_exists(doc, "artists", id)
}

/// Whether a stable playlist id still names a playlist in this document.
pub fn has_playlist(doc: &AutoCommit, id: &str) -> bool {
    find_playlist_obj(doc, id).is_some()
}

/// Append a track id to a playlist (by playlist id). No-op if the track is
/// already present or the playlist is unknown.
pub fn add_track_to_playlist(doc: &mut AutoCommit, playlist_id: &str, track_id: &str) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    if let Some(list) = get_list(doc, &pm, "tracks")
        && !list_strings(doc, &list).iter().any(|t| t == track_id)
    {
        let len = doc.length(&list);
        let _ = doc.insert(&list, len, track_id);
    }
}

/// Remove the first occurrence of a track id from a playlist.
pub fn remove_track_from_playlist(doc: &mut AutoCommit, playlist_id: &str, track_id: &str) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    if let Some(list) = get_list(doc, &pm, "tracks") {
        let ids = list_strings(doc, &list);
        if let Some(pos) = ids.iter().position(|t| t == track_id) {
            let _ = doc.delete(&list, pos);
        }
    }
}

/// Whether a playlist currently contains this track. Action replay checks this
/// before removing an addressable membership so a stale resource is refused
/// rather than recorded as a successful no-op.
pub fn playlist_contains_track(doc: &AutoCommit, playlist_id: &str, track_id: &str) -> bool {
    find_playlist_obj(doc, playlist_id)
        .and_then(|playlist| get_list(doc, &playlist, "tracks"))
        .is_some_and(|list| list_strings(doc, &list).iter().any(|id| id == track_id))
}

/// Read the exact membership identity at an ordered playlist position.
pub fn playlist_track_at(doc: &AutoCommit, playlist_id: &str, index: usize) -> Option<String> {
    let playlist = find_playlist_obj(doc, playlist_id)?;
    let list = get_list(doc, &playlist, "tracks")?;
    list_strings(doc, &list).get(index).cloned()
}

/// Move a track within a playlist from `from` to `to` (clamped). Implemented
/// as delete + insert so the ordered list stays compact.
pub fn move_track_in_playlist(doc: &mut AutoCommit, playlist_id: &str, from: usize, to: usize) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    let Some(list) = get_list(doc, &pm, "tracks") else {
        return;
    };
    let ids = list_strings(doc, &list);
    if from == to {
        return;
    }
    let Some(moved) = ids.get(from).cloned() else {
        return;
    };
    let to = to.min(ids.len().saturating_sub(1));
    let _ = doc.delete(&list, from);
    let _ = doc.insert(&list, to, moved.as_str());
}

/// Rename a playlist.
pub fn rename_playlist(doc: &mut AutoCommit, playlist_id: &str, name: &str) {
    if let Some(pm) = find_playlist_obj(doc, playlist_id) {
        let _ = doc.put(&pm, "name", name);
    }
}

/// Delete a playlist by id.
pub fn delete_playlist(doc: &mut AutoCommit, playlist_id: &str) {
    let Some(playlists) = get_list(doc, &ROOT, "playlists") else {
        return;
    };
    let n = doc.length(&playlists);
    for i in 0..n {
        if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i)
            && get_str(doc, &pm, "id").as_deref() == Some(playlist_id)
        {
            let _ = doc.delete(&playlists, i);
            return;
        }
    }
}

// ---- liked songs + saved albums ------------------------------------------

fn ensure_str_list(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
    get_list(doc, &ROOT, key).or_else(|| doc.put_object(&ROOT, key, ObjType::List).ok())
}

/// Toggle a track's membership in "Liked Songs". Returns the new liked state.
pub fn toggle_liked(doc: &mut AutoCommit, track_id: &str) -> bool {
    let Some(list) = ensure_str_list(doc, "liked") else {
        return false;
    };
    let ids = list_strings(doc, &list);
    if let Some(pos) = ids.iter().position(|t| t == track_id) {
        let _ = doc.delete(&list, pos);
        false
    } else {
        let len = doc.length(&list);
        let _ = doc.insert(&list, len, track_id);
        true
    }
}

/// Force a track liked (idempotent). Used by the context-menu "Like" /
/// "Add to library" entries which are one-way affordances.
pub fn set_liked(doc: &mut AutoCommit, track_id: &str, liked: bool) {
    let Some(list) = ensure_str_list(doc, "liked") else {
        return;
    };
    let ids = list_strings(doc, &list);
    let present = ids.iter().any(|t| t == track_id);
    if liked && !present {
        let len = doc.length(&list);
        let _ = doc.insert(&list, len, track_id);
    } else if !liked
        && present
        && let Some(pos) = ids.iter().position(|t| t == track_id)
    {
        let _ = doc.delete(&list, pos);
    }
}

/// Toggle an album's membership in the saved-albums set. Returns the new
/// saved state.
pub fn toggle_saved_album(doc: &mut AutoCommit, album_id: &str) -> bool {
    let Some(list) = ensure_str_list(doc, "saved_albums") else {
        return false;
    };
    let ids = list_strings(doc, &list);
    if let Some(pos) = ids.iter().position(|t| t == album_id) {
        let _ = doc.delete(&list, pos);
        false
    } else {
        let len = doc.length(&list);
        let _ = doc.insert(&list, len, album_id);
        true
    }
}

/// Toggle whether the user follows an artist. Returns the new followed state.
pub fn toggle_followed_artist(doc: &mut AutoCommit, artist_id: &str) -> bool {
    let Some(list) = ensure_str_list(doc, "followed_artists") else {
        return false;
    };
    let ids = list_strings(doc, &list);
    if let Some(pos) = ids.iter().position(|t| t == artist_id) {
        let _ = doc.delete(&list, pos);
        false
    } else {
        let len = doc.length(&list);
        let _ = doc.insert(&list, len, artist_id);
        true
    }
}

/// Create a new empty playlist, returning its generated id.
pub fn create_playlist(doc: &mut AutoCommit, name: &str) -> String {
    let id = format!("pl_{}", uuid::Uuid::new_v4().simple());
    create_playlist_with_id(doc, &id, name);
    id
}

/// Create a new empty playlist with a caller-supplied id (minted at authoring
/// time). Appending at the current list length is deterministic against a
/// shared base state, so replaying this on a peer reproduces the identical
/// change — the property the action model relies on. No-op if a playlist with
/// this id already exists (so a duplicate replay can't double-insert).
pub fn create_playlist_with_id(doc: &mut AutoCommit, id: &str, name: &str) {
    if find_playlist_obj(doc, id).is_some() {
        return;
    }
    let Some(playlists) = get_list(doc, &ROOT, "playlists")
        .or_else(|| doc.put_object(&ROOT, "playlists", ObjType::List).ok())
    else {
        return;
    };
    let idx = doc.length(&playlists);
    write_playlist(
        doc,
        &playlists,
        idx,
        &Playlist {
            id: id.to_string(),
            name: name.into(),
            description: String::new(),
            track_ids: vec![],
            cover: String::new(),
        },
    );
}

/// Set (or clear) a playlist's portable cover value (an HTTPS URL or bounded
/// embedded PNG data URL). No-op if the playlist is unknown.
pub fn set_playlist_cover(doc: &mut AutoCommit, playlist_id: &str, cover: &str) {
    if let Some(pm) = find_playlist_obj(doc, playlist_id) {
        let _ = doc.put(&pm, "cover", cover);
    }
}

/// Read the entire library out of the document.
pub fn load_library(doc: &AutoCommit) -> Library {
    let mut lib = Library::default();
    let Some(library) = get_map(doc, &ROOT, "library") else {
        return lib;
    };

    if let Some(artists) = get_map(doc, &library, "artists") {
        for key in doc.keys(&artists) {
            if let Some((Value::Object(ObjType::Map), m)) = doc.get(&artists, &key).ok().flatten() {
                lib.artists.insert(
                    key.clone(),
                    Artist {
                        id: key,
                        name: get_str(doc, &m, "name").unwrap_or_default(),
                        genre: get_str(doc, &m, "genre").unwrap_or_default(),
                        monthly_listeners: get_i64(doc, &m, "monthly_listeners").unwrap_or(0)
                            as u64,
                    },
                );
            }
        }
    }

    if let Some(albums) = get_map(doc, &library, "albums") {
        for key in doc.keys(&albums) {
            if let Some((Value::Object(ObjType::Map), m)) = doc.get(&albums, &key).ok().flatten() {
                lib.albums.insert(
                    key.clone(),
                    Album {
                        id: key,
                        title: get_str(doc, &m, "title").unwrap_or_default(),
                        artist_id: get_str(doc, &m, "artist_id").unwrap_or_default(),
                        year: get_i64(doc, &m, "year").unwrap_or(0) as u32,
                    },
                );
            }
        }
    }

    if let Some(tracks) = get_map(doc, &library, "tracks") {
        for key in doc.keys(&tracks) {
            if let Some((Value::Object(ObjType::Map), m)) = doc.get(&tracks, &key).ok().flatten() {
                lib.tracks.insert(
                    key.clone(),
                    Track {
                        id: key,
                        title: get_str(doc, &m, "title").unwrap_or_default(),
                        album_id: get_str(doc, &m, "album_id").unwrap_or_default(),
                        artist_id: get_str(doc, &m, "artist_id").unwrap_or_default(),
                        track_no: get_i64(doc, &m, "track_no").unwrap_or(0) as u32,
                        duration_s: get_i64(doc, &m, "duration_s").unwrap_or(0) as u32,
                        yt_id: get_str(doc, &m, "yt_id").unwrap_or_default(),
                        plays: get_i64(doc, &m, "plays").unwrap_or(0) as u64,
                    },
                );
            }
        }
    }

    if let Some(playlists) = get_list(doc, &ROOT, "playlists") {
        let n = doc.length(&playlists);
        for i in 0..n {
            if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i) {
                let tracks = get_list(doc, &pm, "tracks")
                    .map(|l| list_strings(doc, &l))
                    .unwrap_or_default();
                lib.playlists.push(Playlist {
                    id: get_str(doc, &pm, "id").unwrap_or_default(),
                    name: get_str(doc, &pm, "name").unwrap_or_default(),
                    description: get_str(doc, &pm, "description").unwrap_or_default(),
                    track_ids: tracks,
                    cover: get_str(doc, &pm, "cover").unwrap_or_default(),
                });
            }
        }
    }

    if let Some(liked) = get_list(doc, &ROOT, "liked") {
        for id in list_strings(doc, &liked) {
            lib.liked.insert(id);
        }
    }
    if let Some(saved) = get_list(doc, &ROOT, "saved_albums") {
        for id in list_strings(doc, &saved) {
            lib.saved_albums.insert(id);
        }
    }
    if let Some(followed) = get_list(doc, &ROOT, "followed_artists") {
        for id in list_strings(doc, &followed) {
            lib.followed_artists.insert(id);
        }
    }

    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed;

    #[test]
    fn seed_then_load_round_trips_library() {
        let mut doc = AutoCommit::new();
        let lib = seed::sample_library();
        let pb = seed::sample_playback(&lib);
        assert!(!is_seeded(&doc));
        seed(&mut doc, &lib, &pb);
        assert!(is_seeded(&doc));

        let loaded = load_library(&doc);
        assert_eq!(loaded.artists.len(), lib.artists.len());
        assert_eq!(loaded.albums.len(), lib.albums.len());
        assert_eq!(loaded.tracks.len(), lib.tracks.len());
        assert_eq!(loaded.playlists.len(), lib.playlists.len());
        // A specific track round-trips intact.
        let g1 = loaded.track("tk_g1").unwrap();
        assert_eq!(g1.title, "Glasslight");
        assert_eq!(g1.duration_s, 215);
    }

    #[test]
    fn seeding_contains_no_device_effect_maps() {
        let mut doc = AutoCommit::new();
        let lib = seed::sample_library();
        let pb = seed::sample_playback(&lib);
        seed(&mut doc, &lib, &pb);
        assert!(get_map(&doc, &ROOT, "playback").is_none());
        assert!(get_map(&doc, &ROOT, "cache").is_none());
    }

    #[test]
    fn add_track_to_playlist_appends_once() {
        let mut doc = AutoCommit::new();
        let lib = seed::sample_library();
        seed(&mut doc, &lib, &seed::sample_playback(&lib));
        add_track_to_playlist(&mut doc, "pl_morning", "tk_h1");
        add_track_to_playlist(&mut doc, "pl_morning", "tk_h1"); // dedup
        let loaded = load_library(&doc);
        let pl = loaded.playlist("pl_morning").unwrap();
        assert_eq!(pl.track_ids.iter().filter(|t| *t == "tk_h1").count(), 1);
    }

    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let lib = seed::sample_library();
        seed(&mut doc, &lib, &seed::sample_playback(&lib));
        doc
    }

    #[test]
    fn remove_and_reorder_playlist_tracks() {
        let mut doc = seeded_doc();
        // pl_morning seeds [tk_p1, tk_r1, tk_g2, tk_r3].
        remove_track_from_playlist(&mut doc, "pl_morning", "tk_g2");
        let pl = load_library(&doc).playlist("pl_morning").unwrap().clone();
        assert_eq!(pl.track_ids, vec!["tk_p1", "tk_r1", "tk_r3"]);
        // Move the first track to the end.
        move_track_in_playlist(&mut doc, "pl_morning", 0, 2);
        let pl = load_library(&doc).playlist("pl_morning").unwrap().clone();
        assert_eq!(pl.track_ids, vec!["tk_r1", "tk_r3", "tk_p1"]);
    }

    #[test]
    fn rename_and_delete_playlist() {
        let mut doc = seeded_doc();
        rename_playlist(&mut doc, "pl_focus", "Concentration");
        assert_eq!(
            load_library(&doc).playlist("pl_focus").unwrap().name,
            "Concentration"
        );
        let before = load_library(&doc).playlists.len();
        delete_playlist(&mut doc, "pl_focus");
        let after = load_library(&doc);
        assert_eq!(after.playlists.len(), before - 1);
        assert!(after.playlist("pl_focus").is_none());
    }

    #[test]
    fn upsert_live_track_merges_and_is_idempotent() {
        let mut doc = seeded_doc();
        let before = load_library(&doc).total_tracks();
        let lt = LiveTrack {
            track_id: "yt:abc123".into(),
            title: "Runaway".into(),
            artist_id: "ytc_aurora".into(),
            artist_name: "AURORA".into(),
            album_id: "ytal_allmy".into(),
            album_title: "All My Demons".into(),
            duration_s: 245,
            yt_id: "abc123".into(),
        };
        upsert_live_track(&mut doc, &lt);
        upsert_live_track(&mut doc, &lt); // idempotent
        let lib = load_library(&doc);
        assert_eq!(
            lib.total_tracks(),
            before + 1,
            "one live track added, no dupes"
        );
        let t = lib.track("yt:abc123").expect("live track present");
        assert_eq!(t.title, "Runaway");
        assert_eq!(t.yt_id, "abc123", "real video id preserved for streaming");
        assert_eq!(lib.artist_name("ytc_aurora"), "AURORA");
        assert_eq!(lib.album_title("ytal_allmy"), "All My Demons");
    }

    #[test]
    fn toggle_liked_round_trips_through_doc() {
        let mut doc = seeded_doc();
        assert!(!load_library(&doc).is_liked("tk_g2"));
        assert!(toggle_liked(&mut doc, "tk_g2"));
        assert!(load_library(&doc).is_liked("tk_g2"));
        assert!(!toggle_liked(&mut doc, "tk_g2"));
        assert!(!load_library(&doc).is_liked("tk_g2"));
    }

    #[test]
    fn toggle_saved_album_round_trips() {
        let mut doc = seeded_doc();
        // al_glasslight is seeded saved; al_strata is not.
        assert!(load_library(&doc).is_album_saved("al_glasslight"));
        assert!(toggle_saved_album(&mut doc, "al_strata"));
        assert!(load_library(&doc).is_album_saved("al_strata"));
        assert!(!toggle_saved_album(&mut doc, "al_strata"));
        assert!(!load_library(&doc).is_album_saved("al_strata"));
    }

    #[test]
    fn liked_set_replicates_between_devices() {
        // Device A seeds, B loads, B likes a track, A merges and converges.
        let mut a = seeded_doc();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        set_liked(&mut b, "tk_v2", true);
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        assert!(load_library(&a).is_liked("tk_v2"));
    }
}
