//! Historical schema fixtures built through the supported typed field writer.
//! Atom preserves the old scalar strings; Vec preserves the old playlist List.
//! Catalogue identities deliberately live only in their map keys.
use std::collections::BTreeMap;

use automerge::{AutoCommit, ROOT};
use ui::tilo_resource::{self, Atom, Fields, Model};

#[derive(Model, Default)]
struct LegacyCatalogue {
    artists: BTreeMap<String, LegacyArtist>,
    albums: BTreeMap<String, LegacyAlbum>,
    tracks: BTreeMap<String, LegacyTrack>,
}

#[derive(Model)]
struct LegacyArtist {
    name: Atom,
    genre: Atom,
    monthly_listeners: i64,
}

#[derive(Model)]
struct LegacyAlbum {
    title: Atom,
    artist_id: Atom,
    year: i64,
}

#[derive(Model)]
struct LegacyTrack {
    title: Atom,
    album_id: Atom,
    artist_id: Atom,
    track_no: i64,
    duration_s: i64,
    yt_id: Atom,
    plays: i64,
}

#[derive(Model)]
pub(super) struct LegacyPlaylist {
    id: Atom,
    name: Atom,
    description: Atom,
    pub cover: Atom,
    track_membership: BTreeMap<String, bool>,
    track_order: BTreeMap<String, i64>,
}

impl LegacyPlaylist {
    pub fn new(id: &str, entries: &[(&str, i64, bool)]) -> Self {
        Self {
            id: Atom(id.into()),
            name: Atom(format!("{id} – retained 🎧")),
            description: Atom("First line\nSecond line".into()),
            cover: Atom(format!("https://example.com/{id}.png")),
            track_membership: entries
                .iter()
                .map(|(id, _, present)| ((*id).into(), *present))
                .collect(),
            track_order: entries
                .iter()
                .map(|(id, rank, _)| ((*id).into(), *rank))
                .collect(),
        }
    }
}

#[derive(Model, Default)]
pub(super) struct LegacyLibrary {
    library: LegacyCatalogue,
    pub playlists: Vec<LegacyPlaylist>,
    liked: BTreeMap<String, bool>,
    saved_albums: BTreeMap<String, bool>,
    followed_artists: BTreeMap<String, bool>,
    playlist_tombstones: BTreeMap<String, bool>,
    // Some(false) is an explicit legacy false; None leaves an absent key absent.
    // Plain bool defaults would omit false on the initial typed write.
    #[model(rename = "recommendations/input/likes")]
    likes_input: Option<bool>,
    #[model(rename = "recommendations/input/saved-albums")]
    saved_input: Option<bool>,
    #[model(rename = "recommendations/input/followed-artists")]
    followed_input: Option<bool>,
    #[model(rename = "recommendations/preferred-artist/ar_midnight")]
    preferred: Option<bool>,
    #[model(rename = "recommendations/preferred-artist/not-preferred")]
    not_preferred: Option<bool>,
    #[model(rename = "recommendations/dismissed/tk_a2")]
    dismissed: Option<bool>,
    #[model(rename = "recommendations/dismissed/tk_a3")]
    not_dismissed: Option<bool>,
    #[model(rename = "unrelated-retained-value")]
    unrelated: Atom,
}

impl LegacyLibrary {
    pub fn empty() -> Self {
        Self {
            likes_input: Some(true),
            saved_input: Some(true),
            followed_input: Some(true),
            unrelated: Atom("keep me".into()),
            ..Self::default()
        }
    }

    pub fn document(&self) -> AutoCommit {
        let mut doc = AutoCommit::new();
        self.write_fields(&mut doc, &ROOT).unwrap();
        doc.commit();
        doc
    }
}

#[derive(Model)]
pub(super) struct ScalarLayout {
    pub playlists: Atom,
}

#[derive(Model)]
pub(super) struct InvalidMembership {
    pub track_membership: BTreeMap<String, Atom>,
}

pub(super) fn populated_fixture() -> LegacyLibrary {
    let mut fixture = LegacyLibrary::empty();
    let sample = crate::seed::sample_library();
    let artist = sample.artist("ar_midnight").unwrap();
    fixture.library.artists.insert(
        artist.id.clone(),
        LegacyArtist {
            name: Atom(artist.name.clone()),
            genre: Atom(artist.genre.clone()),
            monthly_listeners: i64::try_from(artist.monthly_listeners).unwrap(),
        },
    );
    let album = sample.album("al_afterhours").unwrap();
    fixture.library.albums.insert(
        album.id.clone(),
        LegacyAlbum {
            title: Atom(album.title.clone()),
            artist_id: Atom(album.artist_id.clone()),
            year: i64::from(album.year),
        },
    );
    for id in ["tk_a1", "tk_a2", "tk_a3"] {
        let track = sample.track(id).unwrap();
        fixture.library.tracks.insert(
            id.into(),
            LegacyTrack {
                title: Atom(track.title.clone()),
                album_id: Atom(track.album_id.clone()),
                artist_id: Atom(track.artist_id.clone()),
                track_no: i64::from(track.track_no),
                duration_s: i64::from(track.duration_s),
                yt_id: Atom(track.yt_id.clone()),
                plays: i64::try_from(track.plays).unwrap(),
            },
        );
    }
    fixture.playlists = vec![
        LegacyPlaylist::new(
            "p-z",
            &[
                ("tk_a1", 1024, true),
                ("tk_a2", -5, true),
                ("tk_a3", 17, false),
            ],
        ),
        LegacyPlaylist::new("p-a", &[("tk_a3", 0, true), ("tk_a1", 0, true)]),
        LegacyPlaylist::new("deleted", &[("removed-track", 0, true)]),
    ];
    fixture.playlist_tombstones.insert("deleted".into(), true);
    fixture.liked = [("tk_a1".into(), true), ("tk_a3".into(), false)].into();
    fixture.saved_albums.insert("al_afterhours".into(), true);
    fixture.followed_artists.insert("ar_midnight".into(), true);
    fixture.likes_input = Some(false);
    fixture.saved_input = Some(false);
    fixture.preferred = Some(true);
    fixture.dismissed = Some(true);
    fixture.not_preferred = Some(false);
    fixture.not_dismissed = Some(false);
    fixture
}
