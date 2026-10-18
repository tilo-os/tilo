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
    // format: machine (SMPTE timecode, hours:minutes:seconds:frames)
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
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to zero above; a clip longer than u64 seconds does not exist"
    )]
    let total = seconds.round() as u64;
    ui::format::timecode(total)
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
    fn timecode_formats_clocks_and_snaps_to_frames() {
        // Case `timecode_formats_minutes_seconds_frames`.
        {
            // 65.5s at 30fps → 01:05:15
            assert_eq!(timecode(65.5, 30.0), "01:05:15");
        }
        // Case `timecode_adds_hours_past_an_hour`.
        {
            assert_eq!(timecode(3661.0, 25.0), "01:01:01:00");
        }
        // Case `clock_is_compact`.
        {
            assert_eq!(clock(75.0), "1:15");
            assert_eq!(clock(9.0), "0:09");
        }
        // Case `snap_lands_on_frame_grid`.
        {
            let snapped = snap_to_frame(1.013, 30.0);
            // nearest 1/30 boundary to 1.013 is 30/30 = 1.0 (1.013*30=30.39→30)
            assert!((snapped - 1.0).abs() < 1e-9, "got {snapped}");
        }
    }
}

/// Parse a typed timecode into seconds.
///
/// Accepts what a person actually types rather than one rigid shape:
/// `90` (seconds), `1:30` (M:SS), `1:30:15` (M:SS:FF at `fps`), and
/// `1:02:03:04` (H:MM:SS:FF). Returns `None` for anything that is not a
/// number in every field — a jump to a garbled time would be worse than
/// refusing, because the user would not know where they had gone.
pub fn parse(input: &str, fps: f64) -> Option<f64> {
    let fps = if fps <= 0.0 { 30.0 } else { fps };
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(':').collect();
    if parts.len() > 4 {
        return None;
    }
    let nums: Option<Vec<f64>> = parts
        .iter()
        .map(|p| p.trim().parse::<f64>().ok().filter(|v| *v >= 0.0))
        .collect();
    let fields = nums?;
    Some(match fields.as_slice() {
        [s] => *s,
        [m, s] => m * 60.0 + s,
        [m, s, f] => m * 60.0 + s + f / fps,
        [h, m, s, f] => h * 3600.0 + m * 60.0 + s + f / fps,
        _ => return None,
    })
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn timecode_parses_typed_shapes_at_the_project_rate() {
        // Case `parses_the_shapes_a_person_types`.
        {
            assert_eq!(parse("90", 30.0), Some(90.0));
            assert_eq!(parse("1:30", 30.0), Some(90.0));
            assert_eq!(parse(" 1:30 ", 30.0), Some(90.0));
            assert_eq!(parse("1:30:15", 30.0), Some(90.5));
            assert_eq!(parse("1:02:03:00", 30.0), Some(3723.0));
        }
        // Case `refuses_what_it_cannot_read_rather_than_guessing`.
        {
            // Landing somewhere arbitrary is worse than not moving: the user would
            // not know where they had gone.
            for bad in ["", "  ", "abc", "1:xx", "-5", "1:2:3:4:5", "1::2"] {
                assert_eq!(parse(bad, 30.0), None, "{bad:?} must not parse");
            }
        }
        // Case `a_frame_field_is_read_at_the_projects_rate`.
        {
            // The same typed frame count is a different instant at a different fps.
            assert_eq!(parse("0:00:12", 24.0), Some(0.5));
            assert_eq!(parse("0:00:12", 48.0), Some(0.25));
        }
    }
}
