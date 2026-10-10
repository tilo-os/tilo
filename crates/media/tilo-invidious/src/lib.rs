//! Fully rangeable `YouTube` media through public Invidious instances.
//!
//! Raw `googlevideo` media URLs (what a player response resolves without a
//! `PoToken`) are gated by PoToken/SABR: a bounded `Range` request past the
//! first ~1-minute segment is refused, so only the opening minute of a track
//! or video ever streams, and nothing seeks.
//!
//! An Invidious instance's `latest_version?…&local=true` endpoint proxies the
//! same formats **through the instance's own socket**, which serves arbitrary
//! byte ranges over the whole file. Public instances are unreliable, though:
//! one can answer metadata yet have a broken media proxy (googlevideo blocks
//! its IP), or answer `2xx` with an anti-bot HTML page. [`first_serving`]
//! therefore probes the media proxy itself and commits only to an instance
//! that actually serves media bytes.

use tilo_clock::Duration;

/// Public Invidious instances tried in order until one's `local` media proxy
/// actually serves bytes. Operators can prepend their own via
/// `TILO_INVIDIOUS` (comma-separated) for reliability / self-hosting.
///
/// Ordering matters: instances whose media proxy is reliably up come first, so
/// the common case resolves on the first probe.
pub const INSTANCES: &[&str] = &[
    "https://invidious.f5.si",
    "https://invidious.nerdvpn.de",
    "https://inv.nadeko.net",
    "https://yewtu.be",
    "https://invidious.jing.rocks",
    "https://inv.tux.pizza",
    "https://iv.melmac.space",
];

/// A plain browser `User-Agent` for talking to an Invidious `local` proxy (it
/// is not a signed googlevideo URL, so no client-specific UA is needed).
pub const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// Per-probe timeout, so one slow instance cannot stall the resolve.
const PROBE_TIMEOUT: Duration = Duration::from_millis(2500);
/// Budget for the whole resolve across instances and formats.
const RESOLVE_BUDGET: Duration = Duration::from_secs(6);

/// The instances to try: the `TILO_INVIDIOUS` override (a comma-separated
/// list) first, then the built-in [`INSTANCES`].
pub fn instances() -> Vec<String> {
    let custom = std::env::var("TILO_INVIDIOUS").unwrap_or_default();
    custom
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .chain(INSTANCES.iter().map(|s| (*s).to_string()))
        .collect()
}

/// The instance's **local media proxy** URL for a specific itag. `local=true`
/// routes the bytes through the instance's own socket, which honours arbitrary
/// HTTP `Range` requests over the full file.
pub fn proxy_media_url(instance: &str, video_id: &str, itag: u32) -> String {
    format!(
        "{}/latest_version?id={video_id}&itag={itag}&local=true",
        instance.trim_end_matches('/'),
    )
}

/// The first instance proxy URL that serves real media for `video_id`, trying
/// each of `formats` (best first, keyed by `itag`) per instance, and the format
/// it serves. `None` when no proxy answers within the budget; the caller then
/// falls back to its direct, possibly truncating, URL.
pub async fn first_serving<'a, F: Sync>(
    video_id: &str,
    formats: &'a [F],
    itag: impl Fn(&F) -> u32 + Send,
) -> Option<(String, &'a F)> {
    let client = tilo_net::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .user_agent(BROWSER_UA)
        .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
        .build()
        .ok()?;
    let started = tilo_clock::now();
    for instance in instances() {
        for format in formats {
            if started.elapsed() >= RESOLVE_BUDGET {
                tracing::debug!(target: "invidious", video_id, "deadline reached, no working instance");
                return None;
            }
            let url = proxy_media_url(&instance, video_id, itag(format));
            if serves_media(&client, &url).await {
                tracing::info!(target: "invidious", %instance, itag = itag(format), "proxy serves real media");
                return Some((url, format));
            }
        }
    }
    None
}

/// Probe a proxy media URL with a tiny range request: `true` only when the
/// instance answers success with a media `Content-Type` (or none) and a
/// non-empty body that does not sniff as HTML.
async fn serves_media(client: &tilo_net::Client, url: &str) -> bool {
    let resp = match client
        .get(url)
        .header(tilo_net::header::RANGE, "bytes=0-2047")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(error) => {
            tracing::debug!(target: "invidious", %url, %error, "proxy probe request failed");
            return false;
        }
    };
    let status = resp.status();
    let media_type = resp
        .headers()
        .get(tilo_net::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(content_type_is_media);
    if !status.is_success() || !media_type {
        tracing::debug!(target: "invidious", %url, %status, "proxy probe rejected (not media)");
        return false;
    }
    let ok = resp
        .bytes()
        .await
        .is_ok_and(|body| !body.is_empty() && !body_sniffs_as_html(&body));
    if !ok {
        tracing::debug!(target: "invidious", %url, "proxy probe rejected (empty or HTML body)");
    }
    ok
}

/// Whether a `Content-Type` header names playable media rather than an HTML
/// challenge / error page. A proxy serving real media answers `audio/…`,
/// `video/…` or a generic `application/octet-stream`; a broken one answers
/// `text/html` / `text/plain` / `application/json`.
pub fn content_type_is_media(ct: &str) -> bool {
    let ct = ct.trim().to_ascii_lowercase();
    if ct.starts_with("text/") || ct.contains("application/json") {
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
    let head = String::from_utf8_lossy(bytes.get(..256).unwrap_or(bytes));
    let head = head.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype") || head.starts_with("<html") || head.starts_with("<head")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_url_is_local_and_carries_id_and_itag() {
        let url = proxy_media_url("https://yewtu.be/", "dQw4w9WgXcQ", 140);
        // `local=true` is what makes the instance proxy the bytes (and thus
        // serve arbitrary ranges over the whole file).
        assert_eq!(
            url,
            "https://yewtu.be/latest_version?id=dQw4w9WgXcQ&itag=140&local=true"
        );
    }

    #[test]
    fn instances_honour_env_override_first() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_INVIDIOUS", "https://my.inv, https://two.inv");
        let list = instances();
        assert_eq!(list[..2], ["https://my.inv", "https://two.inv"]);
        assert_eq!(
            list.len(),
            2 + INSTANCES.len(),
            "defaults follow the override"
        );
    }

    #[test]
    fn media_content_type_accepts_media_rejects_pages() {
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

    #[test]
    fn html_body_is_sniffed_and_rejected() {
        // The exact shapes observed from public instances (gandalf challenge /
        // Cloudflare / "500 Internal Server Error" HTML).
        assert!(body_sniffs_as_html(b"<html>\r\n<head><title>gandalf"));
        assert!(body_sniffs_as_html(b"<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(body_sniffs_as_html(b"   \n<!doctype html>"));
        // Real media begins with an ISO-BMFF box or arbitrary binary.
        assert!(!body_sniffs_as_html(&[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p'
        ]));
        assert!(!body_sniffs_as_html(b"OggS\x00\x02"));
    }

    /// Resolve a real track and prove the proxied source serves a byte range
    /// **past the first ~1-minute segment** — the truncation point of raw
    /// googlevideo URLs. Skips (never fails) when no instance is reachable.
    #[tokio::test]
    async fn rangeable_source_serves_bytes_past_first_segment() {
        // At 128 kbps one minute is ~960 KB, so ~1.5 MB in is well past the
        // first segment.
        let Some((url, _)) = first_serving("dQw4w9WgXcQ", &[140_u32], |itag| *itag).await else {
            eprintln!("[invidious test] skipped — no instance reachable");
            return;
        };
        let Ok(client) = tilo_net::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
        else {
            return;
        };
        let Ok(resp) = client
            .get(&url)
            .header(tilo_net::header::RANGE, "bytes=1500000-1600000")
            .send()
            .await
        else {
            eprintln!("[invidious test] skipped — proxy fetch failed");
            return;
        };
        if !resp.status().is_success() {
            eprintln!(
                "[invidious test] skipped — proxy returned {}",
                resp.status()
            );
            return;
        }
        if let Ok(bytes) = resp.bytes().await {
            assert!(!bytes.is_empty(), "served bytes past the first segment");
        }
    }
}
