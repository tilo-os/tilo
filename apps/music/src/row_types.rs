//! The types Music's rows are items of: a row says what it is — its type's
//! noun and the app's name — so search and an agent name it as they name a
//! resource, and it offers the effects its type declares.

use ui::tilo_resource;

pub(crate) struct Library;

impl ui::tilo_resource::words::RowType for Library {
    const KIND: &'static str = "music.library";
    const LABEL: &'static str = "Library";
}

/// This device's player, the transport's controls act on.
#[derive(tilo_resource::Row)]
#[row(kind = "music.player", label = "Player", effects = crate::actions::MusicEffect)]
pub(crate) struct Player;

/// This device's queue.
#[derive(tilo_resource::Row)]
#[row(kind = "music.queue", label = "Queue", effects = crate::actions::MusicEffect)]
pub(crate) struct Queue;

/// One occurrence in this device's queue.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.queue-item",
    label = "Queue item",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct QueueItem;

/// A song of the library, or of a catalogue the library does not hold.
#[derive(tilo_resource::Row)]
#[row(kind = "music.track", label = "Song", effects = crate::actions::MusicEffect)]
pub(crate) struct Track;

/// An artist of the library or a catalogue.
#[derive(tilo_resource::Row)]
#[row(kind = "music.artist", label = "Artist", effects = crate::actions::MusicEffect)]
pub(crate) struct Artist;

/// An album of the library or a catalogue.
#[derive(tilo_resource::Row)]
#[row(kind = "music.album", label = "Album", effects = crate::actions::MusicEffect)]
pub(crate) struct Album;

/// A playlist of the library or a catalogue.
#[derive(tilo_resource::Row)]
#[row(kind = "music.playlist", label = "Playlist", effects = crate::actions::MusicEffect)]
pub(crate) struct Playlist;

/// One song's membership of a playlist.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.playlist-track",
    label = "Playlist song",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct Membership;

/// A file this device has open outside any library.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.local-file",
    label = "Local file",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct LocalFile;

/// The catalogue this device plays from, and the accounts behind it.
#[derive(tilo_resource::Row)]
#[row(kind = "music.source", label = "Source", effects = crate::actions::MusicEffect)]
pub(crate) struct Source;

pub(crate) struct SpotifyRemoteItem;

impl ui::tilo_resource::words::RowType for SpotifyRemoteItem {
    const KIND: &'static str = "music.spotify-remote-item";
    const LABEL: &'static str = "Remote item";
}

/// One device Spotify can play on.
#[derive(tilo_resource::Row)]
#[row(kind = "music.spotify-device", label = "Device", effects = crate::actions::MusicEffect)]
pub(crate) struct SpotifyDevice;

/// The recommendations page's own preferences.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.recommendation-preferences",
    label = "Recommendation preferences",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct RecommendationPreferences;

/// One input the recommendations weigh.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.recommendation-input",
    label = "Recommendation input",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct RecommendationInput;

/// An artist a listener prefers, or a song's artist the recommendations name.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.recommendation-artist",
    label = "Recommended artist",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct RecommendationArtist;

/// One recommended song, or one the person dismissed.
#[derive(tilo_resource::Row)]
#[row(
    kind = "music.recommendation",
    label = "Recommendation",
    effects = crate::actions::MusicEffect
)]
pub(crate) struct Recommendation;
