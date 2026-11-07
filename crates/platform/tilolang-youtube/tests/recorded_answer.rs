//! A recorded YouTube answer, through the client's mapping and the origin's write: what a
//! search found reaches the package's records — its thumbnail and its view count — and the view
//! it draws shows them.
//!
//! **The fixture is a recording.** `fixtures/search-recorded.json` is a real InnerTube search
//! answer (`POST /youtubei/v1/search`, the WEB client, query "linux kernel news"), trimmed to
//! the three video rows this test reads and with its visitor id removed. Its rows are what
//! rustypipe's own types deserialize to, so the mapping under test is the one production runs;
//! what the test replaces is the network — the picture fetch answers a real PNG — and the store
//! the pictures are kept in.
//!
//! Nothing here claims YouTube answered just now: it claims that the fields a recording carries
//! survive into the record and the view, which is what the client showed missing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::doc_markdown,
    reason = "test assertions; YouTube is a name, not code"
)]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rustypipe::model::YouTubeItem;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::runtime::Value;

const TUBE: &str = "so.tilo.lang.tube";

/// The recorded answer: a real search's rows, as the client receives them.
const RECORDED: &str = include_str!("fixtures/search-recorded.json");

/// The recording's rows, as the client's own types read them: a `VideoItem` deserializes from
/// exactly the shape rustypipe's mapper builds, so this is the model the mapping sees, read
/// from a real answer rather than made up by the test.
fn recorded_items() -> Vec<YouTubeItem> {
    let answer: serde_json::Value = serde_json::from_str(RECORDED).expect("the recording is JSON");
    let rows = answer["contents"]["twoColumnSearchResultsRenderer"]["primaryContents"]
        ["sectionListRenderer"]["contents"][0]["itemSectionRenderer"]["contents"]
        .as_array()
        .expect("the recording's rows");
    rows.iter()
        .map(|row| {
            let video = &row["videoRenderer"];
            let id = video["videoId"].as_str().expect("a video id");
            let thumbnails = video["thumbnail"]["thumbnails"]
                .as_array()
                .expect("its thumbnails");
            let name = video["title"]["runs"][0]["text"].as_str().expect("a title");
            let channel_name = video["shortBylineText"]["runs"][0]["text"]
                .as_str()
                .expect("a channel");
            let views = video["viewCountText"]["simpleText"]
                .as_str()
                .expect("a view count");
            let model = serde_json::json!({
                "id": id,
                "name": name,
                "duration": null,
                "thumbnail": thumbnails,
                "channel": {
                    "id": "UC-linux",
                    "name": channel_name,
                    "avatar": [],
                    "verification": "none",
                    "subscriber_count": null,
                },
                "publish_date": null,
                "publish_date_txt": null,
                "view_count": views_count(views),
                "is_live": false,
                "is_short": false,
                "is_upcoming": false,
                "short_description": null,
            });
            YouTubeItem::Video(serde_json::from_value(model).expect("a video"))
        })
        .collect()
}

/// The count `text` writes ("56,988 views"), as the client reads it.
fn views_count(text: &str) -> u64 {
    text.chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("a count")
}

/// An 8 by 8 PNG, as the picture fetch answers: a real still, which the window's own decoder
/// reads as a picture.
fn png() -> Vec<u8> {
    let rgba: Vec<u8> = std::iter::repeat_n([40u8, 120, 200, 255], 8 * 8)
        .flatten()
        .collect();
    ui::png::encode_rgba8(8, 8, &rgba).expect("a png")
}

/// The recording as the client's page: its pictures fetched (answering a real PNG) and kept in
/// a store of the test's, which is where the package's addresses point.
fn recorded_page() -> (tilolang_youtube::SearchPage, ui::content::ContentStore) {
    let store = ui::content::ContentStore::memory();
    let bytes = png();
    let fetch: tilolang_youtube::live::PictureFetch = Arc::new(move |url: &str| {
        // Only the image CDNs: a fetch of anything else is a test's mistake.
        url.starts_with("https://i.ytimg.com/")
            .then(|| bytes.clone())
    });
    let client =
        tilolang_youtube::live::LiveClient::over(fetch, Some(store.clone())).expect("the client");
    (
        tilolang_youtube::live::page_of(&client, "linux kernel news", recorded_items()),
        store,
    )
}

/// The client's package, loaded over a workspace of its own.
fn workspace() -> tilolang_tilo::host::Host {
    let doc = tilo_resource::Doc::new();
    let host =
        tilolang_tilo::host::Host::new(&doc, tilolang_tilo::runtime::Principal::new("Ada"), || {
            None
        })
        .unwrap();
    host.load_builtin(
        include_str!("../../../../apps/tl/tube/app.tl"),
        include_str!("../../../../apps/tl/tube/app.ids"),
    )
    .unwrap();
    host.root_of(TUBE).unwrap();
    host
}

/// Write `page` into `host`'s record as the service answered it, and settle the origin.
fn written(host: &tilolang_tilo::host::Host, page: tilolang_youtube::SearchPage) {
    let origin = tilolang_youtube::origin(
        Arc::new(tilolang_youtube::Scripted {
            searches: [("linux kernel news".to_owned(), page)]
                .into_iter()
                .collect(),
            ..tilolang_youtube::Scripted::default()
        }),
        Arc::new(AtomicBool::new(false)),
    );
    origin
        .borrow_mut()
        .deliver("youtube.search", &serde_json::json!("linux kernel news"));
    let cx = OriginCx {
        host,
        app: TUBE,
        waker: &(Arc::new(|| {}) as tilolang_tilo::background::Waker),
        now: "2026-11-07T10:00:00Z".to_owned(),
    };
    origin.borrow_mut().poll(&cx);
}

/// Every video the client holds, by its key, as the app reads it.
fn videos(host: &tilolang_tilo::host::Host) -> Vec<(String, Vec<(String, Value)>)> {
    let root = host.root_of(TUBE).unwrap();
    let Value::List(items) = host.runtime().read(&root, "videos").unwrap() else {
        panic!("the client holds its videos")
    };
    items
        .into_iter()
        .filter_map(|item| {
            let Value::Ref(at) = item else {
                return None;
            };
            let Ok(Value::Text(key)) = host.runtime().read(&at.target, "key") else {
                return None;
            };
            let fields = ["views", "thumbnail", "channel_name", "title"]
                .into_iter()
                .map(|name| {
                    (
                        name.to_owned(),
                        host.runtime().read(&at.target, name).unwrap(),
                    )
                })
                .collect();
            Some((key, fields))
        })
        .collect()
}

/// The card the client draws for the video `key` in `video_grid`, the view that draws videos as
/// cards, as the view's rendered tree holds it: the art the card shows and the line under the
/// title.
fn card_of(host: &tilolang_tilo::host::Host) -> (String, Vec<String>) {
    let view = host
        .package(TUBE)
        .and_then(|package| {
            package
                .meta
                .views
                .iter()
                .find(|declared| declared.name == "video_grid")
                .map(|declared| declared.id.clone())
        })
        .expect("the package declares `video_grid`");
    let root = host.root_of(TUBE).unwrap();
    let Value::List(items) = host.runtime().read(&root, "videos").unwrap() else {
        panic!("the client holds its videos")
    };
    // The grid takes the videos it draws, and for what: the search that found them, as a
    // place that lists them gives it.
    let rendered = host
        .runtime()
        .open_view(
            &tilolang_tilo::runtime::Principal::new("Ada"),
            &view,
            vec![Value::List(items), Value::text(""), Value::text("")],
        )
        .expect("the view runs");
    let card = media_card(rendered.tree())
        .unwrap_or_else(|| panic!("the view draws cards: {}", rendered.tree().kind));
    let picture = match card.props.get("picture") {
        Some(Value::Picture(picture)) => picture.clone(),
        Some(other) => panic!("the card's picture: {other:?}"),
        None => panic!(
            "the card has a picture: {:?}",
            card.props.keys().collect::<Vec<_>>()
        ),
    };
    // The card's lines under its title: the channel, then how many watched it and how long it
    // runs.
    let meta = match card.props.get("meta") {
        Some(Value::List(lines)) => lines
            .iter()
            .filter_map(|line| match line {
                Value::Text(line) => Some(line.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        other => panic!("the card's lines: {other:?}"),
    };
    (picture, meta)
}

/// The first `MediaCard` in `tree`, depth first.
fn media_card(tree: &tilolang_runtime::UiNode) -> Option<&tilolang_runtime::UiNode> {
    if tree.kind == "ui.MediaCard" {
        return Some(tree);
    }
    tree.children.iter().find_map(media_card)
}

#[test]
fn a_recorded_search_s_thumbnails_and_view_counts_reach_the_record() {
    let (page, store) = recorded_page();
    // What the recording says, as the client read it: three videos, each with a real count.
    assert_eq!(page.videos.len(), 3, "the recording's rows");
    for video in &page.videos {
        assert!(
            video.views > 1_000,
            "{} has its count: {}",
            video.key,
            video.views
        );
        assert!(
            video.thumbnail.starts_with("tilo-blob://"),
            "{}'s still is content the window reads, not a URL it cannot: {}",
            video.key,
            video.thumbnail
        );
        let hash = video.thumbnail.strip_prefix("tilo-blob://").unwrap();
        assert!(
            store.read(hash).is_some(),
            "{}'s still is kept where its address says: {}",
            video.key,
            video.thumbnail
        );
    }

    let host = workspace();
    written(&host, page);
    let held = videos(&host);
    assert_eq!(held.len(), 3, "the record holds the recording's videos");
    for (key, fields) in &held {
        let field = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v);
        let views = match field("views") {
            Some(Value::Int(views)) => *views,
            other => panic!("{key}'s views are a count: {other:?}"),
        };
        assert!(
            views > 1_000,
            "{key} keeps the count the recording carries: {views}"
        );
        let thumbnail = match field("thumbnail") {
            Some(Value::Text(thumbnail)) => thumbnail.clone(),
            other => panic!("{key}'s still: {other:?}"),
        };
        assert!(
            thumbnail.starts_with("tilo-blob://"),
            "{key}'s still is content: {thumbnail}"
        );
        assert_ne!(
            field("channel_name"),
            Some(&Value::Text(String::new())),
            "{key} keeps the channel the recording names"
        );
    }
}

#[test]
fn a_recorded_search_s_card_shows_the_thumbnail_and_the_count() {
    let (page, _store) = recorded_page();
    let first = page.videos.first().expect("a video").clone();
    let host = workspace();
    written(&host, page);

    let (picture, meta) = card_of(&host);
    assert!(
        picture.starts_with("tilo-blob://"),
        "the card's art is the picture the record holds: {picture}"
    );
    // The client says a count in its own words ("56K views"); the recording's count is the one
    // it reads, and a line that is not "0 views" is that count.
    assert!(
        meta.iter()
            .any(|line| line != "0 views" && line != "1 view"),
        "the card says the video's count rather than none: {meta:?}"
    );
    assert!(
        meta.iter()
            .any(|line| line.contains(&seen_text(first.views))),
        "the card's line carries the recording's count ({}): {meta:?}",
        first.views
    );
}

/// The count as the client's words say it: "57K", "1.2M", "512".
fn seen_text(views: i64) -> String {
    if views >= 1_000_000 {
        format!("{}M", views / 100_000)
    } else if views >= 1_000 {
        let tenths = views / 100;
        if tenths % 10 == 0 {
            format!("{}K", tenths / 10)
        } else {
            format!("{}.{}K", tenths / 10, tenths % 10)
        }
    } else {
        format!("{views}")
    }
}
