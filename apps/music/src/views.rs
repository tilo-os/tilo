//! Music's resource types as its views and its search read them, and the
//! connected streaming catalogue as a remote source.
//!
//! Songs, albums, artists and playlists each declare their fields with the
//! role they play where an item is shown, which fields typed text finds, and
//! which narrow by a chip: Artist and Year by value, Liked and In library on
//! or off. The library's items are the local side of every query view; the
//! catalogue (`YouTube` Music, or Spotify when connected) streams in below
//! them, its quick suggestions first and its full answer after.

use std::sync::{Arc, Mutex, PoisonError};

use ui::framework::views::{
    Cancel, FieldDecl, FieldRole, FilterKind, Form, Item, KindDecl, QuerySpec, RemoteError,
    RemoteSource,
};

use crate::backend::{AlbumHit, ArtistHit, MusicBackend, PlaylistHit, SearchBundle, TrackHit};
use crate::model::Library;
use crate::search_type::{ALBUM_TYPE_ID, ARTIST_TYPE_ID, PLAYLIST_TYPE_ID, SONG_TYPE_ID};

pub const TITLE: &str = "title";
pub const ARTIST: &str = "artist";
pub const ALBUM: &str = "album";
pub const YEAR: &str = "year";
pub const LIKED: &str = "liked";
pub const IN_LIBRARY: &str = "in_library";

/// Music's resource types, in the order their results are grouped.
pub fn kinds() -> Vec<KindDecl> {
    let artist = || {
        FieldDecl::new(ARTIST, "Artist")
            .role(FieldRole::Subtitle)
            .searchable()
            .filter(FilterKind::Value)
    };
    let year = || {
        FieldDecl::new(YEAR, "Year")
            .role(FieldRole::Date)
            .filter(FilterKind::Value)
    };
    let in_library = || FieldDecl::new(IN_LIBRARY, "In library").filter(FilterKind::Toggle);
    vec![
        KindDecl::new(SONG_TYPE_ID, "Song", "Songs", Form::Row)
            .field(FieldDecl::title(TITLE, "Title"))
            .field(artist())
            .field(
                FieldDecl::new(ALBUM, "Album")
                    .role(FieldRole::Detail)
                    .searchable(),
            )
            .field(year())
            .field(FieldDecl::new(LIKED, "Liked").filter(FilterKind::Toggle))
            .field(in_library()),
        KindDecl::new(ALBUM_TYPE_ID, "Album", "Albums", Form::Tile)
            .field(FieldDecl::title(TITLE, "Title"))
            .field(artist())
            .field(year())
            .field(in_library()),
        KindDecl::new(ARTIST_TYPE_ID, "Artist", "Artists", Form::Tile)
            .field(FieldDecl::title(TITLE, "Name"))
            .field(in_library()),
        KindDecl::new(PLAYLIST_TYPE_ID, "Playlist", "Playlists", Form::Tile)
            .field(FieldDecl::title(TITLE, "Name"))
            .field(in_library()),
    ]
}

fn year_text(year: u32) -> String {
    if year > 0 {
        year.to_string()
    } else {
        String::new()
    }
}

/// The library's songs, albums, artists and playlists as items: the local
/// side of a query view.
pub fn library_items(lib: &Library) -> Vec<Item> {
    let mut items = Vec::with_capacity(
        lib.tracks
            .len()
            .saturating_add(lib.albums.len())
            .saturating_add(lib.artists.len()),
    );
    for track in lib.tracks.values() {
        let year = lib
            .albums
            .get(&track.album_id)
            .map_or(0, |album| album.year);
        items.push(
            Item::new(SONG_TYPE_ID, track.id.clone())
                .with(TITLE, track.title.clone())
                .with(ARTIST, lib.artist_name(&track.artist_id))
                .with(ALBUM, lib.album_title(&track.album_id))
                .with(YEAR, year_text(year))
                .flag(LIKED, lib.is_liked(&track.id))
                .flag(IN_LIBRARY, true),
        );
    }
    for album in lib.albums.values() {
        items.push(
            Item::new(ALBUM_TYPE_ID, album.id.clone())
                .with(TITLE, album.title.clone())
                .with(ARTIST, lib.artist_name(&album.artist_id))
                .with(YEAR, year_text(album.year))
                .flag(IN_LIBRARY, true),
        );
    }
    for artist in lib.artists.values() {
        items.push(
            Item::new(ARTIST_TYPE_ID, artist.id.clone())
                .with(TITLE, artist.name.clone())
                .flag(IN_LIBRARY, true),
        );
    }
    for playlist in lib.playlists() {
        items.push(
            Item::new(PLAYLIST_TYPE_ID, playlist.id.clone())
                .with(TITLE, playlist.name.clone())
                .flag(IN_LIBRARY, true),
        );
    }
    items
}

/// What a catalogue answer's items carry, so the app can make each playable,
/// likeable and openable before anything is stored.
#[derive(Clone, Debug)]
pub enum Fetched {
    Track(TrackHit),
    Album(AlbumHit),
    Artist(ArtistHit),
    Playlist(PlaylistHit),
}

/// The items of a catalogue answer, best first within each type.
pub fn bundle_items(bundle: &SearchBundle) -> Vec<Item> {
    let mut items = Vec::new();
    for hit in &bundle.tracks {
        items.push(
            Item::new(SONG_TYPE_ID, crate::app::live_track_id(&hit.video_id))
                .with(TITLE, hit.title.clone())
                .with(ARTIST, hit.artist.clone())
                .with(ALBUM, hit.album.clone().unwrap_or_default())
                .payload(Arc::new(Fetched::Track(hit.clone()))),
        );
    }
    for hit in &bundle.albums {
        items.push(
            Item::new(ALBUM_TYPE_ID, hit.id.clone())
                .with(TITLE, hit.title.clone())
                .with(ARTIST, hit.artist.clone())
                .with(YEAR, year_text(hit.year))
                .payload(Arc::new(Fetched::Album(hit.clone()))),
        );
    }
    for hit in &bundle.artists {
        items.push(
            Item::new(ARTIST_TYPE_ID, hit.id.clone())
                .with(TITLE, hit.name.clone())
                .payload(Arc::new(Fetched::Artist(hit.clone()))),
        );
    }
    for hit in &bundle.playlists {
        items.push(
            Item::new(PLAYLIST_TYPE_ID, hit.id.clone())
                .with(TITLE, hit.title.clone())
                .payload(Arc::new(Fetched::Playlist(hit.clone()))),
        );
    }
    items
}

/// Whether the catalogue answers `query`: it has words to look for, and no
/// chip asks for the person's own marks, which only the library knows.
fn catalogue_accepts(query: &QuerySpec) -> bool {
    !query.text.trim().is_empty()
        && !query
            .chips
            .iter()
            .any(|chip| chip.key == LIKED || chip.key == IN_LIBRARY)
}

/// What the catalogue is asked for `query`: its words, and the artist a chip
/// narrows to, which steers its ranking. Its answer is narrowed by the chips
/// again here.
fn catalogue_text(query: &QuerySpec) -> String {
    let mut text = query.text.trim().to_owned();
    for chip in &query.chips {
        if chip.key == ARTIST
            && let Some(artist) = &chip.value
        {
            text.push(' ');
            text.push_str(artist);
        }
    }
    text
}

/// The connected streaming catalogue as a query view's remote source: its
/// quick suggestions as the first page, its full search as the second.
pub struct CatalogueSource {
    make: fn() -> Box<dyn MusicBackend>,
    backend: Mutex<Option<Arc<dyn MusicBackend>>>,
    label: Mutex<String>,
}

impl CatalogueSource {
    /// A source over the backend `make` builds, the first time it is warmed
    /// or asked (never on the UI thread).
    pub fn new(make: fn() -> Box<dyn MusicBackend>) -> Self {
        Self {
            make,
            backend: Mutex::new(None),
            label: Mutex::new("YouTube Music".to_owned()),
        }
    }

    /// The person switched catalogues: the next request builds the backend
    /// again, which follows the choice.
    pub fn source_changed(&self) {
        *self.backend.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn backend(&self) -> Arc<dyn MusicBackend> {
        let mut held = self.backend.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(backend) = held.as_ref() {
            return Arc::clone(backend);
        }
        let backend: Arc<dyn MusicBackend> = Arc::from((self.make)());
        backend
            .catalogue_label()
            .clone_into(&mut self.label.lock().unwrap_or_else(PoisonError::into_inner));
        *held = Some(Arc::clone(&backend));
        backend
    }
}

impl RemoteSource for CatalogueSource {
    fn id(&self) -> &'static str {
        "catalogue"
    }

    fn label(&self) -> String {
        self.label
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn accepts(&self, query: &QuerySpec) -> bool {
        catalogue_accepts(query)
    }

    fn warm(&self) {
        self.backend().warm();
    }

    fn search(
        &self,
        query: &QuerySpec,
        cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError> {
        let backend = self.backend();
        let text = catalogue_text(query);
        let narrowed = |bundle: &SearchBundle| -> Vec<Item> {
            bundle_items(bundle)
                .into_iter()
                .filter(|item| query.admits(item))
                .collect()
        };
        // The quick answer only leads; when it fails the full one still comes.
        match backend.search_quick(&text) {
            Ok(bundle) if !bundle.is_empty() => page(narrowed(&bundle)),
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "music: catalogue suggestions failed"),
        }
        if cancel.is_cancelled() {
            return Ok(());
        }
        let bundle = backend
            .search_all(&text)
            .map_err(|error| RemoteError(unreachable_text(&self.label(), &error.0)))?;
        page(narrowed(&bundle));
        Ok(())
    }
}

/// What a remote section says when its catalogue could not answer: plainly
/// that it could not be reached, the cause left to the log.
fn unreachable_text(label: &str, cause: &str) -> String {
    tracing::info!(target: "music::search", %cause, "catalogue search failed");
    format!("{label} can\u{2019}t be reached right now.")
}

/// A stand-in catalogue for captures and tests: it answers from a fixed set
/// of songs, albums, artists and playlists that are not in the sample
/// library, so a capture of the remote section is the same on every machine.
pub struct SampleCatalogue;

impl SampleCatalogue {
    fn bundle() -> SearchBundle {
        let track = |id: &str, title: &str, artist: &str, album: &str, seconds: u32| TrackHit {
            video_id: id.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            artist_id: None,
            album: Some(album.to_owned()),
            album_id: None,
            duration_s: seconds,
            cover_url: String::new(),
        };
        SearchBundle {
            top: None,
            tracks: vec![
                track(
                    "smpl-north",
                    "Northern Light",
                    "Aurora Skies",
                    "Northern Light",
                    231,
                ),
                track(
                    "smpl-house",
                    "Lighthouse Keeper",
                    "Neon Harbor",
                    "Harbor Lights (Deluxe)",
                    204,
                ),
                track(
                    "smpl-years",
                    "Light Years Away",
                    "Lumen Theory",
                    "Light Years",
                    262,
                ),
                track(
                    "smpl-candle",
                    "Candlelight Waltz",
                    "Sable Quartet",
                    "Candlelight",
                    189,
                ),
            ],
            albums: vec![
                AlbumHit {
                    id: "smpl-al-years".into(),
                    title: "Light Years".into(),
                    artist: "Lumen Theory".into(),
                    artist_id: None,
                    year: 2025,
                    kind: crate::backend::AlbumKind::Album,
                    cover_url: String::new(),
                },
                AlbumHit {
                    id: "smpl-al-candle".into(),
                    title: "Candlelight".into(),
                    artist: "Sable Quartet".into(),
                    artist_id: None,
                    year: 2024,
                    kind: crate::backend::AlbumKind::Ep,
                    cover_url: String::new(),
                },
            ],
            artists: vec![ArtistHit {
                id: "smpl-ar-keeper".into(),
                name: "The Lightkeepers".into(),
                subscriber_count: 0,
                avatar_url: String::new(),
            }],
            playlists: vec![PlaylistHit {
                id: "smpl-pl-late".into(),
                title: "Late Night Light".into(),
                subtitle: "Playlist".into(),
                track_count: 42,
                cover_url: String::new(),
            }],
        }
    }
}

impl RemoteSource for SampleCatalogue {
    fn id(&self) -> &'static str {
        "catalogue"
    }

    fn label(&self) -> String {
        "YouTube Music".to_owned()
    }

    fn accepts(&self, query: &QuerySpec) -> bool {
        catalogue_accepts(query)
    }

    fn search(
        &self,
        query: &QuerySpec,
        _cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError> {
        let kinds = kinds();
        let found: Vec<Item> = bundle_items(&Self::bundle())
            .into_iter()
            .filter(|item| {
                query.admits(item)
                    && query.text_admits(item, kinds.iter().find(|kind| kind.kind == item.kind))
            })
            .collect();
        page(found);
        Ok(())
    }
}
