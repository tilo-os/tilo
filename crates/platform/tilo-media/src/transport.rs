//! Collaborative playback transport: follow-the-host with drift correction.
//!
//! [`TransportState`] is a copyable snapshot of where playback is — the play flag, the
//! host's playhead at a wall-clock epoch, and the rate. Followers project it to now and
//! nudge their engine toward the projection; both the projection and the drift
//! decision ([`correction`]) are pure functions of `(state, now_ms)`. Where the state
//! is kept and how it reaches the other participants is the app's.

/// Playhead drift (ms) beyond which a follower hard-seeks to the host instead
/// of letting the small gap ride. Below it the follower stays in sync by simply
/// playing on — a seek every frame would stutter worse than the sub-half-second
/// gap it would correct.
pub const DRIFT_SEEK_THRESHOLD_MS: u64 = 500;

/// A snapshot of collaborative playback state. `epoch_ms` is a wall-clock
/// timestamp in unix millis (`tilo_clock::unix_now_ms`), not a monotonic one,
/// so followers on other machines can project the playhead forward to their
/// own "now".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportState {
    pub playing: bool,
    pub position_ms: u64,
    /// Playback rate (1.0 = normal). Reserved for future speed controls; the
    /// projection already honours it.
    pub rate: f32,
    pub epoch_ms: u64,
}

impl TransportState {
    pub fn projected_position(&self, now_ms: u64) -> u64 {
        if !self.playing {
            return self.position_ms;
        }
        let elapsed = now_ms.saturating_sub(self.epoch_ms);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "elapsed and the projected advance are millisecond counts \
                      for a media playhead — far inside u64/f64's exact-integer \
                      range for any real session — and rate.max(0.0) keeps the \
                      product non-negative"
        )]
        let advance = (elapsed as f64 * f64::from(self.rate.max(0.0))) as u64;
        self.position_ms.saturating_add(advance)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    InSync,
    Seek(u64),
}

pub fn correction(local_ms: u64, target_ms: u64) -> Correction {
    let drift = local_ms.abs_diff(target_ms);
    if drift > DRIFT_SEEK_THRESHOLD_MS {
        Correction::Seek(target_ms)
    } else {
        Correction::InSync
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_projects_and_drifts() {
        {
            let paused = TransportState {
                playing: false,
                position_ms: 4_000,
                rate: 1.0,
                epoch_ms: 10_000,
            };
            assert_eq!(
                paused.projected_position(99_999),
                4_000,
                "paused playhead is frozen"
            );

            let playing = TransportState {
                playing: true,
                position_ms: 4_000,
                rate: 1.0,
                epoch_ms: 10_000,
            };
            // 2.5s of wall-clock since epoch → +2500ms.
            assert_eq!(playing.projected_position(12_500), 6_500);
        }
        {
            let fast = TransportState {
                playing: true,
                position_ms: 0,
                rate: 2.0,
                epoch_ms: 0,
            };
            assert_eq!(
                fast.projected_position(1_000),
                2_000,
                "2x rate doubles the advance"
            );
        }
        {
            // 400ms gap < 500ms threshold → ride it.
            assert_eq!(correction(10_000, 10_400), Correction::InSync);
            // 900ms gap in either direction → hard-seek to the host target.
            assert_eq!(correction(10_000, 10_900), Correction::Seek(10_900));
            assert_eq!(correction(20_000, 18_800), Correction::Seek(18_800));
        }
    }
}
