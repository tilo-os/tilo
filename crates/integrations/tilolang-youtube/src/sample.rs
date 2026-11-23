//! What YouTube holds in a sample workspace: five channels and their newest videos, a playlist,
//! and what YouTube lists as trending.
//!
//! The channels, videos and playlists are the service's, which no action of the app writes: a
//! development session's seed and a staged window take them in through the YouTube origin
//! ([`YouTubeOrigin`]) over a scripted service answering from this fixture, so they are written
//! by the same conversion a real answer goes through, as the service's change. What a person
//! does with them (following a channel, liking a video, keeping it for later, watching it) is the
//! app's own actions', which its scenario runs on what this took in.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin as _, OriginCx};
use tilolang_tilo::runtime::Delivery;

use crate::origin::YouTubeOrigin;
use crate::service::{
    ChannelItem, ChannelPage, PlaylistItem, PlaylistPage, Scripted, TrendingPage, VideoItem,
    scripted,
};

/// The playlist the sample holds.
const PLAYLIST: &str = "pl-science";

/// Take what YouTube holds in the sample into the package `app` of `host`: each channel's page,
/// the playlist and the trending list, asked of a scripted YouTube and written by the YouTube
/// origin as its answers are. `now` is when they are taken in; the origin dates its own changes.
///
/// # Errors
/// The package keeps no YouTube types, or the workspace did not take what YouTube said.
pub fn take_in(host: &Host, app: &str, now: &str) -> Result<(), String> {
    let woken = Arc::new(AtomicBool::new(false));
    let mut origin = YouTubeOrigin::new(scripted(service()), woken);
    let mut asks: Vec<(&str, serde_json::Value)> = channels()
        .iter()
        .map(|p| ("youtube.channel", serde_json::json!(p.channel.key)))
        .collect();
    asks.push(("youtube.playlist", serde_json::json!(PLAYLIST)));
    asks.push(("youtube.trending", serde_json::json!("")));
    for (capability, payload) in &asks {
        if let Delivery::Rejected(why) = origin.deliver(capability, payload) {
            return Err(why);
        }
    }
    let waker: tilolang_tilo::background::Waker = Arc::new(|| {});
    let cx = OriginCx {
        host,
        app,
        waker: &waker,
        now: now.to_owned(),
    };
    // A scripted service answers as it is asked: one poll writes every answer.
    let polled = origin.poll(&cx);
    if let Some(why) = origin.unreached() {
        return Err(why.to_owned());
    }
    if origin.waits() || !polled.wrote {
        return Err(format!("{app} took in nothing of what YouTube said"));
    }
    Ok(())
}

/// The scripted YouTube that answers the sample's questions.
fn service() -> Scripted {
    let mut service = Scripted::default();
    for page in channels() {
        service.channels.insert(page.channel.key.clone(), page);
    }
    let science = PlaylistPage {
        playlist: PlaylistItem {
            key: PLAYLIST.to_owned(),
            title: "Science, explained".to_owned(),
            channel_key: "UCveritasium".to_owned(),
            channel_name: "Veritasium".to_owned(),
            video_count: 2,
            ..PlaylistItem::default()
        },
        // In its maker's order, which here is the older first.
        videos: vec![video(WIRES), video(MAGNET)],
    };
    service.playlists.insert(PLAYLIST.to_owned(), science);
    service.trending = TrendingPage {
        videos: vec![video(PHONE), video(MAGNET)],
    };
    service
}

/// Each channel's page: the channel and its newest videos.
fn channels() -> Vec<ChannelPage> {
    vec![
        page(
            Channel {
                key: "UCmkbhd",
                name: "MKBHD",
                handle: "@mkbhd",
                subscribers: 19_800_000,
                verified: true,
                description: "Videos about technology, reviewed by someone who uses it every day.",
            },
            &[PHONE, WATCH],
        ),
        page(
            Channel {
                key: "UCveritasium",
                name: "Veritasium",
                handle: "@veritasium",
                subscribers: 16_400_000,
                verified: true,
                description: "An element of truth: science and engineering explained.",
            },
            &[MAGNET, WIRES],
        ),
        page(
            Channel {
                key: "UCcpu",
                name: "Computers Explained",
                handle: "@computers.explained",
                subscribers: 412_000,
                verified: false,
                description: "How the machines we use every day actually work.",
            },
            &[CACHE],
        ),
        page(
            Channel {
                key: "UCkitchen",
                name: "Small Kitchen",
                handle: "@smallkitchen",
                subscribers: 96_000,
                verified: false,
                description: "Cooking in a very small kitchen, one pan at a time.",
            },
            &[BREAD],
        ),
        page(
            Channel {
                key: "UCbrass",
                name: "The Brass Section",
                handle: "@brass.section",
                subscribers: 218_000,
                verified: false,
                description: "Brass bands, rehearsals and the people who play in them.",
            },
            &[REHEARSAL],
        ),
    ]
}

/// A channel of the sample, as YouTube describes it.
#[derive(Clone, Copy)]
struct Channel {
    key: &'static str,
    name: &'static str,
    handle: &'static str,
    subscribers: i64,
    verified: bool,
    description: &'static str,
}

fn page(channel: Channel, videos: &[Video]) -> ChannelPage {
    ChannelPage {
        channel: ChannelItem {
            key: channel.key.to_owned(),
            name: channel.name.to_owned(),
            handle: channel.handle.to_owned(),
            subscribers: channel.subscribers,
            description: channel.description.to_owned(),
            verified: channel.verified,
            ..ChannelItem::default()
        },
        videos: videos.iter().map(|v| video(*v)).collect(),
    }
}

/// A video of the sample, as YouTube describes it.
#[derive(Clone, Copy)]
struct Video {
    key: &'static str,
    title: &'static str,
    channel_key: &'static str,
    channel_name: &'static str,
    views: i64,
    published: &'static str,
    /// How long it runs, in seconds.
    duration: i64,
    description: &'static str,
    likes: i64,
}

fn video(v: Video) -> VideoItem {
    VideoItem {
        key: v.key.to_owned(),
        title: v.title.to_owned(),
        channel_key: v.channel_key.to_owned(),
        channel_name: v.channel_name.to_owned(),
        views: v.views,
        published: v.published.to_owned(),
        duration: v.duration,
        description: v.description.to_owned(),
        likes: v.likes,
        live: false,
        ..VideoItem::default()
    }
}

const PHONE: Video = Video {
    key: "v-phone",
    title: "The phone that finally lasts a week",
    channel_key: "UCmkbhd",
    channel_name: "MKBHD",
    views: 1_840_000,
    published: "2026-10-28T14:00:00Z",
    duration: 921,
    description: "A week of use, three days of battery, and what that changes about the way you carry a phone.",
    likes: 96_000,
};

const MAGNET: Video = Video {
    key: "v-magnet",
    title: "Why every magnet you own is lying to you",
    channel_key: "UCveritasium",
    channel_name: "Veritasium",
    views: 4_310_000,
    published: "2026-10-27T16:30:00Z",
    duration: 1342,
    description: "The field lines people draw are not what is there. Here is what is.",
    likes: 310_000,
};

const CACHE: Video = Video {
    key: "v-cache",
    title: "A cache is a lie your computer tells you",
    channel_key: "UCcpu",
    channel_name: "Computers Explained",
    views: 210_000,
    published: "2026-10-27T09:00:00Z",
    duration: 745,
    description: "Why the fastest memory is the memory you forgot you had, and what happens when it is wrong.",
    likes: 14_200,
};

const BREAD: Video = Video {
    key: "v-bread",
    title: "Bread in a one-metre kitchen",
    channel_key: "UCkitchen",
    channel_name: "Small Kitchen",
    views: 48_000,
    published: "2026-10-26T18:15:00Z",
    duration: 611,
    description: "Four ingredients, one bowl, and the smallest counter in the world.",
    likes: 3_900,
};

const REHEARSAL: Video = Video {
    key: "v-rehearsal",
    title: "Twenty minutes before the contest",
    channel_key: "UCbrass",
    channel_name: "The Brass Section",
    views: 91_000,
    published: "2026-10-26T07:45:00Z",
    duration: 1180,
    description: "A band warms up in a car park. Then the crowd comes in.",
    likes: 8_800,
};

const WATCH: Video = Video {
    key: "v-watch",
    title: "The watch that needs no winding",
    channel_key: "UCmkbhd",
    channel_name: "MKBHD",
    views: 2_740_000,
    published: "2026-10-24T15:00:00Z",
    duration: 688,
    description: "Six months with a watch that charges itself from the way you move.",
    likes: 148_000,
};

const WIRES: Video = Video {
    key: "v-wires",
    title: "Why undersea cables do not break",
    channel_key: "UCveritasium",
    channel_name: "Veritasium",
    views: 6_120_000,
    published: "2026-10-22T12:00:00Z",
    duration: 1594,
    description: "The internet runs on cables on the sea floor. People repair them by hand.",
    likes: 402_000,
};

/// Have a sample workspace's YouTube answer from [`take_in`]
/// (`tilolang_tilo::origin::take_in_samples`).
pub fn register() {
    tilolang_tilo::origin::register_sample("youtube", SAMPLE);
}

/// What [`register`] hands the platform.
const SAMPLE: &tilolang_tilo::origin::Sample = &take_in;
