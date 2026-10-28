//! Real `InnerTube` answers, trimmed to the rows and fields Tube reads (and
//! with stream URLs and visitor ids replaced), read into the model.

use serde_json::Value;

use super::*;

const SEARCH: &str = include_str!("fixtures/search.json");
const NEXT: &str = include_str!("fixtures/next.json");
const PLAYER: &str = include_str!("fixtures/player.json");
const PLAYER_BOT_CHECK: &str = include_str!("fixtures/player-bot-check.json");
const CHANNEL: &str = include_str!("fixtures/channel.json");

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).expect("fixtures are JSON")
}

#[test]
fn a_search_answer_gives_its_videos_and_creators_in_order() {
    let answer = fixture(SEARCH);
    let found = search(&answer, "Search");
    assert_eq!(
        visitor(&answer).as_deref(),
        Some("CgtleGFtcGxlVmlzaXRvcg%3D%3D")
    );

    assert_eq!(found.channels.len(), 1);
    let creator = &found.channels[0];
    assert_eq!(creator.name, "NBC News");
    assert_eq!(creator.handle, "@NBCNews");
    assert_eq!(creator.subscribers, 12_200_000);
    assert!(creator.verified);
    assert!(creator.avatar_url.starts_with("https://yt3.ggpht.com/"));

    assert_eq!(found.videos.len(), 4);
    let live = &found.videos[0];
    assert!(live.live, "a row viewers are watching is live");
    assert_eq!(live.views, 6_935);
    assert_eq!(live.duration, 0);
    assert_eq!(live.channel_name, "CRUX");
    assert_eq!(live.channel_id, "UC3prwMn9aU2z5Y158ZdGyyA");

    assert!(found.videos[1].live);
    let video = &found.videos[2];
    assert!(!video.live);
    assert!(!video.title.is_empty());
    assert!(video.views > 0);
    assert!(video.duration > 0);
    assert!(video.published.ends_with("ago"), "{}", video.published);
    assert_eq!(
        video.thumbnail_url,
        format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", video.id)
    );
    assert!(!video.channel_avatar_url.is_empty());
    assert!(!video.description.is_empty());
}

#[test]
fn a_watch_page_takes_the_length_and_description_from_the_player() {
    let next = fixture(NEXT);
    let player = fixture(PLAYER);
    let (video, related) = watch(&next, Some(&player), "n61ULEU7CO0").expect("a watch page");
    assert_eq!(
        video.title,
        "Best of lofi hip hop 2021 ✨ [beats to relax/study to]"
    );
    assert_eq!(video.channel_name, "Lofi Girl");
    assert_eq!(video.channel_id, "UCSJ4gkVC6NrvII8umztf0Ow");
    assert_eq!(video.views, 58_224_382);
    assert_eq!(video.duration, 22_258);
    assert_eq!(video.likes, 499_000);
    assert_eq!(video.published, "4 years ago");
    assert!(video.description.starts_with("🎼"));
    assert!(!video.channel_avatar_url.is_empty());

    assert_eq!(related.len(), 3);
    assert!(related[0].live);
    assert_eq!(related[0].channel_name, "Lofi Girl");
    assert_eq!(related[0].channel_id, "UCSJ4gkVC6NrvII8umztf0Ow");
    let recorded = &related[1];
    assert!(!recorded.live);
    assert!(recorded.duration > 0);
    assert!(recorded.views > 0);
    assert!(recorded.published.ends_with("ago"));
    assert!(!recorded.channel_name.is_empty());
}

#[test]
fn a_watch_page_without_a_player_answer_still_has_its_video() {
    let (video, _) = watch(&fixture(NEXT), None, "n61ULEU7CO0").expect("a watch page");
    assert_eq!(video.channel_name, "Lofi Girl");
    assert_eq!(video.duration, 0);
    assert!(!video.description.is_empty());
}

#[test]
fn the_player_names_its_progressive_stream_or_says_why_not() {
    let stream = stream(&fixture(PLAYER), None).expect("a stream");
    assert_eq!(stream.quality, "360p");
    assert!(stream.url.contains("itag=18"), "{}", stream.url);

    // The height the person chose decides which stream is served, so a
    // smaller choice is not silently ignored.
    let capped = super::stream(&fixture(PLAYER), Some(240)).expect("the smallest stream");
    assert_eq!(capped.quality, "360p", "nothing smaller exists than 360p");

    let refused = super::stream(&fixture(PLAYER_BOT_CHECK), None).expect_err("a bot check");
    assert!(refused.starts_with("LOGIN_REQUIRED"), "{refused}");

    // What YouTube said is kept, for the watch page to pass on.
    assert_eq!(refusal(&fixture(PLAYER)), None);
    assert_eq!(
        refusal(&fixture(PLAYER_BOT_CHECK)).as_deref(),
        Some("Sign in to confirm you\u{2019}re not a bot")
    );
    let error: anyhow::Error = super::super::Refused("no".into()).into();
    assert_eq!(super::super::refusal(&error).as_deref(), Some("no"));
}

#[test]
fn a_channel_page_gives_the_creator_and_its_uploads() {
    let (creator, uploads) =
        channel(&fixture(CHANNEL), "UCSJ4gkVC6NrvII8umztf0Ow").expect("a channel page");
    assert_eq!(creator.name, "Lofi Girl");
    assert_eq!(creator.handle, "@LofiGirl");
    assert_eq!(creator.subscribers, 15_800_000);
    assert!(!creator.avatar_url.is_empty());
    assert!(!creator.banner_url.is_empty());
    assert_eq!(uploads.len(), 3);
    for upload in &uploads {
        assert_eq!(upload.channel_id, "UCSJ4gkVC6NrvII8umztf0Ow");
        assert_eq!(upload.channel_name, "Lofi Girl");
        assert!(upload.duration > 0);
        assert!(upload.views > 0);
        assert!(upload.published.ends_with("ago"));
    }
}

#[test]
fn counts_and_clocks_read_as_youtube_shows_them() {
    assert_eq!(count("58,224,382 views"), 58_224_382);
    assert_eq!(count("1.2M views"), 1_200_000);
    assert_eq!(count("824K"), 824_000);
    assert_eq!(count("No views"), 0);
    assert_eq!(clock("1:04:42"), 3_882);
    assert_eq!(clock("3:41"), 221);
    assert_eq!(clock("LIVE"), 0);
}
