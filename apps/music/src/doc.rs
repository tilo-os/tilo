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
//! ├── "playlists" (List<{id, name, description,
//! │                       track_membership: Map<track_id, bool>,
//! │                       track_order: Map<track_id, i64>}>)
//! ├── "playlist_tombstones" (Map<playlist_id, true>)
//! ├── "liked"     (Map<track_id, bool>)
//! ├── "saved_albums" (Map<album_id, bool>)
//! ├── "followed_artists" (Map<artist_id, bool>)
//! └── "recommendations/{input,preferred-artist,dismissed}/{id}" (bool)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{Album, Artist, Library, Playlist, Track};

const ORDER_STEP: i64 = 1_048_576;

// ---- small scalar readers -------------------------------------------------

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

/// Playlist deletion is delete-wins.  Keeping a compact keyed tombstone means
/// an offline device cannot revive a deleted playlist by replaying an old
/// rename, cover or membership action after it reconnects.
pub fn playlist_is_tombstoned(doc: &AutoCommit, id: &str) -> bool {
    !id.is_empty()
        && get_map(doc, &ROOT, "playlist_tombstones")
            .and_then(|tombstones| doc.get(&tombstones, id).ok().flatten())
            .is_some()
}

fn library_entry_exists(doc: &AutoCommit, collection: &str, id: &str) -> bool {
    !id.is_empty()
        && get_map(doc, &ROOT, "library")
            .and_then(|library| get_map(doc, &library, collection))
            .is_some_and(|entries| doc.get(&entries, id).ok().flatten().is_some())
}

fn membership_entries(doc: &AutoCommit, playlist: &ObjId) -> Vec<(String, i64)> {
    let Some(membership) = get_map(doc, playlist, "track_membership") else {
        return Vec::new();
    };
    let order = get_map(doc, playlist, "track_order");
    let mut entries = doc
        .keys(&membership)
        .filter(|id| read_bool(doc, &membership, id).unwrap_or(false))
        .map(|id| {
            let rank = order
                .as_ref()
                .and_then(|order| read_i64_coerced(doc, order, &id))
                .unwrap_or(0);
            (id, rank)
        })
        .collect::<Vec<_>>();
    entries
        .sort_by(|(a_id, a_rank), (b_id, b_rank)| a_rank.cmp(b_rank).then_with(|| a_id.cmp(b_id)));
    entries
}

fn ensure_membership_maps(doc: &mut AutoCommit, playlist: &ObjId) -> Option<(ObjId, ObjId)> {
    if get_map(doc, playlist, "track_membership").is_none() {
        let membership = doc
            .put_object(playlist, "track_membership", ObjType::Map)
            .ok()?;
        let order = doc.put_object(playlist, "track_order", ObjType::Map).ok()?;
        return Some((membership, order));
    }
    let membership = get_map(doc, playlist, "track_membership")?;
    let order = get_map(doc, playlist, "track_order")
        .or_else(|| doc.put_object(playlist, "track_order", ObjType::Map).ok())?;
    Some((membership, order))
}

// ---- public API -----------------------------------------------------------

/// True once the document has been seeded with a library.
pub fn is_seeded(doc: &AutoCommit) -> bool {
    get_map(doc, &ROOT, "library").is_some()
}

/// Write a whole library into a fresh document: empty for a new record's
/// genesis, the sample catalogue in tests. A failure to create one of the
/// top-level containers aborts the rest of that container's seeding (an
/// existing document from a corrupt or pathological state); a failure on a
/// single item within a container (an artist, an album) skips just that item
/// and continues with its siblings.
pub fn seed(doc: &mut AutoCommit, lib: &Library) {
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
    let Ok(liked) = doc.put_object(&ROOT, "liked", ObjType::Map) else {
        return;
    };
    for id in &lib.liked {
        let _ = doc.put(&liked, id.as_str(), true);
    }
    let Ok(saved) = doc.put_object(&ROOT, "saved_albums", ObjType::Map) else {
        return;
    };
    for id in &lib.saved_albums {
        let _ = doc.put(&saved, id.as_str(), true);
    }
    // A keyed delete-wins set; empty on a freshly seeded library.
    let _ = doc.put_object(&ROOT, "playlist_tombstones", ObjType::Map);
    let Ok(followed) = doc.put_object(&ROOT, "followed_artists", ObjType::Map) else {
        return;
    };
    for id in &lib.followed_artists {
        let _ = doc.put(&followed, id.as_str(), true);
    }
    let _ = crate::recommendations::seed(doc, &lib.recommendations);
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
    let Ok(membership) = doc.put_object(&m, "track_membership", ObjType::Map) else {
        return;
    };
    let Ok(order) = doc.put_object(&m, "track_order", ObjType::Map) else {
        return;
    };
    for (i, tid) in p.track_ids.iter().enumerate() {
        let rank = p
            .track_ranks
            .get(tid)
            .copied()
            .unwrap_or_else(|| i64::try_from(i).unwrap_or(i64::MAX / ORDER_STEP) * ORDER_STEP);
        let _ = doc.put(&membership, tid.as_str(), true);
        let _ = doc.put(&order, tid.as_str(), rank);
    }
}

fn get_or_create_map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    get_map(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Map).ok())
}

/// Add a catalogue track, its album and its artist to the library. Idempotent:
/// an existing artist / album / track with the same id is left as it is, so
/// importing a track again does not churn the document.
pub fn import_track(doc: &mut AutoCommit, track: &Track, album: &Album, artist: &Artist) {
    let Some(library) = get_or_create_map(doc, &ROOT, "library") else {
        return;
    };

    let Some(artists) = get_or_create_map(doc, &library, "artists") else {
        return;
    };
    if get_map(doc, &artists, &artist.id).is_none()
        && let Ok(m) = doc.put_object(&artists, artist.id.as_str(), ObjType::Map)
    {
        let _ = doc.put(&m, "name", artist.name.as_str());
        let _ = doc.put(&m, "genre", artist.genre.as_str());
        let _ = doc.put(&m, "monthly_listeners", artist.monthly_listeners as i64);
    }

    let Some(albums) = get_or_create_map(doc, &library, "albums") else {
        return;
    };
    if get_map(doc, &albums, &album.id).is_none()
        && let Ok(m) = doc.put_object(&albums, album.id.as_str(), ObjType::Map)
    {
        let _ = doc.put(&m, "title", album.title.as_str());
        let _ = doc.put(&m, "artist_id", album.artist_id.as_str());
        let _ = doc.put(&m, "year", i64::from(album.year));
    }

    let Some(tracks) = get_or_create_map(doc, &library, "tracks") else {
        return;
    };
    if get_map(doc, &tracks, &track.id).is_none() {
        write_track(doc, &tracks, track);
    }
}

/// Locate a playlist map object by its `id` field. Returns the playlist's
/// `ObjId` so callers can mutate it directly.
fn find_playlist_obj(doc: &AutoCommit, playlist_id: &str) -> Option<ObjId> {
    if playlist_is_tombstoned(doc, playlist_id) {
        return None;
    }
    let playlists = get_list(doc, &ROOT, "playlists")?;
    let n = doc.length(&playlists);
    for i in 0..n {
        if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i)
            && read_str(doc, &pm, "id").as_deref() == Some(playlist_id)
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
pub fn add_track_to_playlist(doc: &mut AutoCommit, playlist_id: &str, track_id: &str, rank: i64) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    let Some((membership, order)) = ensure_membership_maps(doc, &pm) else {
        return;
    };
    let _ = doc.put(&membership, track_id, true);
    let _ = doc.put(&order, track_id, rank);
}

/// Remove the first occurrence of a track id from a playlist.
pub fn remove_track_from_playlist(doc: &mut AutoCommit, playlist_id: &str, track_id: &str) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    if let Some((membership, _)) = ensure_membership_maps(doc, &pm) {
        let _ = doc.put(&membership, track_id, false);
    }
}

/// Whether a playlist currently contains this track. Action replay checks this
/// before removing an addressable membership so a stale resource is refused
/// rather than recorded as a successful no-op.
pub fn playlist_contains_track(doc: &AutoCommit, playlist_id: &str, track_id: &str) -> bool {
    find_playlist_obj(doc, playlist_id).is_some_and(|playlist| {
        membership_entries(doc, &playlist)
            .iter()
            .any(|(id, _)| id == track_id)
    })
}

/// Assign an authored absolute sparse order key to a stable membership.
pub fn set_track_rank(doc: &mut AutoCommit, playlist_id: &str, track_id: &str, rank: i64) {
    let Some(pm) = find_playlist_obj(doc, playlist_id) else {
        return;
    };
    let Some((membership, order)) = ensure_membership_maps(doc, &pm) else {
        return;
    };
    if !read_bool(doc, &membership, track_id).unwrap_or(false) {
        return;
    }
    let _ = doc.put(&order, track_id, rank);
}

/// Rename a playlist.
pub fn rename_playlist(doc: &mut AutoCommit, playlist_id: &str, name: &str) {
    if let Some(pm) = find_playlist_obj(doc, playlist_id) {
        let _ = doc.put(&pm, "name", name);
    }
}

/// Delete a playlist by id.
pub fn delete_playlist(doc: &mut AutoCommit, playlist_id: &str) {
    let Some(tombstones) = get_map(doc, &ROOT, "playlist_tombstones").or_else(|| {
        doc.put_object(&ROOT, "playlist_tombstones", ObjType::Map)
            .ok()
    }) else {
        return;
    };
    let _ = doc.put(&tombstones, playlist_id, true);
    let Some(playlists) = get_list(doc, &ROOT, "playlists") else {
        return;
    };
    let n = doc.length(&playlists);
    for i in 0..n {
        if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i)
            && read_str(doc, &pm, "id").as_deref() == Some(playlist_id)
        {
            let _ = doc.delete(&playlists, i);
            return;
        }
    }
}

// ---- liked songs + saved albums ------------------------------------------

fn ensure_bool_map(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
    if let Some(map) = get_map(doc, &ROOT, key) {
        return Some(map);
    }
    doc.put_object(&ROOT, key, ObjType::Map).ok()
}

fn load_bool_set(doc: &AutoCommit, key: &str) -> std::collections::BTreeSet<String> {
    get_map(doc, &ROOT, key)
        .map(|map| {
            doc.keys(&map)
                .filter(|id| read_bool(doc, &map, id).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default()
}

/// Toggle a track's membership in "Liked Songs". Returns the new liked state.
pub fn toggle_liked(doc: &mut AutoCommit, track_id: &str) -> bool {
    let Some(map) = ensure_bool_map(doc, "liked") else {
        return false;
    };
    let liked = !read_bool(doc, &map, track_id).unwrap_or(false);
    let _ = doc.put(&map, track_id, liked);
    liked
}

/// Force a track liked (idempotent). Used by the context-menu "Like" /
/// "Add to library" entries which are one-way affordances.
pub fn set_liked(doc: &mut AutoCommit, track_id: &str, liked: bool) {
    let Some(map) = ensure_bool_map(doc, "liked") else {
        return;
    };
    let _ = doc.put(&map, track_id, liked);
}

/// Toggle an album's membership in the saved-albums set. Returns the new
/// saved state.
pub fn toggle_saved_album(doc: &mut AutoCommit, album_id: &str) -> bool {
    let Some(map) = ensure_bool_map(doc, "saved_albums") else {
        return false;
    };
    let saved = !read_bool(doc, &map, album_id).unwrap_or(false);
    let _ = doc.put(&map, album_id, saved);
    saved
}

/// Set an album's saved membership to an explicit, replay-safe value.
pub fn set_saved_album(doc: &mut AutoCommit, album_id: &str, saved: bool) {
    let Some(map) = ensure_bool_map(doc, "saved_albums") else {
        return;
    };
    let _ = doc.put(&map, album_id, saved);
}

/// Set an artist's followed membership to an explicit, replay-safe value.
pub fn set_followed_artist(doc: &mut AutoCommit, artist_id: &str, followed: bool) {
    let Some(map) = ensure_bool_map(doc, "followed_artists") else {
        return;
    };
    let _ = doc.put(&map, artist_id, followed);
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
    if id.is_empty() || playlist_is_tombstoned(doc, id) || find_playlist_obj(doc, id).is_some() {
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
            track_ranks: std::collections::BTreeMap::new(),
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
                        name: read_str(doc, &m, "name").unwrap_or_default(),
                        genre: read_str(doc, &m, "genre").unwrap_or_default(),
                        monthly_listeners: read_i64_coerced(doc, &m, "monthly_listeners")
                            .unwrap_or(0) as u64,
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
                        title: read_str(doc, &m, "title").unwrap_or_default(),
                        artist_id: read_str(doc, &m, "artist_id").unwrap_or_default(),
                        year: read_i64_coerced(doc, &m, "year").unwrap_or(0) as u32,
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
                        title: read_str(doc, &m, "title").unwrap_or_default(),
                        album_id: read_str(doc, &m, "album_id").unwrap_or_default(),
                        artist_id: read_str(doc, &m, "artist_id").unwrap_or_default(),
                        track_no: read_i64_coerced(doc, &m, "track_no").unwrap_or(0) as u32,
                        duration_s: read_i64_coerced(doc, &m, "duration_s").unwrap_or(0) as u32,
                        yt_id: read_str(doc, &m, "yt_id").unwrap_or_default(),
                        plays: read_i64_coerced(doc, &m, "plays").unwrap_or(0) as u64,
                    },
                );
            }
        }
    }

    if let Some(playlists) = get_list(doc, &ROOT, "playlists") {
        let n = doc.length(&playlists);
        for i in 0..n {
            if let Ok(Some((Value::Object(ObjType::Map), pm))) = doc.get(&playlists, i) {
                let id = read_str(doc, &pm, "id").unwrap_or_default();
                if id.is_empty() || playlist_is_tombstoned(doc, &id) {
                    continue;
                }
                let entries = membership_entries(doc, &pm);
                let tracks = entries.iter().map(|(id, _)| id.clone()).collect();
                let track_ranks = entries.into_iter().collect();
                lib.playlists.push(Playlist {
                    id,
                    name: read_str(doc, &pm, "name").unwrap_or_default(),
                    description: read_str(doc, &pm, "description").unwrap_or_default(),
                    track_ids: tracks,
                    track_ranks,
                    cover: read_str(doc, &pm, "cover").unwrap_or_default(),
                });
            }
        }
    }

    lib.liked = load_bool_set(doc, "liked");
    lib.saved_albums = load_bool_set(doc, "saved_albums");
    lib.followed_artists = load_bool_set(doc, "followed_artists");
    lib.recommendations = crate::recommendations::read(doc);

    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed;
    use tilo_automerge::ApplyIncoming;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn library_seed_and_live_upserts_round_trip_without_device_state() {
        // Phase `seed_then_load_round_trips_library`.
        {
            let mut doc = AutoCommit::new();
            let lib = seed::sample_library();
            assert!(!is_seeded(&doc));
            seed(&mut doc, &lib);
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

        // Phase `seeding_contains_no_device_effect_maps`.
        {
            let mut doc = AutoCommit::new();
            seed(&mut doc, &seed::sample_library());
            assert!(get_map(&doc, &ROOT, "playback").is_none());
            assert!(get_map(&doc, &ROOT, "cache").is_none());
        }

        // Phase `import_track_merges_and_is_idempotent`.
        {
            let mut doc = seeded_doc();
            let before = load_library(&doc).total_tracks();
            let artist = Artist {
                id: "ytc_aurora".into(),
                name: "AURORA".into(),
                genre: String::new(),
                monthly_listeners: 0,
            };
            let album = Album {
                id: "ytal_allmy".into(),
                title: "All My Demons".into(),
                artist_id: artist.id.clone(),
                year: 0,
            };
            let track = Track {
                id: "yt:abc123".into(),
                title: "Runaway".into(),
                album_id: album.id.clone(),
                artist_id: artist.id.clone(),
                track_no: 0,
                duration_s: 245,
                yt_id: "abc123".into(),
                plays: 0,
            };
            import_track(&mut doc, &track, &album, &artist);
            import_track(&mut doc, &track, &album, &artist); // idempotent
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn playlist_edits_round_trip_and_merge() {
        // Phase `add_track_to_playlist_appends_once`.
        {
            let mut doc = AutoCommit::new();
            let lib = seed::sample_library();
            seed(&mut doc, &lib);
            add_track_to_playlist(&mut doc, "pl_morning", "tk_h1", 9_000_000);
            add_track_to_playlist(&mut doc, "pl_morning", "tk_h1", 9_000_000); // dedup
            let loaded = load_library(&doc);
            let pl = loaded.playlist("pl_morning").unwrap();
            assert_eq!(pl.track_ids.iter().filter(|t| *t == "tk_h1").count(), 1);
        }

        // Phase `remove_and_reorder_playlist_tracks`.
        {
            let mut doc = seeded_doc();
            // pl_morning seeds [tk_p1, tk_r1, tk_g2, tk_r3].
            remove_track_from_playlist(&mut doc, "pl_morning", "tk_g2");
            let pl = load_library(&doc).playlist("pl_morning").unwrap().clone();
            assert_eq!(pl.track_ids, vec!["tk_p1", "tk_r1", "tk_r3"]);
            // Move the first track to the end.
            let playlist = load_library(&doc).playlist("pl_morning").unwrap().clone();
            let rank = playlist.rank_for_move("tk_p1", 2).unwrap();
            set_track_rank(&mut doc, "pl_morning", "tk_p1", rank);
            let pl = load_library(&doc).playlist("pl_morning").unwrap().clone();
            assert_eq!(pl.track_ids, vec!["tk_r1", "tk_r3", "tk_p1"]);
        }

        // Phase `rename_and_delete_playlist`.
        {
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

        // Phase `deleted_playlist_stays_hidden_after_an_old_replica_merges`.
        {
            let mut deleted = seeded_doc();
            deleted.commit();
            let mut stale = AutoCommit::load(&deleted.save()).unwrap();
            delete_playlist(&mut deleted, "pl_focus");
            // The stale device makes a valid edit to the pre-delete object.
            rename_playlist(&mut stale, "pl_focus", "Should not revive");
            let mut stale_changes = AutoCommit::load(&stale.save()).unwrap();
            deleted.merge_incoming(&mut stale_changes).unwrap();
            assert!(playlist_is_tombstoned(&deleted, "pl_focus"));
            assert!(!has_playlist(&deleted, "pl_focus"));
            assert!(
                load_library(&deleted)
                    .playlists
                    .iter()
                    .all(|playlist| playlist.id != "pl_focus")
            );
        }
    }

    fn seeded_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        seed(&mut doc, &seed::sample_library());
        doc
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn liked_and_saved_sets_round_trip_and_replicate() {
        // Phase `toggle_liked_round_trips_through_doc`.
        {
            let mut doc = seeded_doc();
            assert!(!load_library(&doc).is_liked("tk_g2"));
            assert!(toggle_liked(&mut doc, "tk_g2"));
            assert!(load_library(&doc).is_liked("tk_g2"));
            assert!(!toggle_liked(&mut doc, "tk_g2"));
            assert!(!load_library(&doc).is_liked("tk_g2"));
        }

        // Phase `toggle_saved_album_round_trips`.
        {
            let mut doc = seeded_doc();
            // al_glasslight is seeded saved; al_strata is not.
            assert!(load_library(&doc).is_album_saved("al_glasslight"));
            assert!(toggle_saved_album(&mut doc, "al_strata"));
            assert!(load_library(&doc).is_album_saved("al_strata"));
            assert!(!toggle_saved_album(&mut doc, "al_strata"));
            assert!(!load_library(&doc).is_album_saved("al_strata"));
        }

        // Phase `liked_set_replicates_between_devices`.
        {
            // Device A seeds, B loads, B likes a track, A merges and converges.
            let mut a = seeded_doc();
            let mut b = AutoCommit::load(&a.save()).unwrap();
            set_liked(&mut b, "tk_v2", true);
            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            assert!(load_library(&a).is_liked("tk_v2"));
        }
    }
}
