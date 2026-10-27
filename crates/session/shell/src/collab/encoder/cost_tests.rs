#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a measurement: the numbers are printed, small and bounded"
)]

use super::{Codec, EncoderInit, FrameInput, build};
use crate::config::EncoderChoice;
use std::time::{Duration, Instant};

/// A frame of a document-like window: a soft background with rows of
/// glyph-sized marks, so a caret or a typed glyph is a small change and a
/// scroll changes every row.
fn page(width: u32, height: u32, scroll: u32) -> Vec<u8> {
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    for y in 0..height {
        let line = (y + scroll) / 24;
        let in_line = (y + scroll) % 24;
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let glyph =
                in_line > 4 && in_line < 18 && !(x / 9 + line * 7).is_multiple_of(5) && x % 9 < 7;
            let shade = if glyph { 40 } else { 245 - (y % 64) as u8 / 8 };
            rgba[i..i + 4].copy_from_slice(&[shade, shade, shade.saturating_add(4), 255]);
        }
    }
    rgba
}

fn fill(rgba: &mut [u8], width: u32, (left, top, across, down): (u32, u32, u32, u32), value: u8) {
    for row in top..top + down {
        for column in left..left + across {
            let at = ((row * width + column) * 4) as usize;
            rgba[at..at + 3].fill(value);
        }
    }
}

/// CPU time this process has used: user and system, every thread (the
/// encoder's own threads included).
fn cpu() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the struct it is handed.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let time = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec.cast_unsigned())
            + Duration::from_micros(t.tv_usec.cast_unsigned())
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

#[derive(Clone, Copy, Debug)]
enum Change {
    /// A caret blinking: a 2×18 mark on and off.
    Caret,
    /// Typing: one more glyph-sized mark each frame along a line.
    Typing,
    /// Scrolling by 4 rows each frame: every row changes.
    Scroll,
}

/// The encode path a changing window takes, per frame: the window's frame
/// copied out of the frame store, then encoded, told what changed when
/// `changed_only`. Paced at `hz`, so rate control sees the rate it would.
fn measure(
    choice: EncoderChoice,
    (width, height): (u32, u32),
    hz: u32,
    change: Change,
    changed_only: bool,
) -> String {
    let frames = 4 * hz;
    let warm = hz / 2;
    let mut encoder = build(
        EncoderInit::sdr(Codec::H264, width, height, hz, 8000, 0),
        choice,
    )
    .expect("an H.264 encoder");
    let kind = encoder.kind();
    let tall =
        matches!(change, Change::Scroll).then(|| page(width, height + 4 * (warm + frames), 0));
    let mut buffer = page(width, height, 0);
    let mut out = Vec::new();
    let interval = Duration::from_secs(1) / hz;
    let (mut cpu_start, mut wall_start) = (cpu(), Instant::now());
    let mut bytes = 0_usize;
    let mut encode_wall = Duration::ZERO;
    let start = Instant::now();
    for frame in 0..warm + frames {
        if frame == warm {
            (cpu_start, wall_start) = (cpu(), Instant::now());
            bytes = 0;
            encode_wall = Duration::ZERO;
        }
        let changed = match change {
            Change::Caret => {
                let rect = (width / 3, height / 3, 2, 18);
                fill(
                    &mut buffer,
                    width,
                    rect,
                    if frame % 2 == 0 { 20 } else { 245 },
                );
                rect
            }
            Change::Typing => {
                let at = frame % ((width - 80) / 9);
                let rect = (40 + at * 9, height / 2, 7, 13);
                fill(&mut buffer, width, rect, 30);
                rect
            }
            Change::Scroll => {
                let tall = tall.as_ref().unwrap();
                let from = (frame * 4 * width * 4) as usize;
                let len = buffer.len();
                buffer.copy_from_slice(&tall[from..from + len]);
                (0, 0, width, height)
            }
        };
        let committed = buffer.clone();
        out.clear();
        let began = Instant::now();
        let changed = [changed];
        let input = if changed_only {
            FrameInput::RgbaChanged {
                rgba: &committed,
                changed: &changed,
            }
        } else {
            FrameInput::Rgba(&committed)
        };
        encoder.encode(input, &mut out).expect("the frame encodes");
        encode_wall += began.elapsed();
        bytes += out.len();
        let due = start + interval * (frame + 1);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    let cpu_per_frame = cpu().saturating_sub(cpu_start) / frames;
    let wall = wall_start.elapsed();
    format!(
        "{{\"encoder\":\"{}\",\"changed_only\":{changed_only},\"size\":\"{width}x{height}\",\"hz\":{hz},\"change\":\"{change:?}\",\
         \"cpu_ms_per_frame\":{:.3},\"cpu_percent_of_one_core\":{:.1},\"encode_wall_ms\":{:.3},\
         \"kbps\":{:.0},\"kept_rate\":{}}}",
        kind.as_str(),
        cpu_per_frame.as_secs_f64() * 1000.0,
        cpu_per_frame.as_secs_f64() * f64::from(hz) * 100.0,
        encode_wall.as_secs_f64() * 1000.0 / f64::from(frames),
        bytes as f64 * 8.0 / wall.as_secs_f64() / 1000.0,
        wall <= interval * (frames + hz / 10),
    )
}

/// CPU per frame of a streamed window at 60 and 90 Hz. Run alone:
/// `cargo test -p shell --lib -- --ignored --nocapture window_encode_cost`.
#[test]
#[ignore = "a measurement, run on its own"]
fn window_encode_cost() {
    for size in [(1280, 800), (1920, 1080)] {
        for hz in [60, 90] {
            for change in [Change::Caret, Change::Typing, Change::Scroll] {
                for changed_only in [false, true] {
                    eprintln!(
                        "WINDOW-ENCODE {}",
                        measure(EncoderChoice::Software, size, hz, change, changed_only)
                    );
                }
            }
        }
    }
}
