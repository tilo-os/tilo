//! Explainable library exploration. No provider request or listening history.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model, Set};

use crate::model::{Library, Track};

mod resources;
pub use resources::{PREFERENCES, resource_by_id, resources};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    Likes,
    SavedAlbums,
    FollowedArtists,
}

impl Input {
    pub const ALL: [Self; 3] = [Self::Likes, Self::SavedAlbums, Self::FollowedArtists];

    pub fn key(self) -> &'static str {
        match self {
            Self::Likes => "likes",
            Self::SavedAlbums => "saved-albums",
            Self::FollowedArtists => "followed-artists",
        }
    }

    pub fn label(self) -> String {
        crate::i18n::t(&format!("music-recommendations-input-{}", self.key()))
    }
}

/// What recommendations explore from, and the person's feedback on them.
#[derive(Clone, Debug, PartialEq, Model)]
pub struct Preferences {
    /// Explore from liked songs
    pub likes: bool,
    /// Explore from saved albums
    pub saved_albums: bool,
    /// Explore from followed artists
    pub followed_artists: bool,
    /// Preferred artists, by id.
    pub preferred_artists: Set<String>,
    /// Songs not to suggest again, by track id.
    pub dismissed: Set<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            likes: true,
            saved_albums: true,
            followed_artists: true,
            preferred_artists: Set::new(),
            dismissed: Set::new(),
        }
    }
}

impl Preferences {
    /// Whether `track` was dismissed.
    pub fn is_dismissed(&self, track: &str) -> bool {
        self.dismissed.contains(&track.to_owned())
    }

    /// Whether `artist` is preferred.
    pub fn prefers(&self, artist: &str) -> bool {
        self.preferred_artists.contains(&artist.to_owned())
    }

    /// The path of `input`'s switch.
    pub fn input_path(input: Input) -> tilo_resource::Path<Library, bool> {
        let paths = Library::paths().recommendations;
        match input {
            Input::Likes => paths.likes,
            Input::SavedAlbums => paths.saved_albums,
            Input::FollowedArtists => paths.followed_artists,
        }
    }

    pub fn enabled(&self, input: Input) -> bool {
        match input {
            Input::Likes => self.likes,
            Input::SavedAlbums => self.saved_albums,
            Input::FollowedArtists => self.followed_artists,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub track: String,
    pub reason: Reason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    PreferredArtist(String),
    LikedSong(String),
    SavedAlbum(String),
    FollowedArtist(String),
    SharedGenre { artist: String, genre: String },
}

impl Reason {
    pub fn label(&self, lib: &Library) -> String {
        let (key, name) = match self {
            Self::PreferredArtist(id) => ("preferred", lib.artist_name(id)),
            Self::LikedSong(id) => ("liked", lib.track(id).map_or("", |t| t.title.as_str())),
            Self::SavedAlbum(id) => ("saved", lib.album_title(id)),
            Self::FollowedArtist(id) => ("followed", lib.artist_name(id)),
            Self::SharedGenre { artist, genre } => {
                return crate::i18n::t_args(
                    "music-recommendations-reason-genre",
                    &[
                        ("artist", lib.artist_name(artist).into()),
                        ("genre", genre.clone().into()),
                    ],
                );
            }
        };
        crate::i18n::t_args(
            &format!("music-recommendations-reason-{key}"),
            &[("name", name.into())],
        )
    }
}

#[derive(Default)]
struct Signals {
    liked_artists: BTreeMap<String, String>,
    saved_artists: BTreeMap<String, String>,
    liked_genres: BTreeMap<String, String>,
}

impl Signals {
    fn new(lib: &Library) -> Self {
        let mut signals = Self::default();
        if lib.recommendations.likes {
            for track in lib.liked.iter().filter_map(|id| lib.track(id)) {
                signals
                    .liked_artists
                    .entry(track.artist_id.clone())
                    .or_insert_with(|| track.id.clone());
                if let Some(artist) = lib.artist(&track.artist_id) {
                    let genre = artist.genre.trim().to_ascii_lowercase();
                    if !genre.is_empty() {
                        signals
                            .liked_genres
                            .entry(genre)
                            .or_insert_with(|| artist.id.clone());
                    }
                }
            }
        }
        if lib.recommendations.saved_albums {
            for album in lib.saved_albums.iter().filter_map(|id| lib.album(id)) {
                signals
                    .saved_artists
                    .entry(album.artist_id.clone())
                    .or_insert_with(|| album.id.clone());
            }
        }
        signals
    }
}

fn reason(lib: &Library, track: &Track, signals: &Signals) -> Option<(u8, Reason)> {
    let prefs = &lib.recommendations;
    if prefs.prefers(&track.artist_id) {
        return Some((5, Reason::PreferredArtist(track.artist_id.clone())));
    }
    if let Some(liked) = signals.liked_artists.get(&track.artist_id) {
        return Some((4, Reason::LikedSong(liked.clone())));
    }
    if let Some(album) = signals.saved_artists.get(&track.artist_id) {
        return Some((3, Reason::SavedAlbum(album.clone())));
    }
    if prefs.followed_artists && lib.is_artist_followed(&track.artist_id) {
        return Some((2, Reason::FollowedArtist(track.artist_id.clone())));
    }
    let artist = lib.artist(&track.artist_id)?;
    signals
        .liked_genres
        .get(&artist.genre.trim().to_ascii_lowercase())
        .map(|anchor| {
            (
                1,
                Reason::SharedGenre {
                    artist: anchor.clone(),
                    genre: artist.genre.clone(),
                },
            )
        })
}

/// Explore known, unliked library songs. Stable ranking; plays are never read.
pub(crate) fn suggestion(lib: &Library, id: &str) -> Option<Suggestion> {
    if lib.is_liked(id) || lib.recommendations.is_dismissed(id) {
        return None;
    }
    let track = lib.track(id)?;
    reason(lib, track, &Signals::new(lib)).map(|(_, reason)| Suggestion {
        track: id.into(),
        reason,
    })
}

/// Explore known, unliked library songs. Stable ranking; plays are never read.
pub fn suggestions(lib: &Library, limit: usize) -> Vec<Suggestion> {
    let signals = Signals::new(lib);
    let mut matches = lib
        .tracks
        .values()
        .filter(|track| !lib.is_liked(&track.id) && !lib.recommendations.is_dismissed(&track.id))
        .filter_map(|track| {
            reason(lib, track, &signals).map(|(score, reason)| (score, track.id.clone(), reason))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches
        .into_iter()
        .take(limit)
        .map(|(_, track, reason)| Suggestion { track, reason })
        .collect()
}

#[cfg(test)]
mod tests;
