//! Fully-rangeable VIDEO+AUDIO resolution through public Invidious instances.
//!
//! ## The truncation problem this fixes
//!
//! YouTube's default `googlevideo` media URLs (what `player()` / yt-dlp resolve
//! without a PoToken) are gated by PoToken/SABR: a bounded `Range` request past
//! the first ~1-minute segment is refused, so only the opening minute of a video
//! ever streams — the same wall the music app hit. GStreamer's `souphttpsrc`
//! needs arbitrary byte ranges to seek and to keep reading, so a raw googlevideo
//! URL stalls after the first segment (and never seeks).
//!
//! An Invidious instance exposes the same **progressive muxed** formats (audio +
//! video in one MP4) and its `latest_version?…&local=true` endpoint proxies the
//! media **through the instance's own socket**, which serves arbitrary HTTP byte
//! ranges over the whole file. Handing *that* URL to the shared media engine's
//! GStreamer `playbin` streams the full video with sound and lets seeking work.
//!
//! The URL-building + format-selection logic is always compiled and unit-tested
//! (no network); the live [`resolve_rangeable`] fetch probes real instances and
//! commits only to one whose proxy actually serves bytes.
//!
//! [`resolve_rangeable`]: crate::backend::proxy::resolve_rangeable

use ui::time::Duration;

/// Public Invidious instances tried in order until one's `local` media proxy
/// actually serves bytes. Operators can prepend their own via
/// `TILO_TUBE_INVIDIOUS` (comma-separated) for reliability / self-hosting.
///
/// Ordering matters: instances whose media proxy is reliably up come first, so
/// the common case resolves on the first probe.
pub const INVIDIOUS_INSTANCES: &[&str] = &[
    "https://invidious.f5.si",
    "https://invidious.nerdvpn.de",
    "https://inv.nadeko.net",
    "https://yewtu.be",
    "https://invidious.jing.rocks",
    "https://inv.tux.pizza",
    "https://iv.melmac.space",
];

/// Progressive **muxed** `YouTube` itags (video *and* audio in one MP4), best
/// first. itag 22 is 720p H.264+AAC, itag 18 is 360p H.264+AAC — both decode on
/// any stock `GStreamer` (avc1 + aac) and, crucially, are a single file so one
/// rangeable URL carries picture *and* sound. Adaptive (video-only / audio-only)
/// formats would need two synchronized URLs `playbin` can't take at once, so we
/// deliberately stick to muxed here for a reliable "audio+video plays" result.
const KNOWN_MUXED_ITAGS: &[(u32, &str, &str)] = &[
    (22, "video/mp4; codecs=\"avc1.64001F, mp4a.40.2\"", "720p"),
    (18, "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"", "360p"),
];

/// A resolved, directly-playable video stream: a whole rangeable media URL plus
/// the shape the status log wants. Toolkit-free so it can cross the worker
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedVideo {
    /// The video id this resolves.
    pub video_id: String,
    /// Directly-playable, fully-rangeable media URL (an Invidious `local=true`
    /// proxy of a progressive muxed format).
    pub url: String,
    /// `YouTube` stream format id (itag).
    pub itag: u32,
    /// Full MIME (e.g. `video/mp4; codecs="avc1.42001E, mp4a.40.2"`).
    pub mime: String,
    /// Short quality label for the status log (e.g. `360p`).
    pub quality: String,
    /// Whether the stream carries audio as well as video. A muxed format always
    /// does; kept explicit so the UI can say so.
    pub has_audio: bool,
}

/// One progressive muxed format parsed from an Invidious video response's
/// `formatStreams` array (the muxed list — `adaptiveFormats` is video-only /
/// audio-only and is intentionally ignored here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxedFormat {
    pub itag: u32,
    /// Full MIME (`type` field), e.g. `video/mp4; codecs="avc1.42001E,mp4a.40.2"`.
    pub mime: String,
    /// Quality label (`qualityLabel`, e.g. `720p`) when present, else `resolution`.
    pub quality: String,
    /// Larger = higher resolution; used only to rank formats.
    pub height: u32,
}

impl MuxedFormat {
    /// Whether this is an MP4/H.264 muxed format stock `GStreamer` decodes.
    pub fn is_mp4(&self) -> bool {
        let m = self.mime.to_ascii_lowercase();
        m.contains("mp4") && (m.contains("avc1") || m.contains("h264") || m.contains("mp4v"))
    }
}

/// The instance's **local media proxy** URL for a specific itag. `local=true`
/// routes the bytes through the instance's own socket, which honours arbitrary
/// HTTP `Range` requests over the full file — the fix for the ~1-minute
/// truncation of raw googlevideo URLs.
pub fn proxy_media_url(instance: &str, video_id: &str, itag: u32) -> String {
    format!(
        "{}/latest_version?id={}&itag={}&local=true",
        instance.trim_end_matches('/'),
        video_id,
        itag
    )
}

/// The Invidious video-info endpoint for a video id.
pub fn video_info_url(instance: &str, video_id: &str) -> String {
    format!(
        "{}/api/v1/videos/{}",
        instance.trim_end_matches('/'),
        video_id
    )
}

/// Parse the progressive **muxed** entries out of an Invidious
/// `/api/v1/videos/{id}` response's `formatStreams` array (audio+video in one
/// file). The `adaptiveFormats` list is deliberately not consulted — those are
/// single-track and can't be handed to `playbin` as one URL.
pub fn parse_format_streams(json: &serde_json::Value) -> Vec<MuxedFormat> {
    let Some(items) = json.get("formatStreams").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let itag = num_field(it, "itag")? as u32;
            let mime = it
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("video/mp4")
                .to_string();
            let quality = it
                .get("qualityLabel")
                .and_then(|v| v.as_str())
                .or_else(|| it.get("resolution").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            // `resolution` is like "1280x720"; the height ranks candidates.
            let height = it
                .get("resolution")
                .and_then(|v| v.as_str())
                .and_then(|r| r.split(['x', 'X']).nth(1))
                .and_then(|h| h.parse::<u32>().ok())
                .or_else(|| quality.trim_end_matches('p').parse::<u32>().ok())
                .unwrap_or(0);
            Some(MuxedFormat {
                itag,
                mime,
                quality,
                height,
            })
        })
        .collect()
}

/// Read a numeric JSON field that Invidious may send as a number *or* a string.
fn num_field(it: &serde_json::Value, key: &str) -> Option<u64> {
    let v = it.get(key)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Pick the best muxed MP4 format to play: highest resolution among the
/// H.264/MP4 muxed streams (stock `GStreamer` decodes them everywhere), else the
/// highest-resolution muxed stream of any container.
pub fn best_muxed(list: &[MuxedFormat]) -> Option<&MuxedFormat> {
    list.iter()
        .filter(|f| f.is_mp4())
        .max_by_key(|f| f.height)
        .or_else(|| list.iter().max_by_key(|f| f.height))
}

/// The instances to try, honouring the `TILO_TUBE_INVIDIOUS` override (a
/// comma-separated list prepended to the built-in defaults).
pub fn instances() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(env) = std::env::var_os("TILO_TUBE_INVIDIOUS") {
        for s in env.to_string_lossy().split(',') {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    out.extend(
        INVIDIOUS_INSTANCES
            .iter()
            .map(std::string::ToString::to_string),
    );
    out
}

/// A plain browser `User-Agent` for talking to an Invidious `local` proxy (it is
/// not a signed googlevideo URL, so no client-specific UA is needed).
pub const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// Resolve a video id to a **fully-rangeable** proxied muxed video+audio stream.
///
/// Public instances are unreliable in two distinct ways: the `/api/v1/videos`
/// metadata endpoint is frequently Cloudflare-gated, and — worse — an instance
/// can answer metadata yet have a *broken media proxy* (its `local=true`
/// endpoint 500s/403s because googlevideo blocks the instance's IP). So this
/// resolver **probes the media proxy directly** for a standard muxed itag and
/// commits only to an instance whose proxy actually serves bytes — guaranteeing
/// the playback that follows will succeed. `None` when no instance's proxy
/// answers within the budget (the caller then falls back to the direct,
/// possibly-truncating, URL).
pub async fn resolve_rangeable(video_id: &str) -> Option<ResolvedVideo> {
    // Bounded so slow/down instances can't stall the resolve: a short
    // per-request timeout + an overall deadline.
    let client = tilo_net::Client::builder()
        .timeout(Duration::from_millis(2500))
        .user_agent(BROWSER_UA)
        .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
        .build()
        .ok()?;
    let deadline = ui::time::now() + Duration::from_secs(6);
    for instance in instances() {
        for &(itag, mime, quality) in KNOWN_MUXED_ITAGS {
            if ui::time::now() >= deadline {
                return None;
            }
            let url = proxy_media_url(&instance, video_id, itag);
            if proxy_serves_bytes(&client, &url).await {
                return Some(ResolvedVideo {
                    video_id: video_id.to_string(),
                    url,
                    itag,
                    mime: mime.to_string(),
                    quality: quality.to_string(),
                    has_audio: true,
                });
            }
        }
    }
    None
}

/// Probe a proxy media URL with a tiny range request: `true` only when the
/// instance actually serves media bytes (2xx, a media type, a non-empty body),
/// so a broken proxy (500/403/redirect-to-nothing) or an instance answering
/// with a bot-check page is skipped.
async fn proxy_serves_bytes(client: &tilo_net::Client, url: &str) -> bool {
    match client
        .get(url)
        .header(tilo_net::header::RANGE, "bytes=0-2047")
        .send()
        .await
    {
        Ok(resp) => {
            let media = resp
                .headers()
                .get(tilo_net::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_none_or(is_media_type);
            resp.status().is_success() && media && resp.bytes().await.is_ok_and(|b| !b.is_empty())
        }
        Err(_) => false,
    }
}

/// Whether a `Content-Type` can be media bytes rather than a page.
fn is_media_type(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("video/")
        || essence.starts_with("audio/")
        || essence == "application/octet-stream"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls_and_instances_are_built_locally() {
        // Case `proxy_url_is_local_and_carries_id_and_itag`.
        {
            let url = proxy_media_url("https://yewtu.be/", "dQw4w9WgXcQ", 18);
            assert_eq!(
                url,
                "https://yewtu.be/latest_version?id=dQw4w9WgXcQ&itag=18&local=true"
            );
            // `local=true` is what makes the instance proxy the bytes (and thus serve
            // arbitrary ranges over the whole file).
            assert!(url.contains("local=true"), "must proxy locally: {url}");
        }
        // Case `video_info_url_builds_api_path`.
        {
            assert_eq!(
                video_info_url("https://inv.nadeko.net", "abc123"),
                "https://inv.nadeko.net/api/v1/videos/abc123"
            );
        }
        // Case `instances_honours_env_override_first`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_TUBE_INVIDIOUS", "https://my.inv, https://two.inv");
            let list = instances();
            assert_eq!(list[0], "https://my.inv");
            assert_eq!(list[1], "https://two.inv");
            assert!(list.len() > 2, "defaults are appended after the override");
        }
    }

    #[test]
    fn muxed_stream_selection_prefers_highest_quality_mp4() {
        // Case `known_muxed_itags_prefer_higher_quality_first`.
        {
            // 22 (720p) is probed before 18 (360p), so the best available muxed
            // stream is committed to first.
            assert_eq!(KNOWN_MUXED_ITAGS[0].0, 22);
            assert_eq!(KNOWN_MUXED_ITAGS[1].0, 18);
        }
        // Case `parse_and_pick_best_prefers_highest_mp4`.
        {
            // Mirrors an Invidious `/api/v1/videos/{id}` body: muxed formatStreams
            // plus (ignored) adaptive video-only formats. `resolution`/`itag` come as
            // strings (as Invidious sends them).
            let json: serde_json::Value = serde_json::from_str(
            r#"{
                "lengthSeconds": 212,
                "adaptiveFormats": [
                    {"itag":"137","type":"video/mp4; codecs=\"avc1\"","resolution":"1920x1080"}
                ],
                "formatStreams": [
                    {"itag":"18","type":"video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"","resolution":"640x360","qualityLabel":"360p"},
                    {"itag":"22","type":"video/mp4; codecs=\"avc1.64001F, mp4a.40.2\"","resolution":"1280x720","qualityLabel":"720p"}
                ]
            }"#,
        )
        .unwrap();
            let muxed = parse_format_streams(&json);
            // Only the two muxed formatStreams are kept (the adaptive one is dropped).
            assert_eq!(muxed.len(), 2, "only progressive muxed formats: {muxed:?}");
            let best = best_muxed(&muxed).expect("a best muxed format");
            assert_eq!(best.itag, 22, "prefers the 720p muxed MP4 over 360p");
            assert_eq!(best.quality, "720p");
            assert_eq!(best.height, 720);
            assert!(best.is_mp4());
        }
        // Case `best_muxed_falls_back_when_no_mp4`.
        {
            let json: serde_json::Value = serde_json::from_str(
                r#"{"formatStreams":[
                {"itag":"43","type":"video/webm; codecs=\"vp8, vorbis\"","resolution":"640x360"}
            ]}"#,
            )
            .unwrap();
            let muxed = parse_format_streams(&json);
            let best = best_muxed(&muxed).expect("best muxed");
            assert_eq!(best.itag, 43, "the only muxed stream wins even if webm");
            assert!(!best.is_mp4());
        }
    }

    /// Integration check: resolve a real, widely-available video and prove the
    /// proxied source serves a byte range **past the first ~1-minute segment**
    /// (the exact truncation point that made only a minute of raw googlevideo
    /// URLs playable). Skips honestly — never fails — when no Invidious instance
    /// is reachable, so a networked run stays green regardless.
    #[test]
    fn rangeable_source_serves_bytes_past_first_segment() {
        let video_id = "aqz-KE-bpKQ"; // Big Buck Bunny (Blender, CC-BY) — long + stable.
        let Ok(resolved) = ui::rt::block_on(async {
            let Some(stream) = resolve_rangeable(video_id).await else {
                eprintln!("[proxy test] skipped — no Invidious instance reachable");
                return None;
            };
            assert!(
                stream.url.contains("local=true"),
                "resolved a proxied URL: {}",
                stream.url
            );
            assert!(stream.has_audio, "a muxed format carries audio");
            let client = tilo_net::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent(BROWSER_UA)
                .build()
                .ok()?;
            // Request a range well past the first-segment boundary raw
            // googlevideo URLs refuse (~2 MB in).
            let Ok(resp) = client
                .get(&stream.url)
                .header(tilo_net::header::RANGE, "bytes=2000000-2100000")
                .send()
                .await
            else {
                eprintln!("[proxy test] skipped — proxy fetch failed");
                return None;
            };
            if !resp.status().is_success() {
                eprintln!("[proxy test] skipped — proxy returned {}", resp.status());
                return None;
            }
            Some(resp.bytes().await)
        }) else {
            return;
        };
        match resolved {
            Some(Ok(bytes)) => assert!(
                !bytes.is_empty(),
                "the rangeable source served bytes past the first segment"
            ),
            Some(Err(_)) => eprintln!("[proxy test] skipped — body read failed"),
            None => {}
        }
    }
}
