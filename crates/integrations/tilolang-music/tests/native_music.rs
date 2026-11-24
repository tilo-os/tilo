//! Music's capabilities through its own actions and views: the songs a person plays, queues and
//! likes, the catalogue's answers written where the library reads them, the kept song that plays
//! with no network, and the live card the shell shows with the playing resource's own actions.
//!
//! The catalogue is a script (the service in the app's own terms), so the tests run no network
//! and make no claim about YouTube Music: they claim that an answer becomes the library's songs,
//! that every control is an action on the resource, and that what plays is one workspace resource
//! of the shared playback package.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::doc_markdown,
    reason = "test assertions; YouTube Music is a name, not code"
)]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tilolang_music::{
    AlbumItem, AlbumPage, ArtistItem, ArtistPage, HomePage, HomeSection, Item, LyricLine, Lyrics,
    MusicOrigin, PlaylistItem, Scripted, SearchPage, Service, SpotifyDevice, SpotifyPage,
    SpotifyReply, TrackItem,
};
use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ids::MemberId};

const APP: &str = "so.tilo.music";
const PLAYBACK: &str = "so.tilo.playback";

/// The two packages Music runs over: the shared player, which Music's views and actions call,
/// then Music itself.
fn packages() -> [(&'static str, &'static str); 2] {
    [
        (
            include_str!("../../../../apps/tl/playback/app.tl"),
            include_str!("../../../../apps/tl/playback/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/music/app.tl"),
            include_str!("../../../../apps/tl/music/app.ids"),
        ),
    ]
}

/// Ada's workspace with the packages, Music's root made.
fn workspace() -> Host {
    let host = Host::open(
        ResourceStore::in_memory(),
        Actor::Member(MemberId::person("Ada")),
        None,
        || None,
    )
    .unwrap();
    for (source, ids) in packages() {
        host.load_builtin(source, ids).unwrap();
    }
    host.root_of(APP).unwrap();
    host
}

fn ada() -> Actor {
    Actor::from(MemberId::person("Ada"))
}

/// The id of the field `field` of the type `ty`, whichever loaded package declares it: the
/// session's own type lives in the playback package, the library's in Music's.
fn field_id(host: &Host, ty: &str, field: &str) -> String {
    let packages = host.packages();
    packages
        .iter()
        .find_map(|package| package.meta.types.iter().find(|t| t.name == ty))
        .and_then(|t| t.fields.iter().find(|f| f.name == field))
        .unwrap_or_else(|| panic!("{ty}.{field}"))
        .id
        .clone()
}

/// The playing resource the root holds.
fn playing(host: &Host) -> ResourceRef {
    let root = host.root_of(APP).unwrap();
    let field = field_id(host, "Library", "playing");
    match host.runtime().read(&root, &field).unwrap() {
        Value::Opt(Some(value)) => match *value {
            Value::Ref(reference) => (*reference.target).clone(),
            other => panic!("the root's playing is not a resource: {other:?}"),
        },
        other => panic!("the root has no session yet: {other:?}"),
    }
}

/// A field of the playing resource, as the actions left it.
fn played(host: &Host, field: &str) -> Value {
    let at = playing(host);
    let id = field_id(host, "Playback", field);
    host.runtime().read(&at, &id).unwrap()
}

fn action_id(host: &Host, name: &str) -> String {
    let packages = host.packages();
    packages
        .iter()
        .find_map(|package| package.meta.actions.iter().find(|a| a.name == name))
        .unwrap_or_else(|| panic!("the action {name}"))
        .id
        .clone()
}

fn invoke(host: &Host, action: &str, args: Vec<Value>) {
    let id = action_id(host, action);
    host.runtime()
        .invoke_action(&ada(), (id.as_str(), args))
        .unwrap_or_else(|error| panic!("`{action}`: {error}"));
}

/// Fills the library with the catalogue's two songs, as its answer writes them, and answers the
/// song's key.
fn song(host: &Host, key: &str, _title: &str, _artist: &str) -> String {
    let mut origin = MusicOrigin::new(
        Arc::new(scripted()) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    );
    deliver(
        host,
        &mut origin,
        "music.search",
        &serde_json::json!("first light"),
    );
    key.to_owned()
}

/// The resource of the song called `key`, as the origin wrote it into the library.
fn song_at(host: &Host, key: &str) -> ResourceRef {
    let root = host.root_of(APP).unwrap();
    let tracks = host
        .runtime()
        .read(&root, &field_id(host, "Library", "tracks"))
        .unwrap();
    match tracks {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .find(|at| {
                matches!(
                    host.runtime().read(at, &field_id(host, "Track", "key")),
                    Ok(Value::Text(text)) if text == key
                )
            })
            .unwrap_or_else(|| panic!("the song {key} is in the library")),
        other => panic!("the songs are a list: {other:?}"),
    }
}

/// The resource of the album called `key`, as the origin wrote it into the library.
fn album_at(host: &Host, key: &str) -> ResourceRef {
    let root = host.root_of(APP).unwrap();
    let albums = host
        .runtime()
        .read(&root, &field_id(host, "Library", "albums"))
        .unwrap();
    match albums {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .find(|at| {
                matches!(
                    host.runtime().read(at, &field_id(host, "Album", "key")),
                    Ok(Value::Text(text)) if text == key
                )
            })
            .unwrap_or_else(|| panic!("the album {key} is in the library")),
        other => panic!("the albums are a list: {other:?}"),
    }
}

/// Whether the playing resource's state is the variant `case`, by the name its package gives it.
fn state_is(host: &Host, case: &str) -> bool {
    let Value::Enum(state) = played(host, "state") else {
        return false;
    };
    host.packages()
        .iter()
        .flat_map(|package| package.meta.enums.iter())
        .find(|enumeration| enumeration.id == state.ty)
        .and_then(|enumeration| {
            enumeration
                .variants
                .iter()
                .find(|variant| variant.id == state.variant)
        })
        .is_some_and(|variant| variant.name == case)
}

fn track(key: &str, title: &str, artist: &str, seconds: i64) -> TrackItem {
    TrackItem {
        key: key.to_owned(),
        title: title.to_owned(),
        artist: artist.to_owned(),
        artist_key: "UCaurora".to_owned(),
        album: "Northern Lights".to_owned(),
        album_key: "MPREnorthern".to_owned(),
        seconds,
        plays: 1_800_000,
        cover: String::new(),
    }
}

fn scripted() -> Scripted {
    let mut scripted = Scripted::default();
    scripted.searches.insert(
        "first light".to_owned(),
        SearchPage {
            query: "first light".to_owned(),
            top: Some(Item::Track(track(
                "vid1",
                "First Light",
                "Aurora Skies",
                214,
            ))),
            tracks: vec![
                track("vid1", "First Light", "Aurora Skies", 214),
                track("vid2", "Second Wind", "Aurora Skies", 198),
            ],
            albums: vec![AlbumItem {
                key: "MPREnorthern".to_owned(),
                title: "Northern Lights".to_owned(),
                artist: "Aurora Skies".to_owned(),
                artist_key: "UCaurora".to_owned(),
                year: 2026,
                kind: "album".to_owned(),
                cover: String::new(),
            }],
            artists: vec![ArtistItem {
                key: "UCaurora".to_owned(),
                name: "Aurora Skies".to_owned(),
                subscribers: 4_200_000,
                avatar: String::new(),
                genres: Vec::new(),
            }],
            playlists: Vec::new(),
        },
    );
    scripted.home = HomePage {
        sections: vec![HomeSection {
            title: "Top artists".to_owned(),
            items: vec![Item::Artist(ArtistItem {
                key: "UCaurora".to_owned(),
                name: "Aurora Skies".to_owned(),
                subscribers: 4_200_000,
                avatar: String::new(),
                genres: Vec::new(),
            })],
        }],
    };
    scripted.albums.insert(
        "MPREnorthern".to_owned(),
        AlbumPage {
            album: AlbumItem {
                key: "MPREnorthern".to_owned(),
                title: "Northern Lights".to_owned(),
                artist: "Aurora Skies".to_owned(),
                artist_key: "UCaurora".to_owned(),
                year: 2026,
                kind: "album".to_owned(),
                cover: String::new(),
            },
            tracks: vec![track("vid1", "First Light", "Aurora Skies", 214)],
        },
    );
    scripted.artists.insert(
        "UCaurora".to_owned(),
        ArtistPage {
            artist: ArtistItem {
                key: "UCaurora".to_owned(),
                name: "Aurora Skies".to_owned(),
                subscribers: 4_200_000,
                avatar: String::new(),
                genres: Vec::new(),
            },
            tracks: vec![track("vid1", "First Light", "Aurora Skies", 214)],
            ..ArtistPage::default()
        },
    );
    scripted.streams.insert(
        "vid1".to_owned(),
        "https://media.example.test/first-light.webm".to_owned(),
    );
    scripted.lyrics.insert(
        "First Light".to_owned(),
        Lyrics {
            synced: true,
            credit: "lrclib".to_owned(),
            lines: vec![LyricLine {
                at_ms: 8_500,
                text: "Held to the window, a paper sun".to_owned(),
            }],
        },
    );
    scripted.pinned.insert(
        "https://media.example.test/first-light.webm".to_owned(),
        "tilo-blob://kept".to_owned(),
    );
    scripted
}

/// Deliver a capability to the origin and let it write: what the outbox does for an effect.
fn deliver(host: &Host, origin: &mut MusicOrigin, capability: &str, payload: &serde_json::Value) {
    let delivery = origin.deliver(capability, payload);
    assert_eq!(
        delivery,
        tilolang_tilo::runtime::Delivery::Pending,
        "`{capability}` is the music origin's to carry out"
    );
    poll(host, origin);
}

fn poll(host: &Host, origin: &mut MusicOrigin) {
    let cx = OriginCx {
        host,
        app: APP,
        waker: &(Arc::new(|| {}) as tilolang_tilo::background::Waker),
        now: "2026-11-23T10:00:00Z".to_owned(),
    };
    let run = origin.poll(&cx);
    assert!(run.wrote || run.settle, "the answer is written");
}

#[test]
fn playing_a_song_writes_what_plays_and_where_it_stands() {
    let host = workspace();
    let key = song(&host, "vid1", "First Light", "Aurora Skies");
    let song_at = song_at(&host, &key);
    invoke(&host, "play_song", vec![Value::resource(&song_at)]);
    assert_eq!(
        played(&host, "track"),
        Value::text(song_at.id_string()),
        "the session names the song that plays"
    );
    assert_eq!(played(&host, "title"), Value::text("First Light"));
    assert!(state_is(&host, "playing"), "the song plays");
    invoke(&host, "toggle", vec![Value::resource(&playing(&host))]);
    assert!(state_is(&host, "paused"), "and stops where it stood");
}

#[test]
fn the_queue_is_the_playing_resource_and_next_walks_it() {
    let host = workspace();
    song(&host, "vid1", "First Light", "Aurora Skies");
    let album = album_at(&host, "MPREnorthern");
    invoke(&host, "play_album", vec![Value::resource(&album)]);
    assert_eq!(played(&host, "title"), Value::text("First Light"));
    invoke(&host, "next", vec![Value::resource(&playing(&host))]);
    assert_eq!(played(&host, "title"), Value::text("Second Wind"));
    assert_eq!(played(&host, "queue_at"), Value::Int(1));
    invoke(&host, "previous", vec![Value::resource(&playing(&host))]);
    assert_eq!(played(&host, "title"), Value::text("First Light"));
}

#[test]
fn liking_and_keeping_are_the_persons_fields() {
    let host = workspace();
    let key = song(&host, "vid1", "First Light", "Aurora Skies");
    let song_at = song_at(&host, &key);
    invoke(&host, "like", vec![Value::resource(&song_at)]);
    assert_eq!(
        host.runtime()
            .read(&song_at, &field_id(&host, "Track", "liked"))
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn the_catalogue_answer_becomes_the_library() {
    let host = workspace();
    let mut origin = MusicOrigin::new(
        Arc::new(scripted()) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    );
    deliver(
        &host,
        &mut origin,
        "music.search",
        &serde_json::json!("first light"),
    );
    let root = host.root_of(APP).unwrap();
    let tracks = host
        .runtime()
        .read(&root, &field_id(&host, "Library", "tracks"))
        .unwrap();
    match tracks {
        Value::List(items) => assert_eq!(items.len(), 2, "the songs the service found"),
        other => panic!("the songs are a list: {other:?}"),
    }
    assert_eq!(
        host.runtime()
            .read(&root, &field_id(&host, "Library", "search_state"))
            .unwrap(),
        Value::text("done")
    );
}

#[test]
fn lyrics_and_kept_bytes_are_written_into_the_song() {
    let host = workspace();
    let mut origin = MusicOrigin::new(
        Arc::new(scripted()) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    );
    deliver(
        &host,
        &mut origin,
        "music.search",
        &serde_json::json!("first light"),
    );
    deliver(
        &host,
        &mut origin,
        "music.stream",
        &serde_json::json!("vid1"),
    );
    deliver(
        &host,
        &mut origin,
        "music.lyrics",
        &serde_json::json!(["First Light", "Aurora Skies"]),
    );
    deliver(
        &host,
        &mut origin,
        "music.pin",
        &serde_json::json!(["https://media.example.test/first-light.webm", "First Light"]),
    );
    let song_at = song_at(&host, "vid1");
    assert_eq!(
        host.runtime()
            .read(&song_at, &field_id(&host, "Track", "offline"))
            .unwrap(),
        Value::text("tilo-blob://kept"),
        "the kept bytes' address is the song's to play"
    );
    match host
        .runtime()
        .read(&song_at, &field_id(&host, "Track", "lyrics"))
        .unwrap()
    {
        Value::Text(lyrics) => assert!(lyrics.contains("paper sun"), "the words are the song's"),
        other => panic!("lyrics are text: {other:?}"),
    }
}

#[test]
fn the_live_view_shows_the_song_and_each_control_is_an_action() {
    let host = workspace();
    let key = song(&host, "vid1", "First Light", "Aurora Skies");
    invoke(
        &host,
        "play_song",
        vec![Value::resource(&song_at(&host, &key))],
    );
    assert!(
        host.package(PLAYBACK)
            .and_then(|package| package.view_id("live"))
            .is_some(),
        "the playing type's live view is declared"
    );
    let view = host
        .open_resource(&playing(&host), Some("live"))
        .expect("the live card renders on the playing resource");
    let shown = format!("{:?}", view.tree());
    for control in [
        "Pause",
        "Next",
        "Previous",
        "Back 15 seconds",
        "Forward 15 seconds",
    ] {
        assert!(shown.contains(control), "the live card offers `{control}`");
    }
    assert!(
        shown.contains("toggle"),
        "its play control is the playing resource's own action"
    );
}

#[test]
fn the_catalogue_switch_asks_spotify_through_the_music_origin() {
    let host = workspace();
    let origin = MusicOrigin::new(
        Arc::new(scripted()) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(
        origin.serves("music.spotify"),
        "Spotify is the music origin's to ask"
    );
    invoke(
        &host,
        "set_catalogue",
        vec![
            Value::resource(&host.root_of(APP).unwrap()),
            Value::text("spotify"),
        ],
    );
}

/// A field of the library, as text.
fn library_text(host: &Host, field: &str) -> String {
    let root = host.root_of(APP).unwrap();
    match host
        .runtime()
        .read(&root, &field_id(host, "Library", field))
        .unwrap()
    {
        Value::Text(text) => text,
        other => panic!("Library.{field} is text: {other:?}"),
    }
}

/// The sign-in, the devices and a move between them, through the origin: what the person's
/// Spotify is, written where the library reads it.
#[test]
fn spotify_sign_in_devices_and_transfer_reach_the_library() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.spotify.insert(
        "connect".to_owned(),
        SpotifyReply {
            url: "https://accounts.spotify.com/authorize?client_id=tilo".to_owned(),
            ..SpotifyReply::default()
        },
    );
    scripted.spotify.insert(
        "finish".to_owned(),
        SpotifyReply {
            account: "Ada".to_owned(),
            product: "premium".to_owned(),
            ..SpotifyReply::default()
        },
    );
    scripted.spotify.insert(
        "devices".to_owned(),
        SpotifyReply {
            devices: vec![
                SpotifyDevice {
                    key: "kitchen".to_owned(),
                    name: "Kitchen".to_owned(),
                    kind: "speaker".to_owned(),
                    active: false,
                    volume: 40,
                },
                SpotifyDevice {
                    key: "laptop".to_owned(),
                    name: "Laptop".to_owned(),
                    kind: "computer".to_owned(),
                    active: true,
                    volume: 80,
                },
            ],
            active: "laptop".to_owned(),
            now: "First Light — Aurora Skies".to_owned(),
            playing: true.into(),
            ..SpotifyReply::default()
        },
    );
    let mut origin = MusicOrigin::new(
        Arc::new(scripted) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    );
    let step = |origin: &mut MusicOrigin, op: &str, arg: &str| {
        let payload = serde_json::json!([op, arg]);
        deliver(&host, origin, "music.spotify", &payload);
    };
    step(&mut origin, "connect", "");
    assert!(
        library_text(&host, "connect_url").starts_with("https://accounts.spotify.com/"),
        "the page the person signs in on is the library's"
    );
    assert_eq!(library_text(&host, "spotify_state"), "connecting");
    step(&mut origin, "finish", "");
    assert_eq!(library_text(&host, "spotify_state"), "connected");
    assert_eq!(library_text(&host, "spotify_account"), "Ada");
    assert_eq!(library_text(&host, "spotify_product"), "premium");
    step(&mut origin, "devices", "");
    let root = host.root_of(APP).unwrap();
    let Ok(Value::List(devices)) = host
        .runtime()
        .read(&root, &field_id(&host, "Library", "devices"))
    else {
        panic!("the library holds its devices");
    };
    assert_eq!(devices.len(), 2, "both places Spotify plays on");
    assert_eq!(library_text(&host, "spotify_active"), "laptop");
    assert_eq!(
        library_text(&host, "spotify_now"),
        "First Light — Aurora Skies"
    );
}

/// A song as Spotify's catalogue names one.
fn spotify_track(id: &str, title: &str) -> TrackItem {
    TrackItem {
        key: format!("spotify:track:{id}"),
        title: title.to_owned(),
        artist: "Ava".to_owned(),
        artist_key: "spotify:artist:artist-1".to_owned(),
        album: "Morning".to_owned(),
        album_key: "spotify:album:album-1".to_owned(),
        seconds: 214,
        plays: 0,
        cover: String::new(),
    }
}

/// The library's song, album and artist as a Spotify search wrote them, with the artist's
/// genres.
fn spotify_search() -> SearchPage {
    SearchPage {
        query: "ava".to_owned(),
        top: None,
        tracks: vec![spotify_track("track-1", "First Light")],
        albums: vec![AlbumItem {
            key: "spotify:album:album-1".to_owned(),
            title: "Morning".to_owned(),
            artist: "Ava".to_owned(),
            artist_key: "spotify:artist:artist-1".to_owned(),
            year: 2025,
            kind: "album".to_owned(),
            cover: String::new(),
        }],
        artists: vec![ArtistItem {
            key: "spotify:artist:artist-1".to_owned(),
            name: "Ava".to_owned(),
            subscribers: 120_000,
            avatar: String::new(),
            genres: vec!["dream pop".to_owned(), "art pop".to_owned()],
        }],
        playlists: Vec::new(),
    }
}

/// A field of `at`, as the origin left it.
fn field(host: &Host, at: &ResourceRef, ty: &str, name: &str) -> Value {
    host.runtime().read(at, &field_id(host, ty, name)).unwrap()
}

/// The resource of the artist called `key`.
fn artist_at(host: &Host, key: &str) -> ResourceRef {
    let root = host.root_of(APP).unwrap();
    let artists = host
        .runtime()
        .read(&root, &field_id(host, "Library", "artists"))
        .unwrap();
    match artists {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .find(|at| field(host, at, "Artist", "key") == Value::text(key))
            .unwrap_or_else(|| panic!("the artist {key} is in the library")),
        other => panic!("the artists are a list: {other:?}"),
    }
}

/// What the library holds in the collection `field` of `ty`.
fn held(host: &Host, ty: &str, field_name: &str) -> Vec<ResourceRef> {
    let root = host.root_of(APP).unwrap();
    match host
        .runtime()
        .read(&root, &field_id(host, "Library", field_name))
        .unwrap()
    {
        Value::List(items) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::Ref(reference) => Some((*reference.target).clone()),
                _ => None,
            })
            .collect(),
        other => panic!("{ty}.{field_name} is a list: {other:?}"),
    }
}

/// The Spotify origin over `scripted`, ready to answer.
fn spotify_origin(scripted: Scripted) -> MusicOrigin {
    MusicOrigin::new(
        Arc::new(scripted) as Arc<dyn Service>,
        Arc::new(AtomicBool::new(false)),
    )
}

#[test]
fn a_spotify_library_write_reads_back_into_the_song_the_album_and_the_artist() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.searches.insert("ava".to_owned(), spotify_search());
    scripted.spotify.insert(
        "like".to_owned(),
        SpotifyReply {
            liked: true.into(),
            ..SpotifyReply::default()
        },
    );
    scripted.spotify.insert(
        "save_album".to_owned(),
        SpotifyReply {
            saved: true.into(),
            ..SpotifyReply::default()
        },
    );
    scripted.spotify.insert(
        "follow".to_owned(),
        SpotifyReply {
            followed: true.into(),
            ..SpotifyReply::default()
        },
    );
    let mut origin = spotify_origin(scripted);
    deliver(
        &host,
        &mut origin,
        "music.search",
        &serde_json::json!("ava"),
    );
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["like", "spotify:track:track-1", "true"]),
    );
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["save_album", "spotify:album:album-1", "true"]),
    );
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["follow", "spotify:artist:artist-1", "true"]),
    );
    let song = song_at(&host, "spotify:track:track-1");
    assert_eq!(field(&host, &song, "Track", "liked"), Value::Bool(true));
    let album = album_at(&host, "spotify:album:album-1");
    assert_eq!(
        field(&host, &album, "Album", "in_library"),
        Value::Bool(true)
    );
    let artist = artist_at(&host, "spotify:artist:artist-1");
    assert_eq!(
        field(&host, &artist, "Artist", "followed"),
        Value::Bool(true)
    );
    assert_eq!(
        field(&host, &artist, "Artist", "genres"),
        Value::List(vec![Value::text("dream pop"), Value::text("art pop")]),
        "the catalogue's genres are the artist's, for the app to suggest by"
    );
}

#[test]
fn spotifys_own_queue_is_a_snapshot_the_library_shows_in_its_order() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.searches.insert("ava".to_owned(), spotify_search());
    scripted.spotify.insert(
        "queue".to_owned(),
        SpotifyReply {
            now: "First Light — Ava".to_owned(),
            queue: vec![
                spotify_track("track-1", "First Light"),
                spotify_track("track-2", "Blue Hour"),
            ],
            ..SpotifyReply::default()
        },
    );
    scripted.spotify.insert(
        "add_to_queue".to_owned(),
        SpotifyReply {
            queue: vec![spotify_track("track-2", "Blue Hour")],
            ..SpotifyReply::default()
        },
    );
    let mut origin = spotify_origin(scripted);
    deliver(
        &host,
        &mut origin,
        "music.search",
        &serde_json::json!("ava"),
    );
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["queue", ""]),
    );
    let songs = held(&host, "SpotifyQueueItem", "spotify_queue");
    assert_eq!(songs.len(), 2, "the queue Spotify plays next");
    assert_eq!(
        field(&host, &songs[0], "SpotifyQueueItem", "title"),
        Value::text("First Light")
    );
    // A second read is the queue as it is now: what left it leaves the library's snapshot.
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["add_to_queue", "spotify:track:track-2"]),
    );
    let songs = held(&host, "SpotifyQueueItem", "spotify_queue");
    assert_eq!(songs.len(), 1, "the snapshot holds what plays next now");
    assert_eq!(
        field(&host, &songs[0], "SpotifyQueueItem", "key"),
        Value::text("spotify:track:track-2")
    );
}

#[test]
fn a_page_of_what_the_person_keeps_adds_its_section_and_the_one_after_it() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.spotify.insert(
        "saved_page".to_owned(),
        SpotifyReply {
            pages: vec![SpotifyPage {
                kind: "tracks".to_owned(),
                section: HomeSection {
                    title: "Liked songs".to_owned(),
                    items: vec![Item::Track(spotify_track("track-51", "Fifty One"))],
                },
                next: 100,
            }],
            ..SpotifyReply::default()
        },
    );
    let mut origin = spotify_origin(scripted);
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["saved_page", "tracks", "50"]),
    );
    assert_eq!(
        field(
            &host,
            &host.root_of(APP).unwrap(),
            "Library",
            "spotify_next_tracks"
        ),
        Value::Int(100),
        "the app asks for the page after this one without counting for itself"
    );
    let rows = held(&host, "Suggestion", "suggestions");
    assert!(
        rows.iter().any(|row| {
            field(&host, row, "Suggestion", "title") == Value::text("Fifty One")
                && field(&host, row, "Suggestion", "section") == Value::text("Liked songs")
        }),
        "the page is a section of the home page"
    );
}

#[test]
fn a_playlist_spotify_made_is_the_playlist_the_person_then_edits() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.spotify.insert(
        "create_playlist".to_owned(),
        SpotifyReply {
            made: Some(PlaylistItem {
                key: "spotify:playlist:pl-1".to_owned(),
                title: "Roads".to_owned(),
                subtitle: "Ada".to_owned(),
                track_count: 0,
                cover: String::new(),
                description: String::new(),
            }),
            ..SpotifyReply::default()
        },
    );
    let mut origin = spotify_origin(scripted);
    let root = host.root_of(APP).unwrap();
    invoke(
        &host,
        "new_mixtape",
        vec![Value::resource(&root), Value::text("Roads")],
    );
    let mixtapes = held(&host, "Mixtape", "mixtapes");
    let mixtape = mixtapes
        .first()
        .expect("the playlist the person made")
        .clone();
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["create_playlist", "Roads", mixtape.id_string()]),
    );
    assert_eq!(
        field(&host, &mixtape, "Mixtape", "spotify"),
        Value::text("spotify:playlist:pl-1"),
        "the playlist the person made is Spotify's, and their edits go there"
    );
}

#[test]
fn a_sign_in_spotify_ended_is_a_state_the_app_shows_a_connect_button_for() {
    let host = workspace();
    let mut scripted = scripted();
    scripted.spotify.insert(
        "status".to_owned(),
        SpotifyReply {
            expired: true.into(),
            ..SpotifyReply::default()
        },
    );
    let mut origin = spotify_origin(scripted);
    deliver(
        &host,
        &mut origin,
        "music.spotify",
        &serde_json::json!(["status", ""]),
    );
    assert_eq!(library_text(&host, "spotify_state"), "expired");
    assert_eq!(
        library_text(&host, "spotify_trouble"),
        "Spotify ended the sign-in"
    );
}
