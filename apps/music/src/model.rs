//! Pure data model for the Music app.
//!
//! The catalog (artists / albums / tracks) and user-authored playlists are
//! collaborative plain structs mirrored by [`crate::doc`] into Automerge.
//! Cache/download and live playback values share these render types, but are
//! authoritative only on the current device; widgets overlay them after a
//! shared-library refresh rather than syncing them to peers.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Stable identifiers. We use short string ids (not UUIDs) for the seeded
/// sample catalog so the document is readable and the ids are stable across
/// runs; user-created playlists get UUID ids.
pub type ArtistId = String;
pub type AlbumId = String;
pub type TrackId = String;
pub type PlaylistId = String;

/// One artist in the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    pub id: ArtistId,
    pub name: String,
    /// Free-text genre tag, shown on the artist detail header.
    pub genre: String,
    /// Monthly-listeners style stat, purely cosmetic.
    pub monthly_listeners: u64,
}

/// One album (or single/EP) in the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub title: String,
    pub artist_id: ArtistId,
    pub year: u32,
}

/// One track. `yt_id` is the YouTube-Music video id used to resolve a
/// streamable audio URL via the [`crate::backend`] worker; for the offline
/// sample library it is a synthetic id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    pub album_id: AlbumId,
    pub artist_id: ArtistId,
    /// Position within its album (1-based).
    pub track_no: u32,
    /// Duration in whole seconds.
    pub duration_s: u32,
    /// YouTube-Music video id (streaming source key).
    pub yt_id: String,
    /// Play count, used by "most played" sorting and stats.
    pub plays: u64,
}

impl Track {
    pub fn duration_ms(&self) -> u64 {
        u64::from(self.duration_s) * 1000
    }
}

/// A user-curated playlist: an ordered list of track ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    pub description: String,
    pub track_ids: Vec<TrackId>,
    /// Stable sparse order keys for each membership. The rendered
    /// `track_ids` vector is sorted from these keys; keeping them in the
    /// projection lets UI and resource actions author an absolute move rather
    /// than replaying a fragile pair of list indices.
    #[serde(default)]
    pub track_ranks: BTreeMap<TrackId, i64>,
    /// Optional portable playlist cover: an HTTPS URL or a bounded embedded PNG
    /// data URL normalized from the file portal. Empty ⇒ procedural placeholder.
    #[serde(default)]
    pub cover: String,
}

impl Playlist {
    const ORDER_STEP: i64 = 1_048_576;

    /// Absolute sparse rank for appending a new membership.
    pub fn append_rank(&self) -> i64 {
        self.track_ranks
            .values()
            .copied()
            .max()
            .unwrap_or(-Self::ORDER_STEP)
            .saturating_add(Self::ORDER_STEP)
    }

    /// Absolute sparse rank that moves `track` to `to` in the visible order.
    /// Ties are resolved by stable track id by the document projection, so
    /// even exhausted gaps remain deterministic across replicas.
    pub fn rank_for_move(&self, track: &str, to: usize) -> Option<i64> {
        let mut remaining = self
            .track_ids
            .iter()
            .filter(|id| id.as_str() != track)
            .collect::<Vec<_>>();
        let insert = to.min(remaining.len());
        let previous = insert.checked_sub(1).and_then(|index| {
            remaining
                .get(index)
                .and_then(|id| self.track_ranks.get(id.as_str()))
                .copied()
        });
        let next = remaining
            .get(insert)
            .and_then(|id| self.track_ranks.get(id.as_str()))
            .copied();
        remaining.clear();
        Some(match (previous, next) {
            (Some(previous), Some(next)) if next.saturating_sub(previous) > 1 => {
                previous + (next - previous) / 2
            }
            (Some(previous), Some(_) | None) => previous.saturating_add(1),
            (None, Some(next)) => next.saturating_sub(Self::ORDER_STEP),
            (None, None) if self.track_ids.iter().any(|id| id == track) => 0,
            (None, None) => return None,
        })
    }
}

/// Per-track cache state. `Cached` is reserved exclusively for a durable
/// local media file which the audio engine can play without network access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum CacheState {
    /// Not cached — streamed live from the source.
    #[default]
    Streamed,
    /// Download in flight; `0.0..=1.0`.
    Downloading(f32),
    /// Fully downloaded and available offline.
    Cached,
}

/// Repeat mode for the play queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    Off,
    /// Repeat the whole queue.
    All,
    /// Repeat the current track.
    One,
}

impl RepeatMode {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "all" => RepeatMode::All,
            "one" => RepeatMode::One,
            _ => RepeatMode::Off,
        }
    }
}

/// Live playback state for this device. Audio output, queue, shuffle/repeat
/// and volume never cross the CRDT boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playback {
    /// Currently-loaded track, if any.
    pub current: Option<TrackId>,
    /// The play queue (ordered list of track ids). `current` is normally
    /// `queue[queue_index]`.
    pub queue: Vec<TrackId>,
    /// Index of `current` within `queue`.
    pub queue_index: usize,
    /// Whether transport is playing (vs paused).
    pub playing: bool,
    /// Last persisted playhead position, in milliseconds.
    pub position_ms: u64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Volume `0..=100`.
    pub volume: u8,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            current: None,
            queue: Vec::new(),
            queue_index: 0,
            playing: false,
            position_ms: 0,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 80,
        }
    }
}

/// Stable id of the synthetic "Liked Songs" playlist surfaced in the UI. It
/// is not a stored [`Playlist`] — it is a virtual projection of [`Library::liked`]
/// so it always reflects the like set without a second source of truth.
pub const LIKED_PLAYLIST_ID: &str = "pl_liked";

/// The complete in-memory catalog + user data. The app holds one of these
/// as a render snapshot, refreshed from the CRDT each `sync`.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub artists: BTreeMap<ArtistId, Artist>,
    pub albums: BTreeMap<AlbumId, Album>,
    pub tracks: BTreeMap<TrackId, Track>,
    pub playlists: Vec<Playlist>,
    pub cache: BTreeMap<TrackId, CacheState>,
    /// Track ids the user has liked ("Liked Songs"). Ordered set so the
    /// virtual Liked playlist is stable across devices.
    pub liked: BTreeSet<TrackId>,
    /// Album ids the user has saved to their library ("Add to library" on an
    /// album card/header).
    pub saved_albums: BTreeSet<AlbumId>,
    /// Artist ids (channel ids) the user follows ("Follow" on an artist page).
    pub followed_artists: BTreeSet<ArtistId>,
}

impl Library {
    pub fn artist(&self, id: &str) -> Option<&Artist> {
        self.artists.get(id)
    }
    pub fn album(&self, id: &str) -> Option<&Album> {
        self.albums.get(id)
    }
    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.get(id)
    }
    pub fn artist_name(&self, id: &str) -> &str {
        self.artists
            .get(id)
            .map_or("Unknown Artist", |a| a.name.as_str())
    }
    pub fn album_title(&self, id: &str) -> &str {
        self.albums
            .get(id)
            .map_or("Unknown Album", |a| a.title.as_str())
    }
    pub fn cache_state(&self, id: &str) -> CacheState {
        self.cache.get(id).copied().unwrap_or_default()
    }

    /// Tracks of an album, ordered by track number.
    pub fn album_tracks(&self, album: &str) -> Vec<&Track> {
        let mut v: Vec<&Track> = self
            .tracks
            .values()
            .filter(|t| t.album_id == album)
            .collect();
        v.sort_by_key(|t| t.track_no);
        v
    }

    /// Albums of an artist, newest first.
    pub fn artist_albums(&self, artist: &str) -> Vec<&Album> {
        let mut v: Vec<&Album> = self
            .albums
            .values()
            .filter(|a| a.artist_id == artist)
            .collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.year));
        v
    }

    /// All albums, sorted by artist then year.
    pub fn albums_sorted(&self) -> Vec<&Album> {
        let mut v: Vec<&Album> = self.albums.values().collect();
        v.sort_by(|a, b| {
            self.artist_name(&a.artist_id)
                .cmp(self.artist_name(&b.artist_id))
                .then(a.year.cmp(&b.year))
        });
        v
    }

    /// All artists, alphabetical.
    pub fn artists_sorted(&self) -> Vec<&Artist> {
        let mut v: Vec<&Artist> = self.artists.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// All tracks, sorted by artist / album / track number.
    pub fn tracks_sorted(&self) -> Vec<&Track> {
        let mut v: Vec<&Track> = self.tracks.values().collect();
        v.sort_by(|a, b| {
            self.artist_name(&a.artist_id)
                .cmp(self.artist_name(&b.artist_id))
                .then(
                    self.album_title(&a.album_id)
                        .cmp(self.album_title(&b.album_id)),
                )
                .then(a.track_no.cmp(&b.track_no))
        });
        v
    }

    /// Most-played tracks, descending.
    pub fn top_tracks(&self, n: usize) -> Vec<&Track> {
        let mut v: Vec<&Track> = self.tracks.values().collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.plays));
        v.into_iter().take(n).collect()
    }

    /// Cached (offline) tracks.
    pub fn downloaded_tracks(&self) -> Vec<&Track> {
        let mut v: Vec<&Track> = self
            .tracks
            .values()
            .filter(|t| matches!(self.cache_state(&t.id), CacheState::Cached))
            .collect();
        v.sort_by(|a, b| a.title.cmp(&b.title));
        v
    }

    /// Case-insensitive substring search across tracks, albums and artists.
    pub fn search(&self, q: &str) -> SearchResults {
        let needle = q.trim().to_lowercase();
        if needle.is_empty() {
            return SearchResults::default();
        }
        // Genre matches too, so the "Browse all" category chips surface a
        // genre's artists/albums/songs (and a plain genre-name query works).
        let artist_genre = |id: &str| {
            self.artists
                .get(id)
                .map(|a| a.genre.to_lowercase())
                .unwrap_or_default()
        };
        let mut tracks: Vec<TrackId> = self
            .tracks
            .values()
            .filter(|t| {
                t.title.to_lowercase().contains(&needle)
                    || self
                        .artist_name(&t.artist_id)
                        .to_lowercase()
                        .contains(&needle)
                    || artist_genre(&t.artist_id).contains(&needle)
            })
            .map(|t| t.id.clone())
            .collect();
        tracks.sort_by_key(|id| self.tracks.get(id).map_or(0, |t| u64::MAX - t.plays));
        let albums: Vec<AlbumId> = self
            .albums
            .values()
            .filter(|a| {
                a.title.to_lowercase().contains(&needle)
                    || artist_genre(&a.artist_id).contains(&needle)
            })
            .map(|a| a.id.clone())
            .collect();
        let artists: Vec<ArtistId> = self
            .artists
            .values()
            .filter(|a| {
                a.name.to_lowercase().contains(&needle) || a.genre.to_lowercase().contains(&needle)
            })
            .map(|a| a.id.clone())
            .collect();
        SearchResults {
            tracks,
            albums,
            artists,
        }
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    pub fn total_tracks(&self) -> usize {
        self.tracks.len()
    }

    /// Whether a track is in "Liked Songs".
    pub fn is_liked(&self, id: &str) -> bool {
        self.liked.contains(id)
    }

    /// Whether an album is saved to the library.
    pub fn is_album_saved(&self, id: &str) -> bool {
        self.saved_albums.contains(id)
    }

    /// Whether the user follows an artist.
    pub fn is_artist_followed(&self, id: &str) -> bool {
        self.followed_artists.contains(id)
    }

    /// Liked tracks ordered by artist / album / track number (a virtual
    /// "Liked Songs" playlist surfaced in the UI).
    pub fn liked_tracks(&self) -> Vec<&Track> {
        let mut v: Vec<&Track> = self
            .liked
            .iter()
            .filter_map(|id| self.tracks.get(id))
            .collect();
        v.sort_by(|a, b| {
            self.artist_name(&a.artist_id)
                .cmp(self.artist_name(&b.artist_id))
                .then(
                    self.album_title(&a.album_id)
                        .cmp(self.album_title(&b.album_id)),
                )
                .then(a.track_no.cmp(&b.track_no))
        });
        v
    }

    /// Track ids of the "Liked Songs" virtual playlist, in display order.
    pub fn liked_ids(&self) -> Vec<TrackId> {
        self.liked_tracks()
            .into_iter()
            .map(|t| t.id.clone())
            .collect()
    }
}

/// Result buckets for a library search.
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub tracks: Vec<TrackId>,
    pub albums: Vec<AlbumId>,
    pub artists: Vec<ArtistId>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.albums.is_empty() && self.artists.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn sample_library_orders_albums_searches_and_cycles_repeat() {
        // Phase `sample_library_is_populated`.
        {
            let lib = seed::sample_library();
            assert!(lib.artists.len() >= 4);
            assert!(lib.albums.len() >= 6);
            assert!(lib.tracks.len() >= 20);
            assert!(!lib.playlists.is_empty());
        }

        // Phase `album_tracks_are_ordered`.
        {
            let lib = seed::sample_library();
            for album in lib.albums.keys() {
                let ts = lib.album_tracks(album);
                for w in ts.windows(2) {
                    assert!(w[0].track_no <= w[1].track_no);
                }
            }
        }

        // Phase `search_finds_by_title_and_artist`.
        {
            let lib = seed::sample_library();
            // Every track title should be findable by its own title.
            let some = lib.tracks.values().next().unwrap().clone();
            let res = lib.search(&some.title);
            assert!(res.tracks.contains(&some.id));
            // Empty query yields nothing.
            assert!(lib.search("   ").is_empty());
        }

        // Phase `repeat_cycles`.
        {
            assert_eq!(RepeatMode::Off.next(), RepeatMode::All);
            assert_eq!(RepeatMode::All.next(), RepeatMode::One);
            assert_eq!(RepeatMode::One.next(), RepeatMode::Off);
        }
    }
}
