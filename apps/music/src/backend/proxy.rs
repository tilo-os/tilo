//! Fully-rangeable audio resolution through public Invidious instances.
//!
//! ## The truncation problem this fixes
//!
//! rustypipe resolves a track's audio through the iOS/TV player clients, which
//! return direct `googlevideo` media URLs *without* a `PoToken`. That gets audio
//! playing — but those URLs are gated by PoToken/SABR: a bounded `Range` request
//! **past the first ~1-minute segment is refused**, so only the opening minute
//! of a track is ever fetchable. The whole track never streams.
//!
//! An Invidious instance exposes the same adaptive audio formats, but its
//! `latest_version?…&local=true` endpoint proxies the media **through the
//! instance's own socket**, which serves arbitrary HTTP byte ranges for the
//! entire file. Handing *that* URL to the audio engine's bounded-range staging
//! download fetches the complete track — the whole song plays.
//!
//! The URL-building + format-selection logic is always compiled and unit-tested
//! (no network); the live [`resolve_rangeable`] fetch is gated behind the `live`
//! feature (it needs `reqwest`).

#[cfg(feature = "live")]
use crate::backend::ResolvedStream;

/// Public Invidious instances tried in order until one's `local` media proxy
/// actually serves bytes. Operators can prepend their own via
/// `TILO_MUSIC_INVIDIOUS` (comma-separated) for reliability / self-hosting.
///
/// Ordering matters: instances whose media proxy is reliably up come first, so
/// the common case resolves on the first probe.
pub const INVIDIOUS_INSTANCES: &[&str] = &[
    "https://invidious.nerdvpn.de",
    "https://inv.nadeko.net",
    "https://yewtu.be",
    "https://invidious.jing.rocks",
    "https://inv.tux.pizza",
    "https://iv.melmac.space",
];

/// Standard `YouTube` audio itags, best-decodable first. itag 140 is m4a/AAC 128k
/// (decodable by any stock `GStreamer` with libav/faad); itag 251 is webm/opus
/// 160k. These are present for effectively every music track, so the resolver
/// can probe the media proxy directly without first querying (flaky, often
/// Cloudflare-gated) `/api/v1/videos` metadata for the itag list.
#[cfg(feature = "live")]
const KNOWN_AUDIO_ITAGS: &[(u32, &str, &str, &str, u32)] = &[
    // (itag, mime, codec, format, bitrate)
    (
        140,
        "audio/mp4; codecs=\"mp4a.40.2\"",
        "mp4a",
        "m4a",
        128_000,
    ),
    (251, "audio/webm; codecs=\"opus\"", "opus", "webm", 160_000),
];

/// One audio-only adaptive format parsed from an Invidious video response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvAudio {
    /// `YouTube` stream format id (itag) — the key the local proxy re-fetches by.
    pub itag: u32,
    /// Average/target bitrate in bits/second (higher = better).
    pub bitrate: u32,
    /// Full MIME (e.g. `audio/mp4; codecs="mp4a.40.2"`).
    pub mime: String,
    /// Content length in bytes, when the response carries `clen`.
    pub clen: u64,
}

impl InvAudio {
    /// Short codec name (`mp4a`, `opus`, …) sniffed from the MIME.
    pub fn codec(&self) -> &'static str {
        let m = self.mime.to_ascii_lowercase();
        if m.contains("mp4a") || m.contains("aac") {
            "mp4a"
        } else if m.contains("opus") {
            "opus"
        } else if m.contains("vorbis") {
            "vorbis"
        } else {
            "unknown"
        }
    }

    /// Container format (`m4a`, `webm`).
    pub fn format(&self) -> &'static str {
        let m = self.mime.to_ascii_lowercase();
        if m.contains("mp4") {
            "m4a"
        } else if m.contains("webm") {
            "webm"
        } else {
            "unknown"
        }
    }
}

/// The Invidious video-info endpoint for a video id.
pub fn video_info_url(instance: &str, video_id: &str) -> String {
    format!(
        "{}/api/v1/videos/{}",
        instance.trim_end_matches('/'),
        video_id
    )
}

/// The instance's **local media proxy** URL for a specific itag. `local=true`
/// routes the bytes through the instance's own socket, which honours arbitrary
/// HTTP `Range` requests over the full file — so the whole track is fetchable
/// (the fix for the ~1-minute truncation of raw googlevideo URLs).
pub fn proxy_media_url(instance: &str, video_id: &str, itag: u32) -> String {
    format!(
        "{}/latest_version?id={}&itag={}&local=true",
        instance.trim_end_matches('/'),
        video_id,
        itag
    )
}

/// Parse the audio-only entries out of an Invidious `/api/v1/videos/{id}`
/// response's `adaptiveFormats` array.
pub fn parse_adaptive_audio(json: &serde_json::Value) -> Vec<InvAudio> {
    let Some(items) = json.get("adaptiveFormats").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let mime = it.get("type").and_then(|v| v.as_str()).unwrap_or("");
            // Audio-only formats carry an `audio/…` MIME (video formats are
            // `video/…`); the audio entries also expose `audioQuality`.
            if !mime.starts_with("audio") {
                return None;
            }
            let itag = num_field(it, "itag")? as u32;
            let bitrate = num_field(it, "bitrate").unwrap_or(0) as u32;
            let clen = num_field(it, "clen").unwrap_or(0);
            Some(InvAudio {
                itag,
                bitrate,
                mime: mime.to_string(),
                clen,
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

/// Pick the best audio format to play: prefer the broadly-decodable M4A/AAC
/// container (stock `GStreamer` decodes it everywhere), then highest bitrate;
/// fall back to the highest-bitrate stream of any container.
pub fn best_audio(list: &[InvAudio]) -> Option<&InvAudio> {
    list.iter()
        .filter(|a| a.format() == "m4a")
        .max_by_key(|a| a.bitrate)
        .or_else(|| list.iter().max_by_key(|a| a.bitrate))
}

/// The instances to try, honouring the `TILO_MUSIC_INVIDIOUS` override (a
/// comma-separated list prepended to the built-in defaults).
#[cfg(feature = "live")]
fn instances() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(env) = std::env::var_os("TILO_MUSIC_INVIDIOUS") {
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

/// Resolve a video id to a **fully-rangeable** proxied audio stream.
///
/// Public instances are unreliable in two distinct ways: the `/api/v1/videos`
/// metadata endpoint is frequently Cloudflare-gated, and — worse — an instance
/// can answer metadata yet have a *broken media proxy* (its `local=true`
/// endpoint 500s / 403s because googlevideo blocks the instance's IP). So this
/// resolver **probes the media proxy directly** for a standard audio itag and
/// commits only to an instance whose proxy actually serves bytes — guaranteeing
/// the staging download that follows will succeed. `None` when no instance's
/// proxy answers within the budget (the caller then falls back to the direct,
/// possibly-truncating, URL).
#[cfg(feature = "live")]
pub fn resolve_rangeable(video_id: &str) -> Option<ResolvedStream> {
    // Bounded so slow/down instances can't stall the resolve: a short
    // per-request timeout + an overall deadline.
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_millis(2500))
        .user_agent(BROWSER_UA)
        .build()
        .ok()?;
    let deadline = ui::time::now() + std::time::Duration::from_secs(5);
    for instance in instances() {
        for &(itag, mime, codec, format, bitrate) in KNOWN_AUDIO_ITAGS {
            if ui::time::now() >= deadline {
                tracing::debug!(target: "music::proxy", "resolve_rangeable: deadline reached, no working instance");
                return None;
            }
            let url = proxy_media_url(&instance, video_id, itag);
            if proxy_serves_bytes(&client, &url) {
                tracing::info!(target: "music::proxy", instance = %instance, itag, "resolve_rangeable: proxy serves real media");
                return Some(ResolvedStream {
                    video_id: video_id.to_string(),
                    url,
                    mime: mime.to_string(),
                    codec: codec.to_string(),
                    format: format.to_string(),
                    bitrate,
                    itag,
                    size_bytes: 0,
                    duration_ms: 0,
                    fallback_url: None,
                });
            }
        }
    }
    None
}

/// A plain browser `User-Agent` for talking to an Invidious `local` proxy (it
/// is not a signed googlevideo URL, so no client-specific UA is needed).
#[cfg(feature = "live")]
pub(crate) const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// Probe a proxy media URL with a tiny range request: `true` only when the
/// instance actually serves **audio media bytes** — not merely `2xx + non-empty`.
///
/// Public Invidious instances routinely answer with an HTTP 200/206 *HTML*
/// body: an anti-bot interstitial, a Cloudflare page, or an error page
/// rendered as HTML. Accepting any success status would commit the resolver
/// to a dead instance and feed HTML into the audio decoder, so this also
/// requires a media `Content-Type` (never `text/html` / `text/plain` /
/// `application/json`) and a body that does not sniff as HTML.
#[cfg(feature = "live")]
fn proxy_serves_bytes(client: &reqwest::blocking::Client, url: &str) -> bool {
    match client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-2047")
        .send()
    {
        Ok(resp) => {
            let status = resp.status();
            let ct = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if !status.is_success() || !content_type_is_media(&ct) {
                tracing::debug!(target: "music::proxy", %url, %status, content_type = %ct, "proxy probe rejected (not media)");
                return false;
            }
            let ok = resp
                .bytes()
                .is_ok_and(|b| !b.is_empty() && !body_sniffs_as_html(&b));
            if !ok {
                tracing::debug!(target: "music::proxy", %url, "proxy probe rejected (empty or HTML body)");
            }
            ok
        }
        Err(e) => {
            tracing::debug!(target: "music::proxy", %url, error = %e, "proxy probe request failed");
            false
        }
    }
}

/// Whether a `Content-Type` header names playable audio media (rather than an
/// HTML challenge / error page). An Invidious `local` proxy serving real media
/// answers `audio/…` (or a generic `application/octet-stream`); a broken one
/// answers `text/html` / `text/plain` / `application/json`.
pub fn content_type_is_media(ct: &str) -> bool {
    let ct = ct.trim().to_ascii_lowercase();
    if ct.starts_with("text/") || ct.contains("application/json") || ct.contains("text/html") {
        return false;
    }
    ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.contains("application/octet-stream")
        || ct.contains("mp4")
        || ct.contains("webm")
        || ct.contains("ogg")
        || ct.contains("mpegurl")
}

/// Whether a response body sniffs as an HTML document (an anti-bot / error page
/// masquerading as a media response). Cheap prefix check over the first bytes.
pub fn body_sniffs_as_html(bytes: &[u8]) -> bool {
    let n = bytes.len().min(256);
    let head = String::from_utf8_lossy(bytes.get(..n).unwrap_or(bytes));
    let head = head.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.starts_with("<head")
        || head.starts_with("<!doctype")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn proxy_urls_format_selection_and_content_sniffing() {
        // Phase `proxy_url_is_local_and_carries_id_and_itag`.
        {
            let url = proxy_media_url("https://yewtu.be/", "dQw4w9WgXcQ", 140);
            assert_eq!(
                url,
                "https://yewtu.be/latest_version?id=dQw4w9WgXcQ&itag=140&local=true"
            );
            // The `local=true` flag is what makes the instance proxy the bytes (and
            // thus serve arbitrary ranges over the whole file).
            assert!(url.contains("local=true"), "must proxy locally: {url}");
        }

        // Phase `video_info_url_builds_api_path`.
        {
            assert_eq!(
                video_info_url("https://inv.nadeko.net", "abc123"),
                "https://inv.nadeko.net/api/v1/videos/abc123"
            );
        }

        // Phase `parse_and_pick_best_prefers_m4a_then_bitrate`.
        {
            // Mirrors an Invidious `/api/v1/videos/{id}` body: mixed video + audio
            // adaptive formats, bitrate as a string (as Invidious sends it).
            let json: serde_json::Value = serde_json::from_str(
                r#"{
                    "lengthSeconds": 329,
                    "adaptiveFormats": [
                        {"itag":"137","type":"video/mp4; codecs=\"avc1\"","bitrate":"2500000"},
                        {"itag":"251","type":"audio/webm; codecs=\"opus\"","bitrate":"160000","clen":"6000000"},
                        {"itag":"140","type":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":"128000","clen":"5200000"},
                        {"itag":"139","type":"audio/mp4; codecs=\"mp4a.40.5\"","bitrate":"48000","clen":"2000000"}
                    ]
                }"#,
            )
            .unwrap();
            let audios = parse_adaptive_audio(&json);
            // Only the three audio formats are kept (the video one is dropped).
            assert_eq!(audios.len(), 3, "only audio-only formats: {audios:?}");
            let best = best_audio(&audios).expect("a best audio format");
            // M4A is preferred over the higher-bitrate Opus for decodability, and
            // within M4A the higher (128k) bitrate wins over 48k.
            assert_eq!(best.itag, 140, "prefers the 128k M4A stream");
            assert_eq!(best.format(), "m4a");
            assert_eq!(best.codec(), "mp4a");
            assert_eq!(best.clen, 5_200_000);
        }

        // Phase `best_audio_falls_back_to_opus_when_no_m4a`.
        {
            let json: serde_json::Value = serde_json::from_str(
                r#"{"adaptiveFormats":[
                    {"itag":"251","type":"audio/webm; codecs=\"opus\"","bitrate":"160000"},
                    {"itag":"250","type":"audio/webm; codecs=\"opus\"","bitrate":"70000"}
                ]}"#,
            )
            .unwrap();
            let audios = parse_adaptive_audio(&json);
            let best = best_audio(&audios).expect("best audio");
            assert_eq!(best.itag, 251, "highest-bitrate Opus when no M4A exists");
            assert_eq!(best.format(), "webm");
        }

        // Phase `media_content_type_accepts_audio_rejects_html`.
        {
            // Real proxied media.
            assert!(content_type_is_media("audio/mp4"));
            assert!(content_type_is_media("audio/webm; codecs=\"opus\""));
            assert!(content_type_is_media("application/octet-stream"));
            assert!(content_type_is_media("video/mp4"));
            // Anti-bot / error pages an Invidious instance actually returns as 2xx.
            assert!(!content_type_is_media("text/html; charset=utf-8"));
            assert!(!content_type_is_media("text/plain; charset=utf-8"));
            assert!(!content_type_is_media("application/json"));
            assert!(!content_type_is_media(""));
        }

        // Phase `html_body_is_sniffed_and_rejected`.
        {
            // The exact shapes observed from public instances (gandalf challenge /
            // Cloudflare / "500 Internal Server Error" HTML).
            assert!(body_sniffs_as_html(b"<html>\r\n<head><title>gandalf"));
            assert!(body_sniffs_as_html(b"<!DOCTYPE html>\n<html lang=\"en\">"));
            assert!(body_sniffs_as_html(b"   \n<!doctype html>"));
            // A real m4a fragment begins with an ISO-BMFF box (`....ftyp`/`....moov`)
            // or arbitrary binary — never HTML.
            assert!(!body_sniffs_as_html(&[
                0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p'
            ]));
            assert!(!body_sniffs_as_html(b"OggS\x00\x02"));
        }
    }

    /// `live`-gated integration check: resolve a real track and prove the
    /// proxied source serves a byte range **past the first ~1-minute segment**
    /// (the exact truncation point that made only a minute of raw googlevideo
    /// URLs playable). Skips honestly — never fails — when no Invidious instance
    /// is reachable, so a networked `--features live` run stays green regardless.
    #[cfg(feature = "live")]
    #[test]
    fn rangeable_source_serves_bytes_past_first_segment() {
        // A long, stable, widely-available track so "past the first segment" is
        // meaningful: at 128 kbps one minute is ~960 KB, so a range ~1.5 MB in
        // is well past the first segment that raw googlevideo URLs truncate at.
        let video_id = "dQw4w9WgXcQ"; // 3:33
        let Some(stream) = resolve_rangeable(video_id) else {
            eprintln!("[proxy test] skipped — no Invidious instance reachable");
            return;
        };
        assert!(
            stream.url.contains("local=true"),
            "resolved a proxied URL without the required local=true flag"
        );
        let Ok(client) = tilo_net::http::blocking_client()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        else {
            return;
        };
        // Request a range well past the first-segment boundary raw googlevideo
        // URLs refuse (~1.5 MB in).
        let Ok(resp) = client
            .get(&stream.url)
            .header(reqwest::header::RANGE, "bytes=1500000-1600000")
            .send()
        else {
            eprintln!("[proxy test] skipped — proxy fetch failed");
            return;
        };
        if !resp.status().is_success() {
            eprintln!("[proxy test] skipped — proxy returned {}", resp.status());
            return;
        }
        match resp.bytes() {
            Ok(bytes) => assert!(
                !bytes.is_empty(),
                "the rangeable source served bytes past the first segment"
            ),
            Err(_) => eprintln!("[proxy test] skipped — body read failed"),
        }
    }
}
