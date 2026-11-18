//! The types Music's rows are items of: a row says what it is — its type's
//! noun and the app's name — so search and an agent name it as they name a
//! resource.

use ui::tilo_resource::words::RowType;

pub(crate) struct Library;

impl RowType for Library {
    const KIND: &'static str = "music.library";
    const LABEL: &'static str = "Library";
}

pub(crate) struct Player;

impl RowType for Player {
    const KIND: &'static str = "music.player";
    const LABEL: &'static str = "Player";
}

pub(crate) struct Artist;

impl RowType for Artist {
    const KIND: &'static str = "music.artist";
    const LABEL: &'static str = "Artist";
}

pub(crate) struct Album;

impl RowType for Album {
    const KIND: &'static str = "music.album";
    const LABEL: &'static str = "Album";
}

pub(crate) struct Playlist;

impl RowType for Playlist {
    const KIND: &'static str = "music.playlist";
    const LABEL: &'static str = "Playlist";
}

pub(crate) struct SpotifyRemoteItem;

impl RowType for SpotifyRemoteItem {
    const KIND: &'static str = "music.spotify-remote-item";
    const LABEL: &'static str = "Remote item";
}

pub(crate) struct SpotifyDevice;

impl RowType for SpotifyDevice {
    const KIND: &'static str = "music.spotify-device";
    const LABEL: &'static str = "Device";
}

pub(crate) struct RecommendationPreferences;

impl RowType for RecommendationPreferences {
    const KIND: &'static str = "music.recommendation-preferences";
    const LABEL: &'static str = "Recommendation preferences";
}

pub(crate) struct RecommendationInput;

impl RowType for RecommendationInput {
    const KIND: &'static str = "music.recommendation-input";
    const LABEL: &'static str = "Recommendation input";
}

pub(crate) struct RecommendationArtist;

impl RowType for RecommendationArtist {
    const KIND: &'static str = "music.recommendation-artist";
    const LABEL: &'static str = "Recommended artist";
}

pub(crate) struct Recommendation;

impl RowType for Recommendation {
    const KIND: &'static str = "music.recommendation";
    const LABEL: &'static str = "Recommendation";
}
