//! Fully-rangeable VIDEO+AUDIO resolution through public Invidious instances
//! ([`tilo_invidious`]): `GStreamer`'s `souphttpsrc` needs arbitrary byte
//! ranges to seek and keep reading, which raw googlevideo URLs refuse past the
//! first segment.

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

/// Resolve a video id to a **fully-rangeable** proxied muxed video+audio
/// stream no larger than `wanted` (a height in pixels; `None` for the best
/// available). `None` when no instance's proxy answers within the budget (the
/// caller then falls back to the direct, possibly-truncating, URL).
pub async fn resolve_rangeable(video_id: &str, wanted: Option<u32>) -> Option<ResolvedVideo> {
    let wanted = wanted.unwrap_or(u32::MAX);
    // The formats this app can play, largest first: the one a person asked
    // for is the largest that fits, so choosing 480p on a metered connection
    // spends 360p of data rather than 720p.
    let candidates: Vec<(u32, &str, &str)> = KNOWN_MUXED_ITAGS
        .iter()
        .copied()
        .filter(|(_, _, quality)| {
            crate::util::quality_height(quality).is_none_or(|height| height <= wanted)
        })
        .collect();
    let candidates = if candidates.is_empty() {
        KNOWN_MUXED_ITAGS
            .iter()
            .copied()
            .min_by_key(|(_, _, quality)| crate::util::quality_height(quality).unwrap_or(u32::MAX))
            .into_iter()
            .collect()
    } else {
        candidates
    };
    let (url, &(itag, mime, quality)) =
        tilo_invidious::first_serving(video_id, &candidates, |f| f.0).await?;
    Some(ResolvedVideo {
        video_id: video_id.to_string(),
        url,
        itag,
        mime: mime.to_string(),
        quality: quality.to_string(),
        has_audio: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_muxed_itags_prefer_higher_quality_first() {
        // 22 (720p) is probed before 18 (360p), so the best available muxed
        // stream is committed to first.
        assert_eq!(KNOWN_MUXED_ITAGS[0].0, 22);
        assert_eq!(KNOWN_MUXED_ITAGS[1].0, 18);
    }
}
