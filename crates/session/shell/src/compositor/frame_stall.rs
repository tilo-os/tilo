//! Run the frame loop as slowly as a slow machine does.
//!
//! `TILO_TEST_FRAME_STALL_MS` makes the compositor block for a while after
//! every frame it paints: a fixed `80`, or a range such as `50-130` from
//! which each frame draws its own delay. The loop really is that slow, input
//! and clients wait it out, and everything stepped per frame (layout springs,
//! animations, configures) sees the long frame times a slow laptop gives it.
//! The real-boot slow-frame gate (`os/dev/test-slow-frame-launch.sh`) runs
//! apps under it.
//!
//! A developer override like `TILO_TEST_CONTROL`: read only when
//! [`super::dev_env::DEV_ENV_OVERRIDES`] holds, so a shipped compositor never
//! slows itself down whatever its environment says.

use std::time::Duration;

/// The variable naming the per-frame stall.
pub const FRAME_STALL_ENV: &str = "TILO_TEST_FRAME_STALL_MS";

/// The longest stall a frame may be given. Slower than this is not a slow
/// machine but a hung one, which other tests cover.
pub const MAX_STALL_MS: u32 = 2_000;

/// The per-frame stall: a delay drawn from `min_ms..=max_ms` after each
/// painted frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStall {
    min_ms: u32,
    max_ms: u32,
    rng: u64,
}

/// Parse `80` or `50-130` (milliseconds). The bounds may be equal.
///
/// # Errors
///
/// The reason the value is refused: not a number, zero, a reversed range,
/// or more than [`MAX_STALL_MS`].
pub fn parse(raw: &str) -> Result<(u32, u32), String> {
    let raw = raw.trim();
    let bound = |text: &str| {
        text.trim()
            .parse::<u32>()
            .map_err(|_| format!("{text:?} is not a whole number of milliseconds"))
    };
    let (min_ms, max_ms) = if let Some((low, high)) = raw.split_once('-') {
        (bound(low)?, bound(high)?)
    } else {
        let ms = bound(raw)?;
        (ms, ms)
    };
    if min_ms == 0 {
        return Err("a stall of 0 ms does nothing; leave the variable unset".to_string());
    }
    if min_ms > max_ms {
        return Err(format!("{min_ms}-{max_ms} runs backwards"));
    }
    if max_ms > MAX_STALL_MS {
        return Err(format!("{max_ms} ms is over the {MAX_STALL_MS} ms limit"));
    }
    Ok((min_ms, max_ms))
}

impl FrameStall {
    /// A stall over `min_ms..=max_ms`, its jitter seeded by `seed`.
    pub fn new(min_ms: u32, max_ms: u32, seed: u64) -> Self {
        Self {
            min_ms,
            max_ms,
            // xorshift has one fixed point, zero.
            rng: seed | 1,
        }
    }

    /// The stall the environment asks for, as a pure function of the gate
    /// and the raw variable so the shipped answer (always `None`) is
    /// testable from a debug run. A value that does not parse is reported
    /// and ignored.
    pub fn from_env(
        dev_overrides: bool,
        raw: Option<std::ffi::OsString>,
        seed: u64,
    ) -> Option<Self> {
        if !dev_overrides {
            return None;
        }
        let raw = raw?;
        let text = raw.to_string_lossy();
        if text.trim().is_empty() {
            return None;
        }
        match parse(&text) {
            Ok((min_ms, max_ms)) => Some(Self::new(min_ms, max_ms, seed)),
            Err(reason) => {
                eprintln!("tilo compositor: ignoring {FRAME_STALL_ENV}={text:?}: {reason}");
                None
            }
        }
    }

    /// Read [`FRAME_STALL_ENV`] behind the developer-override gate and
    /// announce a stall that is in effect.
    pub fn from_process_env() -> Option<Self> {
        let since = tilo_clock::unix_now_ns();
        let seed = (since / 1_000_000_000).rotate_left(32)
            ^ (since % 1_000_000_000)
            ^ u64::from(std::process::id());
        let stall = Self::from_env(
            super::dev_env::DEV_ENV_OVERRIDES,
            std::env::var_os(FRAME_STALL_ENV),
            seed,
        )?;
        eprintln!(
            "tilo compositor: {FRAME_STALL_ENV}: every painted frame stalls {} ms, as on a slow machine",
            stall.describe()
        );
        Some(stall)
    }

    /// `80` or `50-130`, as the variable spells it.
    pub fn describe(&self) -> String {
        if self.min_ms == self.max_ms {
            self.min_ms.to_string()
        } else {
            format!("{}-{}", self.min_ms, self.max_ms)
        }
    }

    /// The next frame's delay.
    pub fn next_delay(&mut self) -> Duration {
        let span = u64::from(self.max_ms.saturating_sub(self.min_ms)).saturating_add(1);
        self.rng ^= self.rng.wrapping_shl(13);
        self.rng ^= self.rng.wrapping_shr(7);
        self.rng ^= self.rng.wrapping_shl(17);
        let ms = u64::from(self.min_ms).saturating_add(self.rng.checked_rem(span).unwrap_or(0));
        Duration::from_millis(ms)
    }

    /// Block the frame loop for the next frame's delay.
    pub fn stall(&mut self) {
        std::thread::sleep(self.next_delay());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_stall_parses_reads_only_behind_the_gate_and_jitters_within_its_range() {
        // Phase `parse`.
        assert_eq!(parse("80"), Ok((80, 80)));
        assert_eq!(parse(" 50-130 "), Ok((50, 130)));
        assert_eq!(parse("50 - 130"), Ok((50, 130)));
        for refused in ["", "0", "0-10", "130-50", "fast", "50-", "3000", "-5"] {
            assert!(parse(refused).is_err(), "{refused:?} parsed");
        }

        // Phase `gate`: a shipped build never stalls, whatever is set.
        let set = || Some(std::ffi::OsString::from("50-130"));
        assert_eq!(FrameStall::from_env(false, set(), 7), None);
        assert_eq!(
            FrameStall::from_env(true, set(), 7),
            Some(FrameStall::new(50, 130, 7))
        );
        assert_eq!(FrameStall::from_env(true, None, 7), None);
        assert_eq!(FrameStall::from_env(true, Some("".into()), 7), None);
        assert_eq!(FrameStall::from_env(true, Some("slow".into()), 7), None);

        // Phase `jitter`: every delay inside the range, and not all alike.
        let mut stall = FrameStall::new(50, 130, 0);
        let delays: Vec<_> = (0..200).map(|_| stall.next_delay()).collect();
        assert!(
            delays
                .iter()
                .all(|d| (Duration::from_millis(50)..=Duration::from_millis(130)).contains(d))
        );
        assert!(delays.iter().any(|d| *d < Duration::from_millis(70)));
        assert!(delays.iter().any(|d| *d > Duration::from_millis(110)));
        let mut fixed = FrameStall::new(80, 80, 3);
        assert!((0..20).all(|_| fixed.next_delay() == Duration::from_millis(80)));
        assert_eq!(fixed.describe(), "80");
        assert_eq!(stall.describe(), "50-130");
    }
}
