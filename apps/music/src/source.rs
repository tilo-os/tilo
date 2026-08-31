//! Streaming source abstraction.
//!
//! Free YouTube-Music streaming is reached through a privacy-frontend audio
//! endpoint rather than Google's own API. Two interchangeable backends are
//! modelled, both of which expose a track's adaptive **audio-only** streams:
//!
//! * **Piped** — `GET {instance}/streams/{videoId}` returns a JSON body with
//!   an `audioStreams[]` array; each entry has `url`, `bitrate`, `codec`,
//!   `mimeType` and `quality`. (`/search?q=&filter=music_songs` resolves a
//!   query to video ids.)
//! * **Invidious** — `GET {instance}/api/v1/videos/{id}` returns
//!   `adaptiveFormats[]`; the audio entries carry `url`, `bitrate` and
//!   `type` but no `qualityLabel`/`resolution`.
//!
//! The networking itself is gated behind the `streaming` cargo feature so
//! the default build (and the offline `--shots` screenshot) needs no HTTP
//! stack. The pure URL-building + selection logic below is always compiled
//! and unit-tested, so the contract is exercised even offline.

/// Which privacy frontend to resolve audio through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Piped,
    Invidious,
}

/// A configured streaming source: a backend + a base instance URL.
#[derive(Clone, Debug)]
pub struct Source {
    pub backend: Backend,
    /// Instance base, e.g. `https://pipedapi.kavin.rocks` (no trailing slash).
    pub instance: String,
}

impl Default for Source {
    fn default() -> Self {
        // A well-known public Piped API instance. Operators can point this
        // at a self-hosted instance for reliability.
        Self {
            backend: Backend::Piped,
            instance: "https://pipedapi.kavin.rocks".into(),
        }
    }
}

/// One resolved audio stream candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioStream {
    pub url: String,
    /// Bitrate in bits/second (higher = better quality).
    pub bitrate: u32,
    pub mime: String,
}

impl Source {
    fn base(&self) -> &str {
        self.instance.trim_end_matches('/')
    }

    /// The metadata/stream-list endpoint for a video id.
    pub fn streams_url(&self, video_id: &str) -> String {
        match self.backend {
            Backend::Piped => format!("{}/streams/{}", self.base(), video_id),
            Backend::Invidious => format!("{}/api/v1/videos/{}", self.base(), video_id),
        }
    }

    /// The search endpoint for a free-text query, scoped to music songs.
    pub fn search_url(&self, query: &str) -> String {
        let q = urlencode(query);
        match self.backend {
            Backend::Piped => format!("{}/search?q={}&filter=music_songs", self.base(), q),
            Backend::Invidious => {
                format!("{}/api/v1/search?q={}&type=video", self.base(), q)
            }
        }
    }

    /// Pick the best (highest-bitrate) audio stream from a candidate list.
    pub fn best_audio(streams: &[AudioStream]) -> Option<&AudioStream> {
        streams.iter().max_by_key(|s| s.bitrate)
    }

    /// Resolve a video id to its best audio stream URL.
    ///
    /// Without the `streaming` feature this is a compile-time stub that
    /// returns the planned request URL in the error, documenting exactly
    /// what the worker *would* fetch. The audio worker treats this as
    /// "cannot stream right now" and stays silent.
    ///
    /// # Errors
    ///
    /// Always returns `Err` in this build — the `streaming` feature is
    /// required to actually fetch a stream.
    #[cfg(not(feature = "streaming"))]
    pub fn resolve_best(&self, video_id: &str) -> Result<AudioStream, String> {
        // TODO(streaming): enable the `streaming` feature to perform a real
        // blocking GET against `streams_url` and parse `audioStreams` /
        // `adaptiveFormats`. Offline builds bundle the sample library so the
        // UI is fully populated without this path.
        Err(format!("offline: would GET {}", self.streams_url(video_id)))
    }

    /// Resolve a video id to its best audio stream URL via a real blocking
    /// fetch against the configured backend.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the request fails or the response isn't a success
    /// status, if the body isn't valid JSON, or if the response carries no
    /// audio stream candidates.
    #[cfg(feature = "streaming")]
    pub fn resolve_best(&self, video_id: &str) -> Result<AudioStream, String> {
        let url = self.streams_url(video_id);
        let body = reqwest::blocking::get(&url)
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::text)
            .map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let streams = parse_audio_streams(self.backend, &json);
        Self::best_audio(&streams)
            .cloned()
            .ok_or_else(|| "no audio streams in response".to_string())
    }
}

/// Parse a backend response body into audio-stream candidates.
#[cfg(feature = "streaming")]
fn parse_audio_streams(backend: Backend, json: &serde_json::Value) -> Vec<AudioStream> {
    let arr = match backend {
        Backend::Piped => json.get("audioStreams"),
        Backend::Invidious => json.get("adaptiveFormats"),
    };
    let Some(items) = arr.and_then(|v| v.as_array()) else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|it| {
            let url = it.get("url")?.as_str()?.to_string();
            let mime = it
                .get("mimeType")
                .or_else(|| it.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("audio/unknown")
                .to_string();
            // Invidious sends bitrate as a string; Piped as a number.
            let bitrate = it
                .get("bitrate")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(0) as u32;
            // Only keep audio formats.
            if backend == Backend::Invidious && !mime.starts_with("audio") {
                return None;
            }
            Some(AudioStream { url, bitrate, mime })
        })
        .collect()
}

/// Minimal percent-encoding for query strings (alnum + a few safe chars
/// pass through; everything else is %XX). Avoids pulling a url crate.
fn urlencode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piped_urls() {
        let s = Source {
            backend: Backend::Piped,
            instance: "https://x.test/".into(),
        };
        assert_eq!(s.streams_url("abc123"), "https://x.test/streams/abc123");
        assert_eq!(
            s.search_url("aurora skies"),
            "https://x.test/search?q=aurora+skies&filter=music_songs"
        );
    }

    #[test]
    fn invidious_urls() {
        let s = Source {
            backend: Backend::Invidious,
            instance: "https://iv.test".into(),
        };
        assert_eq!(s.streams_url("zzz"), "https://iv.test/api/v1/videos/zzz");
    }

    #[test]
    fn best_audio_picks_highest_bitrate() {
        let streams = vec![
            AudioStream {
                url: "a".into(),
                bitrate: 128_000,
                mime: "audio/mp4".into(),
            },
            AudioStream {
                url: "b".into(),
                bitrate: 256_000,
                mime: "audio/webm".into(),
            },
            AudioStream {
                url: "c".into(),
                bitrate: 64_000,
                mime: "audio/mp4".into(),
            },
        ];
        assert_eq!(Source::best_audio(&streams).unwrap().url, "b");
    }

    #[test]
    fn offline_resolve_reports_planned_url() {
        let s = Source::default();
        let err = s.resolve_best("abc").unwrap_err();
        assert!(err.contains("/streams/abc"));
    }

    #[test]
    fn urlencode_escapes() {
        assert_eq!(urlencode("a b&c"), "a+b%26c");
    }
}
