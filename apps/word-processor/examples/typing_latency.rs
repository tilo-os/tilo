//! What one keystroke costs, measured on the path a person's keystroke takes.
//!
//! A character typed runs the app's whole frame: `handle_event` mutates the
//! CRDT, then `sync` → `build_layout` → `apply_layout` → `draw`. A round trip
//! slower than the gap between keystrokes queues characters in the compositor,
//! landing in bursts. So this measures per-character cost (a burst is a tail; a
//! mean hides it), across document sizes, to rule in/out cost that grows with
//! the document (an O(n)-per-key CRDT write is O(n²) over a paragraph).
//!
//! ```text
//! cargo run -p word-processor --release --example typing_latency
//! cargo run -p word-processor --release --example typing_latency -- --chars 400
//! ```
//!
//! Release only — a debug build is slow enough to make every reading meaningless.

use std::path::PathBuf;

use ui::headless::Driver;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey, SmolStr};
use word_processor::num::{f64_of_usize, u32_of_usize, usize_of_f64};
use word_processor::{EditorDoc, open_document};

/// One paragraph of ordinary prose, used to pad a document to a size.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much. \
By the interval the balcony had filled and the noise off it was the good kind.";

/// `Duration::as_micros` returns `u128`; a keypress or a whole benchmark run
/// never approaches `u64::MAX` microseconds (~584,942 years), so this
/// saturates rather than truncates in practice.
fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Microsecond timings measured here are always far below 2^53, so this
/// conversion never loses precision in practice.
#[allow(
    clippy::cast_precision_loss,
    reason = "microsecond timings never approach 2^53; lossless in practice"
)]
fn lossy_f64(value: u64) -> f64 {
    value as f64
}

/// A microsecond count as milliseconds, for display.
fn ms(us: u64) -> f64 {
    lossy_f64(us) / 1000.0
}

/// The mean of `v`, `0` for an empty slice.
fn avg(v: &[u64]) -> u64 {
    let n = u64::try_from(v.len()).unwrap_or(u64::MAX).max(1);
    v.iter().sum::<u64>().checked_div(n).unwrap_or(0)
}

/// A synthetic character-key press, built directly rather than through
/// `Driver::key` — the paced-burst profile below needs the press alone, with
/// no paired release.
fn press(ch: char) -> WindowEvent {
    let s = SmolStr::new(ch.to_string());
    let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(s.clone());
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(s);
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// A document on disk with `paragraphs` paragraphs in it, opened the way a
/// person's is: through `open_document`, decoded by the format's codec.
fn document_with(paragraphs: usize) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("word-processor-typing-latency");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("body-{paragraphs}.md"));
    let mut body = String::new();
    for _ in 0..paragraphs {
        body.push_str(PARAGRAPH);
        body.push_str("\n\n");
    }
    std::fs::write(&path, &body)?;
    Ok(path)
}

/// Nearest-rank percentile (NIST): the value at 1-indexed rank `ceil(p/100·n)`.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank_f = (p / 100.0) * f64_of_usize(sorted.len());
    let rank = usize_of_f64(rank_f.ceil());
    let idx = rank.saturating_sub(1).min(sorted.len().saturating_sub(1));
    sorted.get(idx).copied().unwrap_or(0)
}

/// Mean cost of each frame pass across every character typed, plus the wall
/// time of the whole keypress — named rather than indexed so a per-pass total
/// can't be transposed by an off-by-one.
#[derive(Default)]
struct PassTotals {
    handle_event: u64,
    sync: u64,
    build_layout: u64,
    apply_layout: u64,
    draw: u64,
    wall: u64,
}

/// Type `chars` characters into a document of `paragraphs` paragraphs, timing
/// each character end to end. Returns the per-character costs in order typed.
///
/// The driver runs a frame per event and a keypress is two events, so its wall
/// clock counts the layout-and-draw work twice. The live app dispatches both
/// events and then paints once — winit coalesces the redraw requests. So the
/// cost attributed to a character here is both `handle_event`s plus **one**
/// frame's sync / layout / draw, taken from the driver's own per-pass
/// profiler. That is what the app really spends before the glyph is on screen.
fn type_and_time(paragraphs: usize, chars: usize) -> std::io::Result<(Vec<u64>, PassTotals)> {
    let path = document_with(paragraphs)?;
    let (state, wp, _fmt) = open_document(&path, tilo_content::ContentStore::memory())?;
    let mut driver = Driver::new(state, Box::new(wp), (1200, 800));

    // Warm up the shaping caches so the first reading is not a cold-start
    // artefact, and leave a caret in the body the way an opened document does.
    driver.type_text("warmup");

    let text: Vec<char> = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .collect();

    driver.begin_profiling();
    let mut per_char = Vec::with_capacity(chars);
    let mut passes = PassTotals::default();
    for ch in text {
        driver.clear_profile_samples();
        let t = ui::time::now();
        driver.key(&ch.to_string());
        let wall = saturating_u64(t.elapsed().as_micros());
        let frames = driver.take_profile_samples();
        if frames.len() != 2 {
            return Err(std::io::Error::other(
                "a keypress should produce exactly the press and release frames",
            ));
        }
        let events: u64 = frames.iter().map(|f| f.handle_event_us).sum();
        // The length check above guarantees a first frame; the fallback is
        // never taken but keeps this free of a panicking index.
        let first = frames.first().copied().unwrap_or_default();
        let paint = first
            .sync_us
            .saturating_add(first.build_layout_us)
            .saturating_add(first.apply_layout_us)
            .saturating_add(first.draw_us);
        passes.handle_event = passes.handle_event.saturating_add(events);
        passes.sync = passes.sync.saturating_add(first.sync_us);
        passes.build_layout = passes.build_layout.saturating_add(first.build_layout_us);
        passes.apply_layout = passes.apply_layout.saturating_add(first.apply_layout_us);
        passes.draw = passes.draw.saturating_add(first.draw_us);
        // Wall time for the whole keypress. It counts the second frame the
        // live app does not run, so it is an upper bound — but unlike the
        // pass sum it also counts what happens *between* passes, which
        // includes the undo commit. The truth is between the two columns.
        passes.wall = passes.wall.saturating_add(wall);
        per_char.push(events.saturating_add(paint));
    }

    // Prove the characters actually landed: a harness that measured an editor
    // quietly dropping input would report a very fast, very wrong number.
    let st = driver.state_mut();
    let ed = EditorDoc::bind(&mut st.doc, &st.root);
    let total: usize = ed
        .blocks(&st.doc)
        .iter()
        .map(|id| ed.text(&st.doc, id).unwrap_or_default().chars().count())
        .sum();
    let expected = paragraphs
        .saturating_mul(PARAGRAPH.chars().count())
        .saturating_add("warmup".len())
        .saturating_add(chars);
    if total < expected {
        return Err(std::io::Error::other(format!(
            "only {total} characters are in the document, expected at least \
             {expected}: the harness typed into something that did not keep the text",
        )));
    }

    let n = u64::try_from(per_char.len()).unwrap_or(u64::MAX).max(1);
    let div = |x: u64| x.checked_div(n).unwrap_or(0);
    passes = PassTotals {
        handle_event: div(passes.handle_event),
        sync: div(passes.sync),
        build_layout: div(passes.build_layout),
        apply_layout: div(passes.apply_layout),
        draw: div(passes.draw),
        wall: div(passes.wall),
    };
    Ok((per_char, passes))
}

/// Print the distribution of `per_char`, plus how the cost moves from the start
/// of the run to the end — a document-size effect shows up there before it
/// shows up in the percentiles.
fn report(label: &str, per_char: &[u64]) {
    let mut sorted = per_char.to_vec();
    sorted.sort_unstable();
    let mean = avg(per_char);

    let quarter = per_char.len() / 4;
    let split = quarter.max(1);
    let first = per_char.get(..split).unwrap_or(per_char);
    let last_start = per_char.len().saturating_sub(split);
    let last = per_char.get(last_start..).unwrap_or(per_char);

    // 60 fps: a character that costs more than one frame cannot be drawn
    // before the next one arrives at any ordinary typing speed.
    let over = per_char.iter().filter(|&&us| us > 16_667).count();

    println!(
        "{label:<28} n={:<4} mean {:>7.2}  p50 {:>7.2}  p95 {:>7.2}  p99 {:>7.2}  max {:>7.2}   \
         first¼ {:>7.2} → last¼ {:>7.2} ({:.1}×)   over 16.7ms: {} ({:.0}%)",
        per_char.len(),
        ms(mean),
        ms(percentile(&sorted, 50.0)),
        ms(percentile(&sorted, 95.0)),
        ms(percentile(&sorted, 99.0)),
        ms(percentile(&sorted, 100.0)),
        ms(avg(first)),
        ms(avg(last)),
        lossy_f64(avg(last)) / lossy_f64(avg(first).max(1)),
        over,
        f64_of_usize(over) * 100.0 / f64_of_usize(per_char.len()),
    );

    // Where the slow characters fall matters as much as how slow they are. A
    // stall that recurs on a fixed stride is periodic work on the typing path;
    // one that creeps upward is cost that grows with the document. The
    // percentiles cannot tell those two apart, and they need different fixes.
    let p50 = percentile(&sorted, 50.0).max(1);
    let spikes: Vec<usize> = per_char
        .iter()
        .enumerate()
        .filter(|&(_, &us)| us > p50.saturating_mul(4))
        .map(|(i, _)| i)
        .collect();
    if !spikes.is_empty() {
        let gaps: Vec<usize> = spikes
            .windows(2)
            .map(|w| match w {
                [a, b] => b.saturating_sub(*a),
                _ => 0,
            })
            .collect();
        let shown: Vec<String> = spikes
            .iter()
            .take(12)
            .map(std::string::ToString::to_string)
            .collect();
        println!(
            "{:>28}   {} characters cost >4× the median, at #{}{}   gaps: {:?}",
            "",
            spikes.len(),
            shown.join(", #"),
            if spikes.len() > 12 { ", …" } else { "" },
            gaps.get(..gaps.len().min(11)).unwrap_or(gaps.as_slice()),
        );
    }
}

/// Type at a fixed real-time cadence — a person's hands do not wait for the
/// app — and record when each character actually finished being processed.
///
/// Keys go in on a schedule from a clock started before the first one, so an
/// overrun slot's due keys go straight through with no pause — a keyboard
/// queue, and what turns a slow character into a visible burst (several land
/// on screen in one display frame).
///
/// Returns, per 60 Hz display frame, how many characters became visible in it.
/// Only the key-down is sent: the driver runs a frame per event but the live
/// app runs one per *character* (winit coalesces the press/release redraws),
/// so press-alone gives the real one-frame-per-character cadence; the release
/// costs nothing since only a press carries text.
fn paced_burst_profile(
    paragraphs: usize,
    chars: usize,
    per_second: f64,
) -> std::io::Result<Vec<usize>> {
    let path = document_with(paragraphs)?;
    let (state, wp, _fmt) = open_document(&path, tilo_content::ContentStore::memory())?;
    let mut driver = Driver::new(state, Box::new(wp), (1200, 800));
    driver.type_text("warmup");

    let interval = std::time::Duration::from_secs_f64(1.0 / per_second);
    let text: Vec<char> = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .collect();

    let start = ui::time::now();
    let mut done_at = Vec::with_capacity(chars);
    for (i, ch) in text.into_iter().enumerate() {
        // The key is due at `i · interval`. If the editor is already past that,
        // the key is waiting and goes in immediately.
        let due = interval.mul_f64(f64_of_usize(i));
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        driver.send(&press(ch));
        done_at.push(start.elapsed());
    }

    let last_us = saturating_u64(done_at.last().copied().unwrap_or_default().as_micros());
    let frames = usize::try_from(last_us / 16_667)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut per_frame = vec![0usize; frames];
    for t in done_at {
        let t_us = saturating_u64(t.as_micros());
        let f = usize::try_from(t_us / 16_667)
            .unwrap_or(usize::MAX)
            .min(frames.saturating_sub(1));
        if let Some(slot) = per_frame.get_mut(f) {
            *slot = slot.saturating_add(1);
        }
    }
    Ok(per_frame)
}

/// Print the burst profile as a run of counts, and say how bad the worst clump
/// is. One character per frame, most frames empty, is what typing should look
/// like at 8 characters a second on a 60 Hz screen.
fn report_bursts(label: &str, per_frame: &[usize], per_second: f64) {
    let typed: usize = per_frame.iter().sum();
    let busy: Vec<usize> = per_frame.iter().copied().filter(|&n| n > 0).collect();
    let worst = busy.iter().copied().max().unwrap_or(0);
    let clumped: usize = busy.iter().filter(|&&n| n > 1).sum();
    println!(
        "{label:<28} {typed} characters at {per_second:.0}/s over {} frames: \
         worst frame showed {worst} at once; {clumped} characters ({:.0}%) \
         arrived in a clump",
        per_frame.len(),
        f64_of_usize(clumped) * 100.0 / f64_of_usize(typed.max(1)),
    );
    // The shape itself, so a burst is legible rather than summarised: one
    // column per display frame, capped at 9 so a clump reads at a glance.
    let shape: String = per_frame
        .iter()
        .map(|&n| match n {
            0 => '.',
            n if n < 10 => char::from_digit(u32_of_usize(n), 10).unwrap_or('#'),
            _ => '#',
        })
        .collect();
    for line in shape.as_bytes().chunks(120) {
        println!("      {}", String::from_utf8_lossy(line));
    }
}

fn main() -> std::io::Result<()> {
    ui::i18n::I18nManager::init("en-US");

    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| i.checked_add(1))
            .and_then(|i| args.get(i))
            .cloned()
    };
    let chars = arg("--chars")
        .and_then(|v| v.parse().ok())
        .unwrap_or(300usize);
    let rate = arg("--rate").and_then(|v| v.parse().ok()).unwrap_or(8.0f64);

    println!(
        "per-character cost, end to end (handle_event + sync + layout + draw), \
         milliseconds\n"
    );
    for paragraphs in [0usize, 1, 4, 16] {
        let (per_char, passes) = type_and_time(paragraphs, chars)?;
        report(&format!("{paragraphs} paragraph(s) of body"), &per_char);
        println!(
            "{:>28}   mean per character: handle_event {:.2}  sync {:.2}  \
             build {:.2}  apply {:.2}  draw {:.2}  | whole keypress, wall {:.2}  (ms)",
            "",
            ms(passes.handle_event),
            ms(passes.sync),
            ms(passes.build_layout),
            ms(passes.apply_layout),
            ms(passes.draw),
            ms(passes.wall),
        );
    }

    println!("\ntyping at a person's pace: what reaches the screen, frame by frame\n");
    for paragraphs in [0usize, 4, 16] {
        let per_frame = paced_burst_profile(paragraphs, chars.min(120), rate)?;
        report_bursts(
            &format!("{paragraphs} paragraph(s) of body"),
            &per_frame,
            rate,
        );
    }

    Ok(())
}
