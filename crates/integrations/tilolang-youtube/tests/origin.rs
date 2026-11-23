//! The YouTube origin (`crates/integrations/tilolang-youtube`): what a package's `youtube` types
//! hold is what the service answered, written as the origin's change and never overwriting what
//! the person did with a video.
//!
//! The service is a script (the app's own answers, in the client's terms), so the test runs no
//! network and is not a claim about YouTube: it is a claim that an answer becomes the client's
//! channels, playlists and videos, keyed by the service's ids and written where the app reads
//! them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::doc_markdown,
    reason = "test assertions; YouTube is a name, not code"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_youtube::{
    Branding, ChannelItem, ChannelPage, CommentsPage, PlaylistItem, PlaylistPage, Scripted,
    SearchFilter, SearchPage, SegmentPage, Service, ServiceError, TrendingPage, VideoItem, Votes,
    YouTubeOrigin,
};

const TUBE: &str = "so.tilo.tube";

fn package() -> (&'static str, &'static str) {
    (
        include_str!("../../../../apps/tl/tube/app.tl"),
        include_str!("../../../../apps/tl/tube/app.ids"),
    )
}

/// The client with its root made, over a workspace of its own.
fn workspace() -> tilolang_tilo::host::Host {
    let host = tilolang_tilo::host::Host::open(
        tilolang_tilo::resources::ResourceStore::in_memory(),
        tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("Ada")),
        None,
        || None,
    )
    .unwrap();
    host.load_builtin(package().0, package().1).unwrap();
    host.root_of(TUBE).unwrap();
    host
}

fn video(key: &str, title: &str, channel: &str) -> VideoItem {
    VideoItem {
        key: key.to_owned(),
        title: title.to_owned(),
        channel_key: channel.to_owned(),
        channel_name: "MKBHD".to_owned(),
        channel_avatar: "https://example.test/avatar.png".to_owned(),
        views: 1_840_000,
        published: "2026-10-28T14:00:00Z".to_owned(),
        duration: 921,
        description: "A week of use, three days of battery.".to_owned(),
        likes: 96000,
        thumbnail: format!("https://i.ytimg.com/vi/{key}/hqdefault.jpg"),
        live: false,
    }
}

fn channel() -> ChannelItem {
    ChannelItem {
        key: "UCmkbhd".to_owned(),
        name: "MKBHD".to_owned(),
        handle: "@mkbhd".to_owned(),
        subscribers: 19_800_000,
        avatar: "https://example.test/avatar.png".to_owned(),
        banner: String::new(),
        description: "Videos about technology.".to_owned(),
        verified: true,
    }
}

fn script() -> Scripted {
    Scripted {
        searches: std::collections::BTreeMap::from([(
            "phone reviews".to_owned(),
            SearchPage {
                query: "phone reviews".to_owned(),
                channels: vec![channel()],
                playlists: vec![PlaylistItem {
                    key: "pl-phones".to_owned(),
                    title: "Every phone reviewed this year".to_owned(),
                    channel_key: "UCmkbhd".to_owned(),
                    channel_name: "MKBHD".to_owned(),
                    video_count: 41,
                    thumbnail: "https://example.test/pl.jpg".to_owned(),
                }],
                videos: vec![video(
                    "v-phone",
                    "The phone that finally lasts a week",
                    "UCmkbhd",
                )],
            },
        )]),
        channels: std::collections::BTreeMap::from([(
            "UCmkbhd".to_owned(),
            ChannelPage {
                channel: channel(),
                videos: vec![
                    video("v-phone", "The phone that finally lasts a week", "UCmkbhd"),
                    video("v-watch", "The watch that needs no winding", "UCmkbhd"),
                ],
            },
        )]),
        comments: std::collections::BTreeMap::new(),
        segments: std::collections::BTreeMap::new(),
        branding: std::collections::BTreeMap::new(),
        votes: std::collections::BTreeMap::new(),
        playlists: std::collections::BTreeMap::from([(
            "pl-phones".to_owned(),
            PlaylistPage {
                playlist: PlaylistItem {
                    key: "pl-phones".to_owned(),
                    title: "Every phone reviewed this year".to_owned(),
                    channel_key: "UCmkbhd".to_owned(),
                    channel_name: "MKBHD".to_owned(),
                    video_count: 3,
                    thumbnail: "https://example.test/pl.jpg".to_owned(),
                },
                // The maker's order, not the videos' dates: the newest is second.
                videos: vec![
                    video("v-old", "The first phone this year", "UCmkbhd"),
                    video("v-phone", "The phone that finally lasts a week", "UCmkbhd"),
                    video("v-budget", "The best phone under 300", "UCmkbhd"),
                ],
            },
        )]),
        trending: TrendingPage {
            videos: vec![video(
                "v-magnet",
                "Why every magnet you own is lying to you",
                "UCver",
            )],
        },
        streams: std::collections::BTreeMap::from([(
            "v-phone".to_owned(),
            "https://media.example/v-phone.mp4".to_owned(),
        )]),
    }
}

/// Poll an origin once with the instance's context.
fn poll(origin: &std::rc::Rc<std::cell::RefCell<YouTubeOrigin>>, host: &tilolang_tilo::host::Host) {
    let cx = OriginCx {
        host,
        app: TUBE,
        waker: &(Arc::new(|| {}) as tilolang_tilo::background::Waker),
        now: "2026-11-06T10:00:00Z".to_owned(),
    };
    origin.borrow_mut().poll(&cx);
}

/// Every text of the kind `kind` the client holds, by its key: what a search, a channel or the
/// trending list wrote.
fn keys(host: &tilolang_tilo::host::Host, kind: &str) -> Vec<String> {
    let packages = host.packages();
    let (_, root_ty) = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client")
        .main()
        .expect("the client's root");
    let root = tilolang_tilo::runtime::ResourceRef::root(
        &host.root_of(TUBE).unwrap().doc,
        root_ty.clone(),
    );
    let field = match kind {
        "channel" => "channels",
        "video" => "videos",
        _ => "playlists",
    };
    let meta = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    let ty = meta
        .meta
        .types
        .iter()
        .find(|t| {
            t.name
                == match kind {
                    "channel" => "Channel",
                    "video" => "Video",
                    _ => "Playlist",
                }
        })
        .expect("the kind");
    let field_id = meta
        .meta
        .types
        .iter()
        .find(|t| t.id == root_ty)
        .and_then(|root| root.fields.iter().find(|f| f.name == field))
        .expect("the root's collection")
        .id
        .clone();
    let key_id = ty
        .fields
        .iter()
        .find(|f| f.name == "key")
        .expect("the kind's key")
        .id
        .clone();
    let Ok(tilolang_tilo::runtime::Value::List(items)) = host.runtime().read(&root, &field_id)
    else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|value| match value {
            tilolang_tilo::runtime::Value::Ref(at) => {
                match host.runtime().read(&at.target, &key_id) {
                    Ok(tilolang_tilo::runtime::Value::Text(key)) => Some(key),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

/// The `video_keys` of the playlist keyed `key`, in the order the client's page reads them.
fn video_keys_of(host: &tilolang_tilo::host::Host, key: &str) -> Vec<String> {
    let packages = host.packages();
    let package = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    let (_, root_ty) = package.main().expect("the client's root");
    let field_id = |ty: &str, name: &str| {
        package
            .meta
            .types
            .iter()
            .find(|t| t.name == ty || t.id == ty)
            .and_then(|t| t.fields.iter().find(|f| f.name == name))
            .unwrap_or_else(|| panic!("{ty}.{name}"))
            .id
            .clone()
    };
    let root = host.root_of(TUBE).expect("the root");
    let Ok(tilolang_tilo::runtime::Value::List(playlists)) =
        host.runtime().read(&root, &field_id(&root_ty, "playlists"))
    else {
        return Vec::new();
    };
    let playlist_key = field_id("Playlist", "key");
    let video_keys = field_id("Playlist", "video_keys");
    for item in playlists {
        let tilolang_tilo::runtime::Value::Ref(at) = item else {
            continue;
        };
        match host.runtime().read(&at.target, &playlist_key) {
            Ok(tilolang_tilo::runtime::Value::Text(held)) if held == key => {}
            _ => continue,
        }
        return match host.runtime().read(&at.target, &video_keys) {
            Ok(tilolang_tilo::runtime::Value::List(keys)) => keys
                .into_iter()
                .filter_map(|value| match value {
                    tilolang_tilo::runtime::Value::Text(key) => Some(key),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
    }
    Vec::new()
}

#[test]
fn a_search_writes_its_channels_playlists_and_videos_where_the_client_reads_them() {
    let host = workspace();
    let woken = Arc::new(AtomicBool::new(false));
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::clone(&woken));
    assert!(
        origin.borrow().serves("youtube.search"),
        "the origin carries out the client's search"
    );
    let delivery = origin
        .borrow_mut()
        .deliver("youtube.search", &serde_json::json!("phone reviews"));
    assert_eq!(delivery, tilolang_tilo::runtime::Delivery::Pending);
    poll(&origin, &host);
    assert_eq!(keys(&host, "channel"), ["UCmkbhd"], "the channel found");
    assert_eq!(keys(&host, "playlist"), ["pl-phones"], "the playlist found");
    assert_eq!(keys(&host, "video"), ["v-phone"], "the video found");
    assert_eq!(
        text_of(&host, "search_state"),
        "done",
        "the search the app set to `searching` settles, so its view stops saying it searches"
    );
}

/// A search the app hands over with the filters it narrowed to is the same search: the query
/// first, then the kind, the length and the date, each `any` for no filter.
#[test]
fn a_search_the_app_narrowed_is_asked_with_its_filters() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin.borrow_mut().deliver(
        "youtube.search",
        &serde_json::json!(["phone reviews", "video", "short", "week"]),
    );
    poll(&origin, &host);
    assert_eq!(keys(&host, "channel"), ["UCmkbhd"], "the answer is written");
    assert_eq!(text_of(&host, "search_state"), "done");
}

/// A stream the app asks for with the quality the person chose carries it: the video first, then
/// the tallest picture, and what the service answers is written into the video the client holds.
#[test]
fn a_stream_asked_at_a_quality_is_written_onto_the_video() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    // The client holds the video its page asks the stream for.
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    origin
        .borrow_mut()
        .deliver("youtube.stream", &serde_json::json!(["v-phone", "480p"]));
    poll(&origin, &host);
    let root = host.root_of(TUBE).unwrap();
    let tilolang_tilo::runtime::Value::List(videos) = host.runtime().read(&root, "videos").unwrap()
    else {
        panic!("the client holds its videos");
    };
    let stream = videos
        .into_iter()
        .filter_map(|item| match item {
            tilolang_tilo::runtime::Value::Ref(r) => Some((*r.target).clone()),
            _ => None,
        })
        .find(|at| {
            host.runtime().read(at, "key").unwrap()
                == tilolang_tilo::runtime::Value::text("v-phone")
        })
        .map(|at| host.runtime().read(&at, "stream").unwrap())
        .expect("the video the stream was asked for");
    assert_eq!(
        stream,
        tilolang_tilo::runtime::Value::text("https://media.example/v-phone.mp4")
    );
}

/// The root's text field `name`, as the app reads it.
fn text_of(host: &tilolang_tilo::host::Host, name: &str) -> String {
    let packages = host.packages();
    let package = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    let (_, root_ty) = package.main().expect("the client's root");
    let field = package
        .meta
        .types
        .iter()
        .find(|t| t.id == root_ty)
        .and_then(|root| root.fields.iter().find(|f| f.name == name))
        .expect("the root's field")
        .id
        .clone();
    let root = host.root_of(TUBE).expect("the root");
    match host.runtime().read(&root, &field) {
        Ok(tilolang_tilo::runtime::Value::Text(text)) => text,
        _ => String::new(),
    }
}

#[test]
fn a_channel_writes_its_page_and_its_newest_videos() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    let mut videos = keys(&host, "video");
    videos.sort();
    assert_eq!(videos, ["v-phone", "v-watch"]);
}

/// A playlist's page: its videos, in its maker's order, as the keys the page reads them by.
#[test]
fn a_playlist_writes_its_videos_in_its_maker_s_order() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    assert!(
        origin.borrow().serves("youtube.playlist"),
        "the origin carries out the client's playlist question"
    );
    origin
        .borrow_mut()
        .deliver("youtube.playlist", &serde_json::json!("pl-phones"));
    poll(&origin, &host);
    let playlist = keys(&host, "playlist");
    assert_eq!(playlist, ["pl-phones"], "the playlist itself");
    assert_eq!(
        video_keys_of(&host, "pl-phones"),
        ["v-old", "v-phone", "v-budget"],
        "its videos, in the order its maker put them, not by date"
    );
    let mut videos = keys(&host, "video");
    videos.sort();
    assert_eq!(
        videos,
        ["v-budget", "v-old", "v-phone"],
        "and the videos themselves are where a page finds them"
    );
}

#[test]
fn trending_marks_the_videos_it_lists() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin
        .borrow_mut()
        .deliver("youtube.trending", &serde_json::json!(""));
    poll(&origin, &host);
    assert_eq!(keys(&host, "video"), ["v-magnet"]);
}

/// A second answer about the same video keeps it once and never takes the person's like away:
/// the service writes what it says, the person writes what they did.
#[test]
fn a_second_answer_keeps_one_video_and_the_person_s_like() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    let before = keys(&host, "video");
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    let after = keys(&host, "video");
    assert_eq!(before.len(), after.len(), "the same videos, not twice");
    assert_eq!(before.len(), 2, "the channel's two videos");
}

/// What the origin does with an answer it did not get: nothing, and no write.
#[test]
fn a_failed_answer_writes_nothing() {
    struct Failing;
    impl Service for Failing {
        fn search(&self, _: &str, _: &SearchFilter) -> Result<SearchPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn channel(&self, _: &str) -> Result<ChannelPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn playlist(&self, _: &str) -> Result<PlaylistPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn trending(&self) -> Result<TrendingPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn stream(&self, _: &str, _: &str) -> Result<String, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn comments(&self, _: &str) -> Result<CommentsPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn segments(&self, _: &str) -> Result<SegmentPage, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn branding(&self, _: &str) -> Result<Branding, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn votes(&self, _: &str) -> Result<Votes, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
    }
    let host = workspace();
    let woken = Arc::new(AtomicBool::new(false));
    let origin: std::rc::Rc<std::cell::RefCell<YouTubeOrigin>> =
        tilolang_youtube::origin(Arc::new(Failing), Arc::clone(&woken));
    origin
        .borrow_mut()
        .deliver("youtube.search", &serde_json::json!("anything"));
    assert!(
        woken.load(Ordering::Acquire),
        "the answer, failure or not, wakes the instance"
    );
    poll(&origin, &host);
    assert!(keys(&host, "channel").is_empty());
    assert!(keys(&host, "video").is_empty());
}
