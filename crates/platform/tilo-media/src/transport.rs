//! Collaborative playback transport — follow-the-host with drift correction.
//!
//! [`TransportState`] is a small, copyable snapshot of "where playback is": the
//! play/pause flag, the authoritative playhead at a wall-clock `epoch`, and the
//! rate. A **host** publishes it; **followers** read it and project it forward
//! to *now*, then nudge their local engine toward that projection.
//!
//! The projection ([`TransportState::projected_position`]) plus the drift
//! decision ([`correction`]) are pure functions of `(state, now_ms)` and carry
//! all the collaborative-playback timing logic, so they are unit-tested without
//! any decode, socket, or clock.
//!
//! Where the state is kept and how it reaches the other participants is the
//! app's: a player keeps it in its shared document.

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
    /// Whether the host is playing (vs. paused).
    pub playing: bool,
    /// The authoritative playhead, in ms, **as of `epoch_ms`**.
    pub position_ms: u64,
    /// Playback rate (1.0 = normal). Reserved for future speed controls; the
    /// projection already honours it.
    pub rate: f32,
    /// Wall-clock time (unix millis) at which `position_ms` was authoritative.
    pub epoch_ms: u64,
}

impl TransportState {
    /// Where the playhead should be **now** for a follower, given `now_ms` in
    /// the same wall-clock frame as `epoch_ms`. While paused the position is
    /// frozen; while playing it advances by `(now - epoch) * rate`.
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

/// The drift decision for a follower whose engine is at `local_ms` when the
/// host's projected playhead is at `target_ms`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    /// Within tolerance — let playback ride, no seek.
    InSync,
    /// Off by more than [`DRIFT_SEEK_THRESHOLD_MS`] — seek to this ms.
    Seek(u64),
}

/// Decide whether a follower at `local_ms` needs to hard-seek to catch the
/// host's projected `target_ms`. Pure; the whole drift policy lives here.
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
