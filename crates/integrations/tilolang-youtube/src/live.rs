//! The live service: YouTube's InnerTube client, carrying the answers the origin writes.
//!
//! The client is rustypipe through [`tilo_innertube`](tilo_innertube), as Tube's own live backend
//! uses it. It is built once per process inside the app runtime, on the thread the queries run
//! on: rustypipe's own connections and its visitor-data prefetch live on that runtime, not in the
//! caller.
//!
//! **Pictures come through here too.** Every still YouTube names (a video's thumbnail, a
//! channel's avatar and banner, a playlist's cover) is a URL on an image CDN, and the app that
//! shows it never has the network: its window is a sandbox, and the pictures it loads are its
//! content store's (`ui::pictures::BLOB_SCHEME`, a content address) or a `data:` address carried
//! in the record. So each answer's pictures are fetched here, in the connector — the one process
//! that does have the network — and kept as this device's content, whose address the answer
//! carries in place of the URL. A picture that cannot be fetched leaves its field empty, which
//! every card draws as its placeholder; it never holds an address the window cannot read.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::OnceLock;

use rustypipe::client::{RustyPipe, RustyPipeQuery};
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{ChannelItem, PlaylistItem, VideoItem, YouTubeItem};

use crate::service::{
    Branding, ChannelItem as SaidChannel, ChannelPage, CommentItem as SaidComment, CommentsPage,
    PlaylistItem as SaidPlaylist, PlaylistPage, SearchFilter, SearchPage, SegmentItem, SegmentPage,
    ServiceError, TrendingPage, VideoItem as SaidVideo, Votes,
};

/// The one rustypipe client of the process, with the runtime its queries wait on.
pub struct LiveClient {
    client: &'static RustyPipe,
    runtime: &'static ui::rt::AppRuntime,
    /// The pictures this process has fetched, by the URL they came from, with the content
    /// address each is kept at: an answer naming a still twice (a video in a search and in its
    /// channel's page) fetches it once, and two answers of the same picture name one content.
    /// Empty for a service over a socket, whose pictures the connector fetched.
    held: std::sync::Mutex<BTreeMap<String, String>>,
    /// The content store the pictures are kept in: this device's, which the app reads its
    /// `tilo-blob://` addresses from, or a store of its own where the process was handed none
    /// (a test). `None` leaves every picture's field empty.
    content: Option<ui::content::ContentStore>,
    /// The bytes of the picture at a URL: this device's network. A test hands one answering a
    /// recording, so what a recorded answer becomes is a claim a test can make.
    fetch: PictureFetch,
}

/// How a picture's bytes are fetched from its URL.
pub type PictureFetch = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

impl LiveClient {
    /// A client over the process's YouTube client that fetches pictures with `fetch` and keeps
    /// them in `content`, as a test hands a recording's bytes and a store of its own: what a
    /// recorded answer becomes is then a claim the test makes, with no network.
    pub fn over(
        fetch: PictureFetch,
        content: Option<ui::content::ContentStore>,
    ) -> Result<Self, ServiceError> {
        let mut client = client()?;
        client.fetch = fetch;
        client.content = content;
        Ok(client)
    }
}

impl std::fmt::Debug for LiveClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveClient")
            .field("pictures", &self.held.lock().map(|held| held.len()))
            .finish_non_exhaustive()
    }
}

/// The process's client, built on first use inside the app runtime.
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
        held: std::sync::Mutex::default(),
        // The device's content store, which the app's window reads its pictures from. A process
        // the launcher handed none (a test, a tool run outside a session) keeps none: the
        // answers' pictures are then left empty rather than naming content nobody holds.
        content: {
            let store = ui::content::device();
            // A connector whose pictures stay empty is read as the app's fault. Which store
            // this process reached, and whether it reached one at all, is what says otherwise.
            tracing::debug!(
                holds_a_content_store = store.is_some(),
                "the YouTube client resolved its content store"
            );
            store
        },
        fetch: Arc::new(fetch_over_http),
    })
}

/// The bytes of the picture at `url`, from this device's network: `None` for a URL nothing
/// answered, or one larger than an image CDN ever serves.
fn fetch_over_http(url: &str) -> Option<Vec<u8>> {
    /// The most a picture YouTube serves costs. A thumbnail is tens of kilobytes; this is far
    /// above the largest still, and below anything that would fill the store's cache.
    const MAX_PICTURE_BYTES: u64 = 8 << 20;
    // One client for the process: its connection pool and TLS setup are the slow part, and an
    // answer names tens of stills at once.
    static CLIENT: OnceLock<Option<tilo_net::http::BlockingClient>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        let built = tilo_net::http::blocking_client()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|error| tracing::warn!(%error, "no client for YouTube's pictures"));
        built.ok()
    });
    // Every way this can fail says which: a picture that could not be fetched is named by its
    // URL and its reason, never left for someone to guess from a placeholder.
    let Some(client) = client.as_ref() else {
        tracing::warn!(%url, "no client for YouTube's pictures");
        return None;
    };
    let response = match client
        .get(url)
        // A browser's user agent: the image CDNs answer an unknown client with a 403.
        .header(
            "user-agent",
            "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0",
        )
        .send()
    {
        Ok(response) => match response.error_for_status() {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%url, %error, "YouTube's picture host refused a still");
                return None;
            }
        },
        Err(error) => {
            tracing::warn!(%url, %error, "a YouTube picture could not be fetched");
            return None;
        }
    };
    if response
        .content_length()
        .is_some_and(|len| len > MAX_PICTURE_BYTES)
    {
        tracing::warn!(%url, "a YouTube picture is larger than a still ever is");
        return None;
    }
    let bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%url, %error, "a YouTube picture's bytes could not be read");
            return None;
        }
    };
    let bytes = bytes.to_vec();
    if bytes.is_empty() || u64::try_from(bytes.len()).is_ok_and(|len| len > MAX_PICTURE_BYTES) {
        tracing::warn!(%url, len = bytes.len(), "a YouTube picture's bytes are not a still");
        return None;
    }
    tracing::debug!(%url, len = bytes.len(), "a YouTube picture is fetched");
    Some(bytes)
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

    /// `url` as the address the app reads the picture by: the content address its bytes are
    /// kept at, fetched here once. Empty for a URL nothing answered: the field reads empty, its
    /// card drawing its placeholder, never an address the window cannot read.
    fn held_picture(&self, url: &str) -> String {
        if url.is_empty() {
            return String::new();
        }
        if let Some(kept) = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(url)
        {
            return kept.clone();
        }
        // A connector with no content store is a launch the launcher handed no device store:
        // its answers carry no picture, and saying so is the only clue the person's window
        // gets as to why every still is a placeholder.
        let Some(content) = &self.content else {
            tracing::warn!(
                %url,
                "this process holds no content store: the answer's pictures are left empty"
            );
            return String::new();
        };
        let Some(bytes) = (self.fetch)(url) else {
            return String::new();
        };
        let hash = match content.put(&bytes) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(%url, %error, "a YouTube picture could not be kept");
                return String::new();
            }
        };
        let address = format!("{}{hash}", ui::pictures::BLOB_SCHEME);
        // The address is only readable by a window whose store holds the same content: say
        // where this one put it, and whether reading it back through the store answers.
        tracing::debug!(
            %url,
            len = bytes.len(),
            hash = %hash,
            read_back = content.read(&hash).is_some(),
            "a YouTube picture is kept as this device's content"
        );
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(url.to_owned(), address.clone());
        address
    }
}

/// What YouTube finds for `query`, narrowed by `filter`, split the way the client's search view
/// lists it.
pub fn search(
    client: &LiveClient,
    query: &str,
    filter: &SearchFilter,
) -> Result<SearchPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::search_filter(
        &q,
        query,
        &search_filter(filter),
    ))?;
    Ok(page_of(client, query, found.items.items))
}

/// The filter a search narrows by, in rustypipe's own terms: each part the client names, and
/// nothing for one it leaves `any`.
fn search_filter(filter: &SearchFilter) -> rustypipe::param::search_filter::SearchFilter {
    use rustypipe::param::search_filter::{ItemType, Length, SearchFilter as Rusty, UploadDate};
    let mut narrowed = Rusty::new();
    match filter.kind.as_str() {
        "video" => narrowed = narrowed.item_type(ItemType::Video),
        "channel" => narrowed = narrowed.item_type(ItemType::Channel),
        "playlist" => narrowed = narrowed.item_type(ItemType::Playlist),
        _ => {}
    }
    match filter.length.as_str() {
        "short" => narrowed = narrowed.length(Length::Short),
        "medium" => narrowed = narrowed.length(Length::Medium),
        "long" => narrowed = narrowed.length(Length::Long),
        _ => {}
    }
    match filter.published.as_str() {
        "hour" => narrowed = narrowed.date(UploadDate::Hour),
        "today" | "day" => narrowed = narrowed.date(UploadDate::Day),
        "week" => narrowed = narrowed.date(UploadDate::Week),
        "month" => narrowed = narrowed.date(UploadDate::Month),
        "year" => narrowed = narrowed.date(UploadDate::Year),
        _ => {}
    }
    narrowed
}

/// What the client's items become for the client: each still held as content, and every field
/// of it the package's kinds read. `query` is what a search found them for.
///
/// This is the boundary a recorded answer is mapped at: [`LiveClient::over`] takes a test's own
/// fetch and store, and `page_of` turns the client's own items — the ones a recording's rows
/// deserialize to — into the page the origin writes.
#[must_use]
pub fn page_of(
    client: &LiveClient,
    query: &str,
    items: impl IntoIterator<Item = YouTubeItem>,
) -> SearchPage {
    let mut page = SearchPage {
        query: query.to_owned(),
        ..SearchPage::default()
    };
    for item in items {
        match item {
            YouTubeItem::Video(v) => page.videos.push(said_video(v, client)),
            YouTubeItem::Playlist(p) => page.playlists.push(said_playlist(p, client)),
            YouTubeItem::Channel(c) => page.channels.push(said_channel(c, client)),
        }
    }
    page
}

/// A channel's page and its newest videos.
pub fn channel(client: &LiveClient, key: &str) -> Result<ChannelPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::channel_videos(&q, key))?;
    let videos = found
        .content
        .items
        .into_iter()
        .map(|v| said_video(v, client))
        .collect();
    Ok(ChannelPage {
        channel: SaidChannel {
            key: found.id.clone(),
            name: found.name.clone(),
            handle: found.handle.clone().unwrap_or_else(|| found.id.clone()),
            subscribers: found.subscriber_count.unwrap_or(0).cast_signed(),
            avatar: client.held_picture(&best_thumb(&found.avatar)),
            banner: client.held_picture(&best_thumb(&found.banner)),
            description: found.description.clone(),
            verified: !matches!(found.verification, rustypipe::model::Verification::None),
        },
        videos,
    })
}

/// A playlist's page, and the videos it holds in the order its maker put them.
pub fn playlist(client: &LiveClient, key: &str) -> Result<PlaylistPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::playlist(&q, key))?;
    let videos = found
        .videos
        .items
        .into_iter()
        .map(|v| said_video(v, client))
        .collect();
    Ok(PlaylistPage {
        playlist: SaidPlaylist {
            thumbnail: client.held_picture(&best_thumb(&found.thumbnail)),
            key: found.id,
            title: found.name,
            channel_key: found
                .channel
                .as_ref()
                .map_or_else(String::new, |c| c.id.clone()),
            channel_name: found
                .channel
                .as_ref()
                .map_or_else(String::new, |c| c.name.clone()),
            video_count: found.video_count.cast_signed(),
        },
        videos,
    })
}

/// The videos YouTube lists as trending.
pub fn trending(client: &LiveClient) -> Result<TrendingPage, ServiceError> {
    let q = client.query();
    let found = client.block(tilo_innertube::trending(&q))?;
    Ok(TrendingPage {
        videos: found.into_iter().map(|v| said_video(v, client)).collect(),
    })
}

/// The address a video's bytes play from: the progressive stream carrying picture and sound,
/// no taller than `quality` names (`auto` for the best that fits), so a plain HTTP source can
/// stream, seek and reach the end.
pub fn stream(client: &LiveClient, key: &str, quality: &str) -> Result<String, ServiceError> {
    let q = client.query();
    let player = client.block(tilo_innertube::player(&q, key))?;
    tilo_innertube::whole_file_stream(&player.video_streams, height_of(quality))
        .map(|stream| stream.url.clone())
        .ok_or_else(|| ServiceError(format!("YouTube serves no playable stream of {key}")))
}

/// The tallest picture a quality names, as `1080p` reads: `None` for `auto` (or nothing), the
/// best that fits.
#[must_use]
pub fn height_of(quality: &str) -> Option<u32> {
    let height = quality.strip_suffix('p').unwrap_or(quality);
    height.parse().ok().filter(|height| *height > 0)
}

/// A video's comments, most liked first.
pub fn comments(client: &LiveClient, key: &str) -> Result<CommentsPage, ServiceError> {
    let q = client.query();
    let details = client.block(tilo_innertube::video_details(&q, key))?;
    let page = client.block(tilo_innertube::next_comments(&details.top_comments, &q))?;
    let comments = page
        .map(|page| page.items)
        .unwrap_or_default()
        .into_iter()
        .map(|comment| said_comment(client, key, comment))
        .collect();
    Ok(CommentsPage {
        video: key.to_owned(),
        comments,
    })
}

fn said_comment(
    client: &LiveClient,
    video: &str,
    comment: rustypipe::model::Comment,
) -> SaidComment {
    let (author, avatar) = comment.author.map_or_else(
        || (String::new(), String::new()),
        |c| (c.name, client.held_picture(&best_thumb(&c.avatar))),
    );
    SaidComment {
        key: comment.id,
        video_key: video.to_owned(),
        author,
        avatar,
        text: plain(&comment.text),
        likes: i64::from(comment.like_count.unwrap_or(0)),
        published: comment.publish_date.map_or_else(
            || "1970-01-01T00:00:00Z".to_owned(),
            |at| {
                at.to_offset(tilo_innertube::time::UtcOffset::UTC)
                    .format(&tilo_innertube::time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
            },
        ),
        pinned: comment.pinned,
        hearted: comment.hearted,
        replies: i64::from(comment.reply_count),
    }
}

/// `SponsorBlock`'s skip segments for a video.
const SPONSOR_ENDPOINT: &str = "https://sponsor.ajay.app/api/skipSegments";
/// `DeArrow`'s community titles and pictures.
const DEARROW_ENDPOINT: &str = "https://sponsor.ajay.app/api/branding";
/// Return YouTube Dislike's estimates.
const DISLIKE_ENDPOINT: &str = "https://returnyoutubedislikeapi.com/votes";

/// The `SponsorBlock` segments of a video, in the order they play.
pub fn segments(client: &LiveClient, key: &str) -> Result<SegmentPage, ServiceError> {
    let _ = client;
    let categories = sponsor_categories();
    let answer = get_json(
        SPONSOR_ENDPOINT,
        &[("videoID", key), ("categories", &categories)],
    )?;
    let segments = answer
        .map(|value| parse_segments(key, &value))
        .unwrap_or_default();
    Ok(SegmentPage {
        video: key.to_owned(),
        segments,
    })
}

/// The `SponsorBlock` categories a skip asks for: every kind it marks, as its own API names them.
fn sponsor_categories() -> String {
    serde_json::to_string(&[
        "sponsor",
        "selfpromo",
        "interaction",
        "intro",
        "outro",
        "preview",
        "music_offtopic",
        "filler",
    ])
    .unwrap_or_else(|_| "[]".to_owned())
}

/// The segments a `SponsorBlock` answer lists, each kept when its kind and times read, in the
/// order they play.
#[must_use]
pub fn parse_segments(video: &str, answer: &serde_json::Value) -> Vec<SegmentItem> {
    let Some(list) = answer.as_array() else {
        // A video SponsorBlock holds no segment of answers `[]`, and one it does not know a
        // 404 the caller already read as none.
        return Vec::new();
    };
    let mut segments: Vec<SegmentItem> = list
        .iter()
        .filter_map(|segment| {
            let category = segment.get("category")?.as_str()?.to_owned();
            let times = segment.get("segment")?.as_array()?;
            let start = times.first()?.as_f64()?;
            let end = times.get(1)?.as_f64()?;
            Some(SegmentItem {
                video_key: video.to_owned(),
                category,
                start,
                end,
                votes: segment
                    .get("votes")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0),
            })
        })
        .collect();
    segments.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    segments
}

/// `DeArrow`'s community title and picture of a video, as the client's own terms carry them:
/// the picture is fetched and kept as this device's content, like every still.
pub fn branding(client: &LiveClient, key: &str) -> Result<Branding, ServiceError> {
    let answer = get_json(DEARROW_ENDPOINT, &[("videoID", key)])?;
    let Some(answer) = answer else {
        return Ok(Branding {
            video: key.to_owned(),
            ..Branding::default()
        });
    };
    let (title, thumbnail) = parse_branding(key, &answer);
    Ok(Branding {
        video: key.to_owned(),
        title,
        thumbnail: thumbnail.map_or_else(String::new, |url| client.held_picture(&url)),
    })
}

/// What a `DeArrow` answer calls a video: the best-voted title that is not the maker's own, and
/// the timestamp a picture is taken at. A locked entry beats every unlocked one.
#[must_use]
pub fn parse_branding(key: &str, answer: &serde_json::Value) -> (String, Option<String>) {
    fn score(entry: &serde_json::Value) -> Option<i64> {
        if entry
            .get("original")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }
        let votes = entry
            .get("votes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let locked = entry
            .get("locked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if votes < 0 && !locked {
            return None;
        }
        Some(if locked {
            votes.saturating_add(1_000_000)
        } else {
            votes
        })
    }
    let title = answer
        .get("titles")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .filter_map(|entry| Some((score(entry)?, entry)))
                .max_by_key(|(votes, _)| *votes)
                .and_then(|(_, entry)| entry.get("title")?.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let thumbnail = answer
        .get("thumbnails")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .filter_map(|entry| Some((score(entry)?, entry.get("timestamp")?.as_f64()?)))
                .max_by_key(|(votes, _)| *votes)
                .map(|(_, timestamp)| {
                    format!(
                        "https://dearrow-thumb.ajay.app/api/v1/getThumbnail?videoID={key}&time={timestamp}"
                    )
                })
        });
    (title, thumbnail)
}

/// Return YouTube Dislike's estimate for a video.
pub fn votes(client: &LiveClient, key: &str) -> Result<Votes, ServiceError> {
    let _ = client;
    let answer = get_json(DISLIKE_ENDPOINT, &[("videoId", key)])?;
    let votes =
        answer
            .as_ref()
            .and_then(parse_votes)
            .map_or_else(Votes::default, |(dislikes, likes)| Votes {
                video: key.to_owned(),
                dislikes,
                likes,
            });
    Ok(votes)
}

/// What a Return YouTube Dislike answer says of the votes: the dislikes, and the likes when it
/// carries them.
#[must_use]
pub fn parse_votes(answer: &serde_json::Value) -> Option<(i64, Option<i64>)> {
    let dislikes = answer.get("dislikes")?.as_i64()?;
    let likes = answer.get("likes").and_then(serde_json::Value::as_i64);
    Some((dislikes, likes))
}

/// `url` with `query` asked as a query string: every byte outside the unreserved set is
/// percent-encoded, as a category list of JSON needs.
fn queried(url: &str, query: &[(&str, &str)]) -> String {
    fn encoded(text: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(byte));
            } else {
                let high = usize::from(byte >> 4);
                let low = usize::from(byte & 0x0f);
                out.push('%');
                out.push(char::from(*HEX.get(high).unwrap_or(&b'0')));
                out.push(char::from(*HEX.get(low).unwrap_or(&b'0')));
            }
        }
        out
    }
    let mut out = url.to_owned();
    for (at, (key, value)) in query.iter().enumerate() {
        out.push(if at == 0 { '?' } else { '&' });
        out.push_str(&encoded(key));
        out.push('=');
        out.push_str(&encoded(value));
    }
    out
}

/// What `url`, asked with `query`, answers as JSON; `None` when the service holds nothing for
/// it (an HTTP 404).
fn get_json(url: &str, query: &[(&str, &str)]) -> Result<Option<serde_json::Value>, ServiceError> {
    /// One client for the process, as the pictures' is: its connection pool and TLS setup are
    /// the slow part, and a video's page asks three services at once.
    static CLIENT: OnceLock<Option<tilo_net::http::BlockingClient>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        let built = tilo_net::http::blocking_client()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .map_err(|error| tracing::warn!(%error, "no client for YouTube's community services"));
        built.ok()
    });
    let Some(client) = client.as_ref() else {
        return Err(ServiceError(
            "no client for YouTube's community services".to_owned(),
        ));
    };
    let response = client
        .get(queried(url, query))
        .send()
        .map_err(|error| ServiceError(format!("{url}: {error}")))?;
    if response.status().as_u16() == 404 {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|error| ServiceError(format!("{url}: {error}")))?;
    let value: serde_json::Value = response
        .json()
        .map_err(|error| ServiceError(format!("{url}: {error}")))?;
    Ok(Some(value))
}

fn said_video(v: VideoItem, client: &LiveClient) -> SaidVideo {
    let (channel_key, channel_name, channel_avatar) = v.channel.map_or_else(
        || (String::new(), String::new(), String::new()),
        |c| (c.id, c.name, client.held_picture(&best_thumb(&c.avatar))),
    );
    SaidVideo {
        thumbnail: client.held_picture(&thumb_of(&v.id)),
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

fn said_playlist(p: PlaylistItem, client: &LiveClient) -> SaidPlaylist {
    let (channel_key, channel_name) = p
        .channel
        .map_or_else(|| (String::new(), String::new()), |c| (c.id, c.name));
    SaidPlaylist {
        thumbnail: client.held_picture(&best_thumb(&p.thumbnail)),
        key: p.id,
        title: p.name,
        channel_key,
        channel_name,
        video_count: p.video_count.unwrap_or(0).cast_signed(),
    }
}

fn said_channel(c: ChannelItem, client: &LiveClient) -> SaidChannel {
    SaidChannel {
        key: c.id.clone(),
        name: c.name,
        handle: c.handle.unwrap_or_else(|| c.id.clone()),
        subscribers: c.subscriber_count.unwrap_or(0).cast_signed(),
        avatar: client.held_picture(&best_thumb(&c.avatar)),
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
