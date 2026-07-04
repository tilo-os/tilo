//! Timecode formatting helpers.
//!
//! A non-linear editor speaks two clocks at once: wall-clock seconds
//! (what the user thinks in) and frame counts (what export and trimming
//! snap to). These helpers convert between the two and render the
//! `HH:MM:SS:FF` / `MM:SS` strings shown across the ruler, transport,
//! and inspector. Everything is pure — no rendering, no state.

/// Render `seconds` at `fps` as `MM:SS:FF` (drop the hours when zero) or
/// `HH:MM:SS:FF` when the clip runs past an hour. The frame field is the
/// fractional remainder quantised to the frame grid.
pub fn timecode(seconds: f64, fps: f64) -> String {
    let fps = if fps <= 0.0 { 30.0 } else { fps };
    let seconds = seconds.max(0.0);
    let total_frames = (seconds * fps).round() as i64;
    let frames_per = fps.round() as i64;
    let f = total_frames % frames_per;
    let total_secs = total_frames / frames_per;
    let s = total_secs % 60;
    let m = (total_secs / 60) % 60;
    let h = total_secs / 3600;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}:{f:02}")
    } else {
        format!("{m:02}:{s:02}:{f:02}")
    }
}

/// Compact clock for media-bin durations: `M:SS` (or `H:MM:SS`). No
/// frame field — the bin is a glance, not a scrub.
pub fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let total = seconds.round() as i64;
    let s = total % 60;
    let m = (total / 60) % 60;
    let h = total / 3600;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// One frame's duration in seconds at `fps`. Guards against a zero fps.
pub fn frame_step(fps: f64) -> f64 {
    if fps <= 0.0 { 1.0 / 30.0 } else { 1.0 / fps }
}

/// Snap `seconds` to the nearest frame boundary at `fps`. Trimming and
/// the playhead always land on a real frame so export is deterministic.
pub fn snap_to_frame(seconds: f64, fps: f64) -> f64 {
    let step = frame_step(fps);
    (seconds / step).round() * step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_formats_minutes_seconds_frames() {
        // 65.5s at 30fps → 01:05:15
        assert_eq!(timecode(65.5, 30.0), "01:05:15");
    }

    #[test]
    fn timecode_adds_hours_past_an_hour() {
        assert_eq!(timecode(3661.0, 25.0), "01:01:01:00");
    }

    #[test]
    fn clock_is_compact() {
        assert_eq!(clock(75.0), "1:15");
        assert_eq!(clock(9.0), "0:09");
    }

    #[test]
    fn snap_lands_on_frame_grid() {
        let snapped = snap_to_frame(1.013, 30.0);
        // nearest 1/30 boundary to 1.013 is 30/30 = 1.0 (1.013*30=30.39→30)
        assert!((snapped - 1.0).abs() < 1e-9, "got {snapped}");
    }
}
