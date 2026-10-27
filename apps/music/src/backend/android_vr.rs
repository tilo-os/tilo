//! Pure-Rust full-track resolver using `YouTube`'s keyless `ANDROID_VR` client.
//!
//! Rustypipe's ordinary iOS/TV media URLs can be SABR/range-gated after their
//! first segment. `ANDROID_VR` returns ordinary adaptive audio URLs and does
//! not require a JavaScript player. Querying that public `InnerTube` client
//! directly removes the `yt-dlp`/Python subprocess from the trusted OS while
//! retaining the full-track path.

#[cfg(any(feature = "live", test))]
use crate::backend::ResolvedStream;

#[cfg(feature = "live")]
const PLAYER_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
#[cfg(feature = "live")]
const CLIENT_NAME: &str = "28";
#[cfg(feature = "live")]
const CLIENT_VERSION: &str = "1.65.10";
#[cfg(feature = "live")]
pub(crate) const USER_AGENT: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
#[cfg(feature = "live")]
const MAX_PLAYER_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[cfg(any(feature = "live", test))]
fn numeric_field(value: &serde_json::Value, name: &str) -> Option<u64> {
    value
        .get(name)
        .and_then(|field| field.as_u64().or_else(|| field.as_str()?.parse().ok()))
}

#[cfg(any(feature = "live", test))]
fn parse_player_response(video_id: &str, bytes: &[u8]) -> Option<ResolvedStream> {
    let response: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if response
        .pointer("/playabilityStatus/status")
        .and_then(serde_json::Value::as_str)
        != Some("OK")
    {
        return None;
    }
    let formats = response
        .pointer("/streamingData/adaptiveFormats")?
        .as_array()?;
    let format = formats
        .iter()
        .filter(|format| {
            format
                .get("mimeType")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mime| mime.starts_with("audio/"))
                && format
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(crate::backend::trusted_googlevideo_url)
        })
        .max_by_key(|format| {
            let mime = format
                .get("mimeType")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            (
                u8::from(mime.starts_with("audio/mp4")),
                numeric_field(format, "bitrate").unwrap_or(0),
            )
        })?;
    let url = format.get("url")?.as_str()?.to_owned();
    let mime = format.get("mimeType")?.as_str()?.to_owned();
    let lower = mime.to_ascii_lowercase();
    let (codec, container) = if lower.contains("webm") || lower.contains("opus") {
        ("opus", "webm")
    } else {
        ("mp4a", "m4a")
    };
    Some(ResolvedStream {
        video_id: video_id.to_owned(),
        url,
        mime,
        codec: codec.to_owned(),
        format: container.to_owned(),
        bitrate: u32::try_from(numeric_field(format, "bitrate").unwrap_or(0)).unwrap_or(u32::MAX),
        itag: u32::try_from(numeric_field(format, "itag")?).ok()?,
        size_bytes: numeric_field(format, "contentLength").unwrap_or(0),
        duration_ms: numeric_field(format, "approxDurationMs").unwrap_or(0),
        fallback_url: None,
    })
}

/// Resolve a full-track audio URL without invoking an external interpreter.
///
/// The request and response are time/size bounded and accept media URLs only
/// from `YouTube`'s exact HTTPS `googlevideo.com` origin. `None` lets the caller
/// fall back to the independently resolved Rustypipe/proxy paths.
#[cfg(feature = "live")]
pub fn resolve_full_audio(video_id: &str) -> Option<ResolvedStream> {
    use std::io::Read;

    if video_id.is_empty()
        || video_id.len() > 32
        || !video_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent(USER_AGENT)
        .build()
        .ok()?;
    let body = serde_json::json!({
        "context": { "client": {
            "clientName": "ANDROID_VR",
            "clientVersion": CLIENT_VERSION,
            "deviceMake": "Oculus",
            "deviceModel": "Quest 3",
            "androidSdkVersion": 32,
            "userAgent": USER_AGENT,
            "osName": "Android",
            "osVersion": "12L"
        }},
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true
    });
    let response = client
        .post(PLAYER_ENDPOINT)
        .header("X-YouTube-Client-Name", CLIENT_NAME)
        .header("X-YouTube-Client-Version", CLIENT_VERSION)
        .json(&body)
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PLAYER_RESPONSE_BYTES)
    {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_PLAYER_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_PLAYER_RESPONSE_BYTES {
        return None;
    }
    let stream = parse_player_response(video_id, &bytes)?;
    tracing::info!(
        target: "music::android_vr",
        video_id,
        itag = stream.itag,
        size_bytes = stream.size_bytes,
        "Rust-native Android VR resolver returned a full-track audio URL"
    );
    Some(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_response_prefers_trusted_m4a_and_fails_closed() {
        // Phase `player_response_prefers_trusted_m4a_audio`.
        {
            let response = br#"{
              "playabilityStatus":{"status":"OK"},
              "streamingData":{"adaptiveFormats":[
                {"itag":251,"url":"https://r1.googlevideo.com/videoplayback?itag=251","mimeType":"audio/webm; codecs=\"opus\"","bitrate":160000,"contentLength":"4000000"},
                {"itag":140,"url":"https://r1.googlevideo.com/videoplayback?itag=140","mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":128000,"contentLength":"3458986","approxDurationMs":"212000"},
                {"itag":999,"url":"https://attacker.invalid/audio","mimeType":"audio/mp4","bitrate":999999}
              ]}}
            "#;
            let stream = parse_player_response("video_id", response).expect("valid audio stream");
            assert_eq!(stream.itag, 140);
            assert_eq!(stream.size_bytes, 3_458_986);
            assert_eq!(stream.duration_ms, 212_000);
            assert_eq!(stream.format, "m4a");
        }

        {
            assert!(
                parse_player_response("v", br#"{"playabilityStatus":{"status":"LOGIN_REQUIRED"}}"#)
                    .is_none()
            );
            assert!(parse_player_response("v", br#"{"playabilityStatus":{"status":"OK"},"streamingData":{"adaptiveFormats":[{"itag":140,"url":"https://googlevideo.com.attacker.invalid/a","mimeType":"audio/mp4"}]}}"#).is_none());
        }
    }

    #[cfg(feature = "live")]
    #[test]
    #[ignore = "queries YouTube and fetches bytes from a real track"]
    fn android_vr_resolves_and_serves_media() {
        let stream = resolve_full_audio("dQw4w9WgXcQ").expect("Android VR player response");
        assert!(stream.size_bytes > 2 * 1024 * 1024);
        let response = tilo_net::http::blocking_client()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent(USER_AGENT)
            .build()
            .unwrap()
            .get(&stream.url)
            .header(reqwest::header::RANGE, "bytes=0-2047")
            .send()
            .expect("tail range request");
        assert!(
            response.status().is_success(),
            "status={}",
            response.status()
        );
        assert!(!response.bytes().expect("tail bytes").is_empty());
    }
}
