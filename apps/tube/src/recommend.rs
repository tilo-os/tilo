//! Recommendations model — how Tube decides what to show without an account.
//!
//! YouTube's personalised home needs the user's Google cookies (ToS / ban
//! risk). Tube deliberately mirrors **NewPipe / Piped** instead, which build a
//! home from three *no-auth* signals:
//!
//! 1. **Subscriptions feed** — fan-out the latest uploads across the channels
//!    the user subscribed to, interleaved newest-first. This is the primary
//!    home surface ([`fan_out_subscription_feed`]).
//! 2. **Trending** — the region trending browse feed (no personalisation).
//! 3. **Per-video related** — the "up next" list from the video's `next`
//!    endpoint, shown on the watch page.
//!
//! All three are exposed on [`crate::backend::YtBackend`]; this module holds
//! the backend-agnostic *composition* logic (the fan-out + ranking) so both the
//! offline sample and the live rustypipe backend get identical behaviour and it
//! is unit-tested in isolation.
//!
//! Ranking is intentionally simple and explainable — "newest first, round-robin
//! across channels so one prolific uploader can't flood the feed" — matching
//! Tube's no-dark-patterns promise. No engagement signals, no watch-time
//! optimisation.

use crate::backend::{BackendError, BackendOutcome, BackendResult, YtBackend};
use crate::model::Video;

/// Build the subscriptions feed by asking the backend for each subscribed
/// channel's uploads and interleaving them.
///
/// This is the default body of [`YtBackend::subscription_feed`]; a backend with
/// a cheaper bulk endpoint can override, but the offline sample and any simple
/// live backend share this.
///
/// Channels that error (deleted / private) are skipped rather than failing the
/// whole feed — one bad subscription shouldn't blank the home page.
pub fn fan_out_subscription_feed<B: YtBackend + ?Sized>(
    backend: &B,
    channel_ids: &[String],
) -> BackendOutcome {
    if channel_ids.is_empty() {
        return Ok(BackendResult::Videos(Vec::new()));
    }
    let mut per_channel: Vec<Vec<Video>> = Vec::new();
    for id in channel_ids {
        if let Ok(BackendResult::Channel { uploads, .. }) = backend.channel(id) {
            if !uploads.is_empty() {
                per_channel.push(uploads);
            }
        }
    }
    if per_channel.is_empty() {
        return Err(BackendError("no uploads across subscriptions".into()));
    }
    Ok(BackendResult::Videos(interleave_newest_first(per_channel)))
}

/// Round-robin interleave of each channel's uploads so the feed is varied
/// (channel A, channel B, channel C, channel A, …) rather than dominated by one
/// prolific uploader. Each channel's list is assumed already newest-first.
pub fn interleave_newest_first(mut per_channel: Vec<Vec<Video>>) -> Vec<Video> {
    // Reverse each so we can cheaply `pop()` the *newest* off the back.
    for c in &mut per_channel {
        c.reverse();
    }
    let mut out = Vec::new();
    let mut progress = true;
    while progress {
        progress = false;
        for c in &mut per_channel {
            if let Some(v) = c.pop() {
                out.push(v);
                progress = true;
            }
        }
    }
    out
}

/// Cap a feed to a sane page size so a thousand-subscription user doesn't build
/// an enormous widget tree in one frame. Returns the first `n`.
pub fn page(mut videos: Vec<Video>, n: usize) -> Vec<Video> {
    videos.truncate(n);
    videos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::offline::OfflineBackend;

    fn vid(id: &str, ch: &str) -> Video {
        let mut v = crate::sample::model().videos[0].clone();
        v.id = id.into();
        v.channel_id = ch.into();
        v
    }

    #[test]
    fn interleave_is_round_robin_across_channels() {
        let a = vec![vid("a1", "A"), vid("a2", "A"), vid("a3", "A")];
        let b = vec![vid("b1", "B")];
        let out = interleave_newest_first(vec![a, b]);
        // First two come from different channels (round-robin), not A,A.
        assert_eq!(out[0].channel_id, "A");
        assert_eq!(out[1].channel_id, "B");
        assert_eq!(out[2].channel_id, "A");
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn empty_subscriptions_yield_empty_feed() {
        let b = OfflineBackend::new();
        let BackendResult::Videos(v) = fan_out_subscription_feed(&b, &[]).unwrap() else {
            panic!("Videos")
        };
        assert!(v.is_empty());
    }

    #[test]
    fn fan_out_pulls_only_subscribed_channels() {
        let b = OfflineBackend::new();
        let ids = vec!["ch_lowlevel".to_string()];
        let BackendResult::Videos(v) = fan_out_subscription_feed(&b, &ids).unwrap() else {
            panic!("Videos")
        };
        assert!(!v.is_empty());
        assert!(v.iter().all(|v| v.channel_id == "ch_lowlevel"));
    }

    #[test]
    fn page_caps_length() {
        let many: Vec<Video> = (0..10).map(|i| vid(&format!("v{i}"), "A")).collect();
        assert_eq!(page(many, 3).len(), 3);
    }
}
