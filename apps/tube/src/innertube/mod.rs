//! `YouTube`'s own public web API (`InnerTube`), read through `tilo_net`.
//!
//! The API `YouTube`'s web and mobile apps call: `POST
//! https://www.youtube.com/youtubei/v1/{search,next,player,browse}` with a
//! JSON body naming the calling client. `NewPipe` and yt-dlp read the same
//! endpoints. It answers in well under a second and needs no third-party
//! server, so it is Tube's first source wherever the worker runs without the
//! extractors (a page) and the last one behind them on the desktop; public
//! Invidious instances are the fallback (see [`crate::api`]). In a page the
//! requests go through the host's fetch proxy: youtube.com sends no CORS
//! headers.
//!
//! Which client asks matters:
//!
//! - `WEB` answers search, watch-next and channel pages, with the richest
//!   rows.
//! - `ANDROID` answers the player request with stream URLs that need no
//!   signature deciphering (the `WEB` client's need `YouTube`'s player
//!   script) and that serve every byte range, so the media element can seek.
//!   The progressive stream (picture and sound in one file) is 360p.
//! - Requests carry the visitor id an earlier answer handed out, as the apps
//!   do.
//!
//! A stream URL is bound to the address that asked for it, so the media
//! element must fetch it the way the player request went: in a page, through
//! the same proxy, which `tilo-media` falls back to when the direct load
//! fails.

pub mod parse;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use ui::lock::MutexRecover as _;

use crate::model::{Channel, Video};
pub use parse::{Found, Stream};

const API: &str = "https://www.youtube.com/youtubei/v1";
/// A request that takes longer than this gives way to the fallback: search
/// should answer within about two seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);
/// Search filter: videos only.
const VIDEOS_ONLY: &str = "EgIQAQ%3D%3D";
/// Search filter: videos uploaded this week, most viewed first.
const POPULAR_THIS_WEEK: &str = "CAMSBAgDEAE%3D";
/// Channel tab: its videos, newest first.
const CHANNEL_VIDEOS: &str = "EgZ2aWRlb3PyBgQKAjoA";
/// Player answers kept for playback (stream URLs last hours).
const KEPT_STREAMS: usize = 16;

/// Whether a kept stream at `quality` answers a request for `wanted`: the
/// same size, or — for a quality nobody can be under — any size at all, so a
/// source with one small stream still plays rather than re-resolving forever.
fn covers(wanted: Option<u32>, quality: &str) -> bool {
    let Some(wanted) = wanted else {
        return true;
    };
    quality
        .trim_end_matches('p')
        .parse::<u32>()
        .is_ok_and(|height| height <= wanted)
}

/// `YouTube` answered the player request but would not play the video,
/// in its own words (such as "Sign in to confirm you're not a bot"): no
/// other source is likely to do better, and the person should hear why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "YouTube refused: {}", self.0)
    }
}

impl std::error::Error for Refused {}

/// What `YouTube` said when it refused to play, if the error is that.
#[must_use]
pub fn refusal(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<Refused>()
        .map(|refused| refused.0.clone())
}

/// A client `YouTube` knows, as the request names it.
#[derive(Clone, Copy, Debug)]
enum Caller {
    Web,
    Android,
}

impl Caller {
    fn context(self, visitor: Option<&str>) -> Value {
        let mut client = match self {
            Self::Web => json!({
                "clientName": "WEB",
                "clientVersion": "2.20251020.01.00",
            }),
            Self::Android => json!({
                "clientName": "ANDROID",
                "clientVersion": "20.10.38",
                "androidSdkVersion": 30,
                "osName": "Android",
                "osVersion": "11",
            }),
        };
        if let Some(fields) = client.as_object_mut() {
            fields.insert("hl".into(), "en".into());
            fields.insert("gl".into(), "US".into());
            if let Some(visitor) = visitor {
                fields.insert("visitorData".into(), visitor.into());
            }
        }
        json!({ "client": client })
    }

    /// The number `YouTube` gives this client, in `x-youtube-client-name`.
    fn number(self) -> &'static str {
        match self {
            Self::Web => "1",
            Self::Android => "3",
        }
    }

    fn user_agent(self) -> &'static str {
        match self {
            Self::Web => {
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/141.0 Safari/537.36"
            }
            Self::Android => "com.google.android.youtube/20.10.38 (Linux; U; Android 11) gzip",
        }
    }
}

/// An `InnerTube` reader: one per worker, so the visitor id and the streams
/// a watch page already resolved carry over between requests.
pub struct InnerTube {
    client: tilo_net::Client,
    /// Where the API answers: [`API`], or a test's fixture server.
    api: String,
    visitor: Mutex<Option<String>>,
    streams: Mutex<HashMap<String, Stream>>,
}

impl InnerTube {
    pub fn new(client: tilo_net::Client) -> Self {
        Self::at(client, API)
    }

    /// A reader asking the API at `api` instead of `YouTube`'s.
    fn at(client: tilo_net::Client, api: &str) -> Self {
        Self {
            client,
            api: api.trim_end_matches('/').to_string(),
            visitor: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// Videos and creators matching `query`.
    pub async fn search(&self, query: &str) -> anyhow::Result<Found> {
        let answer = self
            .call(
                "search",
                Caller::Web,
                json!({ "query": query, "params": VIDEOS_ONLY }),
            )
            .await?;
        Ok(parse::search(&answer, "Search"))
    }

    /// The week's most-viewed videos about `topic`.
    ///
    /// # Errors
    ///
    /// As [`InnerTube::search`].
    pub async fn popular(&self, topic: &str) -> anyhow::Result<Vec<Video>> {
        let answer = self
            .call(
                "search",
                Caller::Web,
                json!({ "query": topic, "params": POPULAR_THIS_WEEK }),
            )
            .await?;
        Ok(parse::search(&answer, "Popular").videos)
    }

    /// A video's watch page and what it recommends next. The player answer
    /// asked on the way names its stream, which playback then reuses.
    pub async fn video(&self, id: &str) -> anyhow::Result<(Video, Vec<Video>)> {
        let player = self.player(id).await.ok();
        let next = self
            .call("next", Caller::Web, json!({ "videoId": id }))
            .await?;
        parse::watch(&next, player.as_ref(), id)
            .with_context(|| format!("the watch page of {id} names no video"))
    }

    /// A stream the media element plays for `id`.
    ///
    /// `wanted` is the height the person chose (`Some(480)` for 480p), which
    /// decides which of the streams the answer names is served: the kept
    /// stream only answers when it is the size asked for, so a choice of a
    /// smaller size is not silently ignored.
    pub async fn stream(&self, id: &str, wanted: Option<u32>) -> anyhow::Result<Stream> {
        if let Some(stream) = self.kept_stream(id)
            && covers(wanted, &stream.quality)
        {
            return Ok(stream);
        }
        let player = self.player(id).await?;
        if let Some(reason) = parse::refusal(&player) {
            return Err(Refused(reason).into());
        }
        parse::stream(&player, wanted).map_err(|reason| anyhow::anyhow!("{id}: {reason}"))
    }

    /// The caption tracks `id` offers, from its player answer: each one's
    /// language, what it calls itself, and the timed-text URL that serves it.
    /// An empty answer means the video has no captions, which is not an error.
    pub async fn caption_tracks(
        &self,
        id: &str,
    ) -> anyhow::Result<Vec<crate::captions::CaptionTrack>> {
        let player = self.player(id).await?;
        Ok(parse::caption_tracks(&player))
    }

    /// A channel's page and its newest uploads.
    pub async fn channel(&self, id: &str) -> anyhow::Result<(Channel, Vec<Video>)> {
        let answer = self
            .call(
                "browse",
                Caller::Web,
                json!({ "browseId": id, "params": CHANNEL_VIDEOS }),
            )
            .await?;
        parse::channel(&answer, id).with_context(|| format!("{id} is not a channel page"))
    }

    /// The player answer for `id`, keeping the stream it names.
    async fn player(&self, id: &str) -> anyhow::Result<Value> {
        let player = self
            .call(
                "player",
                Caller::Android,
                json!({ "videoId": id, "contentCheckOk": true, "racyCheckOk": true }),
            )
            .await?;
        if let Ok(stream) = parse::stream(&player, None) {
            let mut streams = self.streams.lock_recover();
            if streams.len() >= KEPT_STREAMS {
                streams.clear();
            }
            streams.insert(id.to_string(), stream);
        }
        Ok(player)
    }

    fn kept_stream(&self, id: &str) -> Option<Stream> {
        self.streams.lock_recover().get(id).cloned()
    }

    /// POST one `InnerTube` request as `caller`, and remember the visitor id
    /// the answer hands out.
    async fn call(&self, endpoint: &str, caller: Caller, mut body: Value) -> anyhow::Result<Value> {
        let visitor = self.visitor.lock_recover().clone();
        if let Some(fields) = body.as_object_mut() {
            fields.insert("context".into(), caller.context(visitor.as_deref()));
        }
        let mut request = self
            .client
            .post(format!("{}/{endpoint}?prettyPrint=false", self.api))
            .timeout(REQUEST_TIMEOUT)
            .header(tilo_net::header::USER_AGENT, caller.user_agent())
            .header("x-youtube-client-name", caller.number())
            .json(&body);
        if let Some(visitor) = &visitor {
            request = request.header("x-goog-visitor-id", visitor.as_str());
        }
        let answer: Value = request
            .send()
            .await
            .with_context(|| format!("YouTube {endpoint}"))?
            .error_for_status()
            .with_context(|| format!("YouTube {endpoint}"))?
            .json()
            .await
            .with_context(|| format!("YouTube {endpoint} answer"))?;
        if let Some(fresh) = parse::visitor(&answer) {
            *self.visitor.lock_recover() = Some(fresh);
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod client_tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    /// One request as the fixture server read it.
    struct Asked {
        line: String,
        head: String,
        body: Value,
    }

    /// Answer each connection with the next of `answers` (status, body),
    /// reporting what was asked.
    fn serve(answers: Vec<(u16, String)>) -> (String, mpsc::Receiver<Asked>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a fixture port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().expect("a request");
                let mut read = Vec::new();
                let mut chunk = [0_u8; 4096];
                let head_end = loop {
                    let count = stream.read(&mut chunk).expect("read the request");
                    assert!(count > 0, "the client closed early");
                    read.extend_from_slice(&chunk[..count]);
                    if let Some(at) = read.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at.saturating_add(4);
                    }
                };
                let head = String::from_utf8_lossy(&read[..head_end]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                while read.len() < head_end.saturating_add(length) {
                    let count = stream.read(&mut chunk).expect("read the body");
                    assert!(count > 0, "the client closed mid-body");
                    read.extend_from_slice(&chunk[..count]);
                }
                let line = head.lines().next().unwrap_or_default().to_string();
                let asked_body = serde_json::from_slice(read.get(head_end..).unwrap_or_default())
                    .unwrap_or(Value::Null);
                let _ = tx.send(Asked {
                    line,
                    head,
                    body: asked_body,
                });
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("answer");
            }
        });
        (base, rx)
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        ui::rt::app()
            .block_on(future)
            .expect("the app runtime runs")
    }

    fn reader(base: &str) -> InnerTube {
        let client = tilo_net::Client::builder().build().expect("a client");
        InnerTube::at(client, base)
    }

    #[test]
    fn search_asks_as_the_web_client_and_carries_the_visitor_id_after() {
        let search = include_str!("fixtures/search.json").to_string();
        let (base, asked) = serve(vec![(200, search.clone()), (200, search)]);
        let youtube = reader(&base);

        let found = block(youtube.search("nbc news")).expect("an answer");
        assert_eq!(found.videos.len(), 4);
        let first = asked.recv().expect("the search request");
        assert!(
            first.line.starts_with("post /search?prettyprint=false"),
            "{}",
            first.line
        );
        assert!(first.head.contains("x-youtube-client-name: 1"));
        assert_eq!(first.body["query"], "nbc news");
        assert_eq!(first.body["context"]["client"]["clientName"], "WEB");
        assert!(first.body["context"]["client"].get("visitorData").is_none());

        block(youtube.popular("news")).expect("an answer");
        let second = asked.recv().expect("the popular request");
        assert_eq!(
            second.body["context"]["client"]["visitorData"],
            "CgtleGFtcGxlVmlzaXRvcg%3D%3D"
        );
        assert!(
            second
                .head
                .contains("x-goog-visitor-id: cgtlegftcgxlvmlzaxrvcg%3d%3d")
        );
    }

    #[test]
    fn a_watch_page_keeps_the_stream_its_player_answer_named() {
        let (base, asked) = serve(vec![
            (200, include_str!("fixtures/player.json").to_string()),
            (200, include_str!("fixtures/next.json").to_string()),
        ]);
        let youtube = reader(&base);
        let (video, related) = block(youtube.video("n61ULEU7CO0")).expect("a watch page");
        assert_eq!(video.duration, 22_258);
        assert_eq!(related.len(), 3);
        let player = asked.recv().expect("the player request");
        assert!(player.line.starts_with("post /player"));
        assert_eq!(player.body["context"]["client"]["clientName"], "ANDROID");
        assert!(
            player
                .head
                .contains("user-agent: com.google.android.youtube/")
        );
        assert!(
            asked
                .recv()
                .expect("the next request")
                .line
                .starts_with("post /next")
        );

        // Playback asks nothing more: the server has no answers left.
        let stream = block(youtube.stream("n61ULEU7CO0", None)).expect("the kept stream");
        assert_eq!(stream.quality, "360p");
    }

    #[test]
    fn a_refused_player_answer_is_an_error_naming_why() {
        let (base, _asked) = serve(vec![(
            200,
            include_str!("fixtures/player-bot-check.json").to_string(),
        )]);
        let error = block(reader(&base).stream("n61ULEU7CO0", None)).expect_err("a bot check");
        assert_eq!(
            refusal(&error).as_deref(),
            Some("Sign in to confirm you\u{2019}re not a bot"),
            "{error:#}"
        );
    }

    #[test]
    fn a_server_error_is_an_error() {
        let (base, _asked) = serve(vec![(500, "{}".into())]);
        assert!(block(reader(&base).search("x")).is_err());
    }
}
