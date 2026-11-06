//! The live service: YouTube's InnerTube client, carrying the answers the origin writes.
//!
//! The client is rustypipe through [`tilo_innertube`](tilo_innertube), as Tube's own live backend
//! uses it. It is built once per process inside the app runtime, on the thread the queries run
//! on: rustypipe's own connections and its visitor-data prefetch live on that runtime, not in the
//! caller.

use std::sync::OnceLock;

use rustypipe::client::{RustyPipe, RustyPipeQuery};
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{ChannelItem, PlaylistItem, VideoItem, YouTubeItem};

use crate::service::{
    ChannelItem as SaidChannel, ChannelPage, PlaylistItem as SaidPlaylist, SearchPage,
    ServiceError, TrendingPage, VideoItem as SaidVideo,
};

/// The one rustypipe client of the process, with the runtime its queries wait on.
pub struct LiveClient {
    client: &'static RustyPipe,
    runtime: &'static ui::rt::AppRuntime,
}

/// The process's client, built on first use inside the app runtime.
///
/// # Errors
/// No app runtime, or the client could not be built.
pub fn client() -> Result<LiveClient, ServiceError> {
    static CLIENT: OnceLock<Result<RustyPipe, String>> = OnceLock::new();
    let runtime = ui::rt::try_app()
        .map_err(|e| ServiceError(format!("the YouTube client needs the app runtime: {e}")))?;
    let built = runtime.block_on(async {
        CLIENT
            .get_or_init(|| {
                tilo_net::install_crypto_provider();
                // rustypipe's reporter writes a JSON file into the process's working
                // directory on every parser warning; a shipped app must not litter it, and
                // core fields are extracted regardless.
                RustyPipe::builder()
                    .no_reporter()
                    .no_botguard()
                    .build()
                    .map_err(|e| format!("rustypipe build: {e}"))
            })
            .as_ref()
            .map_err(|error| ServiceError(error.clone()))
    });
    let built =
        built.map_err(|error| ServiceError(format!("the YouTube client's runtime: {error}")))??;
    Ok(LiveClient {
        client: built,
        runtime,
    })
}

impl LiveClient {
    fn query(&self) -> RustyPipeQuery {
        self.client.query()
    }

    fn block<F, T, E>(&self, future: F) -> Result<T, ServiceError>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let ran = self
            .runtime
            .block_on(future)
            .map_err(|error| ServiceError(format!("the YouTube client's runtime: {error}")))?;
        ran.map_err(|e| ServiceError(e.to_string()))
    }
}

/// What YouTube finds for `query`, split the way the client's search view lists it.
///
/// # Errors
/// YouTube could not be reached, or refused the query.
pub fn search(client: &LiveClient, query: &str) -> Result<SearchPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::search(&q, query))?;
    let mut page = SearchPage {
        query: query.to_owned(),
        ..SearchPage::default()
    };
    for item in found.items.items {
        match item {
            YouTubeItem::Video(v) => page.videos.push(said_video(v)),
            YouTubeItem::Playlist(p) => page.playlists.push(said_playlist(p)),
            YouTubeItem::Channel(c) => page.channels.push(said_channel(c)),
        }
    }
    Ok(page)
}

/// A channel's page and its newest videos.
///
/// # Errors
/// YouTube could not be reached, or has no such channel.
pub fn channel(client: &LiveClient, key: &str) -> Result<ChannelPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::channel_videos(&q, key))?;
    let videos = found.content.items.into_iter().map(said_video).collect();
    Ok(ChannelPage {
        channel: SaidChannel {
            key: found.id.clone(),
            name: found.name.clone(),
            handle: found.handle.clone().unwrap_or_else(|| found.id.clone()),
            subscribers: found.subscriber_count.unwrap_or(0).cast_signed(),
            avatar: best_thumb(&found.avatar),
            banner: best_thumb(&found.banner),
            description: found.description.clone(),
            verified: !matches!(found.verification, rustypipe::model::Verification::None),
        },
        videos,
    })
}

/// The videos YouTube lists as trending.
///
/// # Errors
/// YouTube could not be reached.
pub fn trending(client: &LiveClient) -> Result<TrendingPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::trending(&q))?;
    Ok(TrendingPage {
        videos: found.into_iter().map(said_video).collect(),
    })
}

/// The address a video's bytes play from: the progressive stream carrying picture and sound,
/// so a plain HTTP source can stream, seek and reach the end.
///
/// # Errors
/// YouTube could not be reached, or serves no playable stream of it.
pub fn stream(client: &LiveClient, key: &str) -> Result<String, ServiceError> {
    let q = client.query();
    let player = client.block(tilo_innertube::player(&q, key))?;
    tilo_innertube::whole_file_stream(&player.video_streams, None)
        .map(|stream| stream.url.clone())
        .ok_or_else(|| ServiceError(format!("YouTube serves no playable stream of {key}")))
}

fn said_video(v: VideoItem) -> SaidVideo {
    let (channel_key, channel_name, channel_avatar) = v.channel.map_or_else(
        || (String::new(), String::new(), String::new()),
        |c| (c.id, c.name, best_thumb(&c.avatar)),
    );
    SaidVideo {
        thumbnail: thumb_of(&v.id),
        key: v.id,
        title: v.name,
        channel_key,
        channel_name,
        channel_avatar,
        views: v.view_count.unwrap_or(0).cast_signed(),
        // A time the runtime records: YouTube's own instant when it sent one, never its "11
        // months ago", which reads differently every day and in every language.
        published: v.publish_date.map_or_else(
            || "1970-01-01T00:00:00Z".to_owned(),
            |at| {
                at.to_offset(tilo_innertube::time::UtcOffset::UTC)
                    .format(&tilo_innertube::time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
            },
        ),
        duration: i64::from(v.duration.unwrap_or(0)),
        description: v.short_description.unwrap_or_default(),
        // The search's and the channel's lists carry no like count; the watch page does.
        likes: 0,
        live: v.is_live,
    }
}

fn said_playlist(p: PlaylistItem) -> SaidPlaylist {
    let (channel_key, channel_name) = p
        .channel
        .map_or_else(|| (String::new(), String::new()), |c| (c.id, c.name));
    SaidPlaylist {
        thumbnail: best_thumb(&p.thumbnail),
        key: p.id,
        title: p.name,
        channel_key,
        channel_name,
        video_count: p.video_count.unwrap_or(0).cast_signed(),
    }
}

fn said_channel(c: ChannelItem) -> SaidChannel {
    SaidChannel {
        key: c.id.clone(),
        name: c.name,
        handle: c.handle.unwrap_or_else(|| c.id.clone()),
        subscribers: c.subscriber_count.unwrap_or(0).cast_signed(),
        avatar: best_thumb(&c.avatar),
        banner: String::new(),
        description: c.short_description,
        verified: !matches!(c.verification, rustypipe::model::Verification::None),
    }
}

/// The still a video serves, from its id alone: `hqdefault.jpg` is the stable one every public
/// video has, so a card has a picture without a second request.
fn thumb_of(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("https://i.ytimg.com/vi/{key}/hqdefault.jpg")
    }
}

/// The largest of a thumbnail set, made absolute: the service hands some back protocol-relative
/// (`//yt3.ggpht.com/…`), which a plain HTTP fetch refuses for want of a scheme.
fn best_thumb(thumbs: &[rustypipe::model::Thumbnail]) -> String {
    thumbs
        .iter()
        .max_by_key(|t| u64::from(t.width).saturating_mul(u64::from(t.height)))
        .map(|t| {
            t.url
                .strip_prefix("//")
                .map_or_else(|| t.url.clone(), |rest| format!("https://{rest}"))
        })
        .unwrap_or_default()
}

/// A comment's words without its markup, for a service that keeps them.
#[must_use]
pub fn plain(text: &impl ToPlaintext) -> String {
    text.to_plaintext()
}
