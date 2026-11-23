//! The YouTube origin (`crates/platform/tilolang-youtube`): what a package's `youtube` types
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
    CaptionTrack, ChannelItem, ChannelPage, Playback, PlaylistItem, PlaylistPage, Quality,
    Scripted, SearchPage, Service, ServiceError, TrendingPage, VideoItem, YouTubeOrigin,
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
            Playback {
                url: "https://media.example/v-phone-1080.mp4".to_owned(),
                qualities: vec![
                    Quality {
                        label: "1080p".to_owned(),
                        url: "https://media.example/v-phone-1080.mp4".to_owned(),
                    },
                    Quality {
                        label: "720p".to_owned(),
                        url: "https://media.example/v-phone-720.mp4".to_owned(),
                    },
                ],
                captions: vec![CaptionTrack {
                    language: "en".to_owned(),
                    label: "English".to_owned(),
                    automatic: false,
                }],
            },
        )]),
        captions: std::collections::BTreeMap::from([(
            ("v-phone".to_owned(), "en".to_owned()),
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHello\n".to_owned(),
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

/// A video's field `name`, as the app reads it: what the service answered with.
fn video_field(
    host: &tilolang_tilo::host::Host,
    key: &str,
    name: &str,
) -> tilolang_tilo::runtime::Value {
    let packages = host.packages();
    let package = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    let (_, root_ty) = package.main().expect("the client's root");
    let video = package
        .meta
        .types
        .iter()
        .find(|t| t.name == "Video")
        .expect("the video kind");
    let field_id = video
        .fields
        .iter()
        .find(|f| f.name == name)
        .expect("the video's field")
        .id
        .clone();
    let key_id = video
        .fields
        .iter()
        .find(|f| f.name == "key")
        .expect("the video's key")
        .id
        .clone();
    let videos_field = package
        .meta
        .types
        .iter()
        .find(|t| t.id == root_ty)
        .and_then(|root| root.fields.iter().find(|f| f.name == "videos"))
        .expect("the root's videos")
        .id
        .clone();
    let root = host.root_of(TUBE).expect("the root");
    let Ok(tilolang_tilo::runtime::Value::List(items)) = host.runtime().read(&root, &videos_field)
    else {
        return tilolang_tilo::runtime::Value::Unit;
    };
    for value in items {
        let tilolang_tilo::runtime::Value::Ref(at) = value else {
            continue;
        };
        if host.runtime().read(&at.target, &key_id).ok()
            != Some(tilolang_tilo::runtime::Value::Text(key.to_owned()))
        {
            continue;
        }
        return host
            .runtime()
            .read(&at.target, &field_id)
            .unwrap_or(tilolang_tilo::runtime::Value::Unit);
    }
    tilolang_tilo::runtime::Value::Unit
}

/// The stream answer writes where the video plays from, the qualities it offers and its caption
/// tracks, and a caption answer writes the words of the track chosen.
#[test]
fn a_stream_answer_writes_its_qualities_and_captions_and_the_words_are_asked_for() {
    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    origin
        .borrow_mut()
        .deliver("youtube.stream", &serde_json::json!("v-phone"));
    poll(&origin, &host);
    assert_eq!(
        video_field(&host, "v-phone", "stream"),
        tilolang_tilo::runtime::Value::text("https://media.example/v-phone-1080.mp4"),
        "the video plays from its best whole-file stream"
    );
    assert_eq!(
        video_field(&host, "v-phone", "stream_qualities"),
        tilolang_tilo::runtime::Value::List(vec![
            tilolang_tilo::runtime::Value::text("1080p"),
            tilolang_tilo::runtime::Value::text("720p"),
        ]),
        "the quality menu lists the streams best first"
    );
    assert_eq!(
        video_field(&host, "v-phone", "caption_tracks"),
        tilolang_tilo::runtime::Value::List(vec![tilolang_tilo::runtime::Value::Tuple(vec![
            tilolang_tilo::runtime::Value::text("en"),
            tilolang_tilo::runtime::Value::text("English"),
        ])]),
        "the caption menu names the track and its language"
    );
    origin
        .borrow_mut()
        .deliver("youtube.captions", &serde_json::json!(["v-phone", "en"]));
    poll(&origin, &host);
    assert_eq!(
        video_field(&host, "v-phone", "captions"),
        tilolang_tilo::runtime::Value::text("WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHello\n"),
        "the chosen track's words are the video's to draw"
    );
}

/// The video of the app's collection whose key is `key`.
fn video_ref(host: &tilolang_tilo::host::Host, key: &str) -> tilolang_tilo::runtime::ResourceRef {
    use tilolang_tilo::runtime::Value;
    let root = host.root_of(TUBE).expect("the root");
    let Ok(Value::List(items)) = host.runtime().read(&root, "videos") else {
        panic!("the client holds its videos as a list");
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Ref(at) => Some((*at.target).clone()),
            _ => None,
        })
        .find(|video| host.runtime().read(video, "key").ok() == Some(Value::text(key)))
        .unwrap_or_else(|| panic!("the video {key}"))
}

/// What the client's action `action`, run by the person, answered.
fn call(
    host: &tilolang_tilo::host::Host,
    action: &str,
    args: Vec<tilolang_tilo::runtime::Value>,
) -> tilolang_tilo::runtime::InvokeOutcome {
    let id = host
        .package(TUBE)
        .expect("the client")
        .meta
        .actions
        .iter()
        .find(|held| held.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    host.runtime()
        .invoke_action(host.viewer(), (id.as_str(), args))
        .expect("the action runs")
}

/// A person picks the speed, the quality and the caption track with the client's own actions: the
/// speed is the client's, the quality picks the stream of that label, and a caption track is the
/// service's to fetch; turning them off is the client's alone.
#[test]
fn the_speed_the_quality_and_the_caption_track_are_the_person_s_actions() {
    use tilolang_tilo::runtime::{InvokeOutcome, Value};

    let host = workspace();
    let origin = tilolang_youtube::origin(Arc::new(script()), Arc::new(AtomicBool::new(false)));
    origin
        .borrow_mut()
        .deliver("youtube.channel", &serde_json::json!("UCmkbhd"));
    poll(&origin, &host);
    origin
        .borrow_mut()
        .deliver("youtube.stream", &serde_json::json!("v-phone"));
    poll(&origin, &host);
    let root = host.root_of(TUBE).expect("the root");
    let video = video_ref(&host, "v-phone");

    assert!(matches!(
        call(
            &host,
            "set_rate",
            vec![Value::resource(&root), Value::Float(1.5)]
        ),
        InvokeOutcome::Done(_)
    ));
    assert_eq!(
        host.runtime().read(&root, "rate").ok(),
        Some(Value::Float(1.5)),
        "the speed follows the person from video to video"
    );

    assert!(matches!(
        call(
            &host,
            "play_at_quality",
            vec![Value::resource(&video), Value::text("720p")]
        ),
        InvokeOutcome::Done(_)
    ));
    assert_eq!(
        host.runtime().read(&video, "quality").ok(),
        Some(Value::text("720p")),
        "the chosen quality is the person's, and the menu shows it"
    );
    assert_eq!(
        video_field(&host, "v-phone", "streams"),
        Value::List(vec![
            Value::Tuple(vec![
                Value::text("1080p"),
                Value::text("https://media.example/v-phone-1080.mp4"),
            ]),
            Value::Tuple(vec![
                Value::text("720p"),
                Value::text("https://media.example/v-phone-720.mp4"),
            ]),
        ]),
        "and the view plays that quality's own address"
    );

    // A track's words are YouTube's to fetch, so the track is on at once and the fetch waits in
    // the outbox for the device that carries outside effects out.
    assert!(matches!(
        call(
            &host,
            "show_captions",
            vec![Value::resource(&video), Value::text("en")]
        ),
        InvokeOutcome::Done(_)
    ));
    assert_eq!(
        host.runtime().read(&video, "caption_language").ok(),
        Some(Value::text("en")),
        "the person's choice of a track is the client's"
    );
    let asked = host.runtime().pending_outbox();
    assert_eq!(
        asked
            .iter()
            .map(|entry| (entry.capability.as_str(), entry.payload.clone()))
            .collect::<Vec<_>>(),
        [("youtube.captions", serde_json::json!(["v-phone", "en"]))],
        "the device carrying outside effects is asked for that track of that video"
    );

    // Off needs nothing from YouTube: the track goes, and the view draws no words.
    origin
        .borrow_mut()
        .deliver("youtube.captions", &serde_json::json!(["v-phone", "en"]));
    poll(&origin, &host);
    assert!(matches!(
        call(
            &host,
            "show_captions",
            vec![Value::resource(&video), Value::text("")]
        ),
        InvokeOutcome::Done(_)
    ));
    assert_eq!(
        host.runtime().read(&video, "caption_language").ok(),
        Some(Value::text("")),
        "no track is on"
    );
}

/// What the origin does with an answer it did not get: nothing, and no write.
#[test]
fn a_failed_answer_writes_nothing() {
    struct Failing;
    impl Service for Failing {
        fn search(&self, _: &str) -> Result<SearchPage, ServiceError> {
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
        fn stream(&self, _: &str) -> Result<Playback, ServiceError> {
            Err(ServiceError("unreachable".to_owned()))
        }
        fn captions(&self, _: &str, _: &str) -> Result<String, ServiceError> {
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
