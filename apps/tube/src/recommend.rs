//! Recommendations model — how Tube decides what to show without an account.
//!
//! `YouTube`'s personalised home needs the user's Google cookies (`ToS` / ban
//! risk). Tube mirrors **`NewPipe` / Piped** instead: no-auth signals —
//! region trending and per-video related, exposed on
//! [`crate::backend::YtBackend`], plus feeds composed from several lists.
//! This module holds the backend-agnostic *composition* logic so the offline
//! sample and the live backend behave identically.
//!
//! Ranking is intentionally simple and explainable — newest first,
//! round-robin across channels so one prolific uploader can't flood the feed.
//! No engagement signals, no watch-time optimisation.

use crate::model::Video;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(id: &str, ch: &str) -> Video {
        let mut v = crate::sample::model().videos[0].clone();
        v.id = id.into();
        v.channel_id = ch.into();
        v
    }

    #[test]
    fn interleave_is_round_robin_across_channels() {
        {
            let a = vec![vid("a1", "A"), vid("a2", "A"), vid("a3", "A")];
            let b = vec![vid("b1", "B")];
            let out = interleave_newest_first(vec![a, b]);
            // First two come from different channels (round-robin), not A,A.
            assert_eq!(out[0].channel_id, "A");
            assert_eq!(out[1].channel_id, "B");
            assert_eq!(out[2].channel_id, "A");
            assert_eq!(out.len(), 4);
        }
    }
}
