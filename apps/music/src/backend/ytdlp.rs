//! Optional `yt-dlp`-backed **full-track** stream resolver.
//!
//! rustypipe resolves audio through the iOS/TV player clients (no `PoToken`),
//! but since `YouTube`'s SABR rollout those googlevideo URLs are **range-gated
//! to their first ~1-minute segment** — playback stalls at ~1 MB (HTTP
//! 403/416). The Invidious `local` proxy fallback ([`crate::backend::proxy`])
//! only helps while a public instance is up, and that network has largely
//! collapsed.
//!
//! `yt-dlp` resolves the same video through the **`android_vr`** client,
//! which is not SABR-gated and serves the whole file. rustypipe can't use
//! that client itself (its player-client order is fixed to `[Ios, Tv]`), so
//! when `yt-dlp` is present we shell out to it for the full-stream URL.
//!
//! Whether the gating applies is `YouTube`'s decision on the day, and an
//! `android_vr` URL has been observed refusing every range past the first
//! ~1 MB too. So this is the *best available* source, not a guarantee: what
//! actually landed is reported by
//! [`AudioEngine::staging_complete`](crate::audio::AudioEngine::staging_complete),
//! and the transport ends a track whose staged bytes run out rather than
//! freezing on it.
//!
//! Runtime-optional, not a build dependency: invoked as a subprocess, so if
//! `yt-dlp` is missing or disabled, resolution falls back to the proxy and
//! then the direct (truncating) URL. Set `QOS_MUSIC_YTDLP` to the binary path
//! to override, or to an empty string to disable it.

use crate::backend::ResolvedStream;

/// Build a `ResolvedStream` from a resolved googlevideo media `url` for
/// `video_id`, reading `itag` / `clen` / `mime` out of the URL query. Returns
/// `None` when `url` is not a googlevideo media URL. Pure (no I/O) so it is
/// unit-tested without a network or `yt-dlp`.
pub fn parse_stream(video_id: &str, url: &str) -> Option<ResolvedStream> {
    if !url.contains("googlevideo.com") || !url.contains("itag=") {
        return None;
    }
    let field = |key: &str| {
        url.split(['?', '&'])
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
    };
    let itag: u32 = field("itag").and_then(|v| v.parse().ok()).unwrap_or(0);
    let size_bytes: u64 = field("clen").and_then(|v| v.parse().ok()).unwrap_or(0);
    // The `mime` param is a media type, not form data: a `+` in it is literal
    // (`image/svg+xml`), so this decodes per RFC 3986.
    let mime = field("mime").map_or_else(|| "audio/mp4".to_string(), ui::uri::decode);
    // Sniff codec/container off the (decoded or raw) mime — both carry the words.
    let m = mime.to_ascii_lowercase();
    let (codec, format) = if m.contains("webm") || m.contains("opus") {
        ("opus", "webm")
    } else {
        ("mp4a", "m4a")
    };
    Some(ResolvedStream {
        video_id: video_id.to_string(),
        url: url.to_string(),
        mime,
        codec: codec.to_string(),
        format: format.to_string(),
        bitrate: 0,
        itag,
        size_bytes,
        duration_ms: 0,
        fallback_url: None,
    })
}

/// The `yt-dlp` binary to use: the `QOS_MUSIC_YTDLP` override (empty string
/// disables it), else `yt-dlp` from `PATH`. `None` when disabled.
#[cfg(feature = "live")]
fn ytdlp_bin() -> Option<String> {
    match std::env::var_os("QOS_MUSIC_YTDLP") {
        Some(v) => {
            let s = v.to_string_lossy().trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        }
        None => Some("yt-dlp".to_string()),
    }
}

/// Resolve a **fully-rangeable** audio stream URL for `video_id` via `yt-dlp`.
///
/// Returns `None` when `yt-dlp` is unavailable/disabled, errors, times out, or
/// yields no usable googlevideo audio URL — the caller then falls back to the
/// proxy / direct URL. Runs on the background stream worker, so the multi-second
/// subprocess never blocks the UI.
#[cfg(feature = "live")]
pub fn resolve_full_audio(video_id: &str) -> Option<ResolvedStream> {
    let bin = ytdlp_bin()?;
    let watch = format!("https://www.youtube.com/watch?v={video_id}");
    // `-g` prints the resolved media URL(s); `-f bestaudio[ext=m4a]/bestaudio`
    // prefers the broadly-decodable M4A/AAC stream. Quiet so stray warnings
    // (e.g. the missing-JS-runtime notice) don't pollute stdout.
    let args = [
        "-f",
        "bestaudio[ext=m4a]/bestaudio",
        "--no-playlist",
        "--no-warnings",
        "--quiet",
        "-g",
        watch.as_str(),
    ];
    let stdout = run_with_timeout(&bin, &args, std::time::Duration::from_secs(20))?;
    // Take the first googlevideo URL line (there is one per selected format).
    let url = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.contains("googlevideo.com") && l.contains("itag="))?;
    let stream = parse_stream(video_id, url)?;
    tracing::info!(
        target: "music::ytdlp",
        video_id,
        itag = stream.itag,
        size_bytes = stream.size_bytes,
        "yt-dlp resolved a full-track audio URL"
    );
    Some(stream)
}

/// Run `bin args…`, returning its stdout as a `String` if it exits successfully
/// within `timeout`; `None` on spawn failure (not installed), non-zero exit, or
/// timeout (the child is killed). `-g` output is a few hundred bytes, well under
/// the pipe buffer, so reading stdout after the child exits cannot deadlock.
#[cfg(feature = "live")]
fn run_with_timeout(bin: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?; // yt-dlp not installed → None (graceful fallback)

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut s = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut s);
                }
                return if status.success() { Some(s) } else { None };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(target: "music::ytdlp", "yt-dlp timed out — falling back");
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_itag_clen_mime_from_googlevideo_url() {
        let url = "https://rr2---sn-abc.googlevideo.com/videoplayback?c=ANDROID_VR&clen=3458986&itag=140&mime=audio%2Fmp4&source=youtube";
        let s = parse_stream("lYBUbBu4W08", url).expect("parses a googlevideo audio url");
        assert_eq!(s.itag, 140);
        assert_eq!(s.size_bytes, 3_458_986);
        assert_eq!(s.mime, "audio/mp4");
        assert_eq!(s.codec, "mp4a");
        assert_eq!(s.format, "m4a");
        assert_eq!(s.video_id, "lYBUbBu4W08");
        assert!(s.fallback_url.is_none());
    }

    #[test]
    fn parse_recognises_webm_opus() {
        let url = "https://x.googlevideo.com/videoplayback?itag=251&clen=6000000&mime=audio%2Fwebm";
        let s = parse_stream("v", url).expect("parses opus/webm");
        assert_eq!(s.itag, 251);
        assert_eq!(s.codec, "opus");
        assert_eq!(s.format, "webm");
    }

    #[test]
    fn parse_rejects_non_googlevideo_or_non_media() {
        assert!(parse_stream("v", "https://example.com/notvideo").is_none());
        // A googlevideo URL without an itag is not a resolved media URL.
        assert!(parse_stream("v", "https://x.googlevideo.com/foo?source=youtube").is_none());
    }

    #[test]
    fn the_mime_param_decodes_per_rfc_3986() {
        assert_eq!(ui::uri::decode("audio%2Fmp4"), "audio/mp4");
        assert_eq!(ui::uri::decode("plain"), "plain");
        // Malformed trailing escape is left as-is rather than panicking.
        assert_eq!(ui::uri::decode("a%2"), "a%2");
        // A media type may legitimately contain a plus; it is not a space.
        assert_eq!(ui::uri::decode("image%2Fsvg+xml"), "image/svg+xml");
    }
}
