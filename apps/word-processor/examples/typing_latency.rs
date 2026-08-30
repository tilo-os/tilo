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
use std::time::Instant;

use ui::headless::Driver;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey, SmolStr};
use word_processor::{EditorDoc, open_document};

/// One paragraph of ordinary prose, used to pad a document to a size.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much. \
By the interval the balcony had filled and the noise off it was the good kind.";

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
fn document_with(paragraphs: usize) -> PathBuf {
    let dir = std::env::temp_dir().join("word-processor-typing-latency");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join(format!("body-{paragraphs}.md"));
    let mut body = String::new();
    for _ in 0..paragraphs {
        body.push_str(PARAGRAPH);
        body.push_str("\n\n");
    }
    std::fs::write(&path, &body).expect("the document is written");
    path
}

/// Nearest-rank percentile (NIST): the value at 1-indexed rank `ceil(p/100·n)`.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
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
fn type_and_time(paragraphs: usize, chars: usize) -> (Vec<u64>, [u64; 6]) {
    let path = document_with(paragraphs);
    let (state, wp, _fmt) = open_document(&path).expect("the document opens");
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
    let mut passes = [0u64; 6];
    for ch in text {
        driver.clear_profile_samples();
        let t = Instant::now();
        driver.key(&ch.to_string());
        let wall = t.elapsed().as_micros() as u64;
        let frames = driver.take_profile_samples();
        assert_eq!(
            frames.len(),
            2,
            "a keypress should produce exactly the press and release frames",
        );
        let events: u64 = frames.iter().map(|f| f.handle_event_us).sum();
        let paint = frames[0].sync_us
            + frames[0].build_layout_us
            + frames[0].apply_layout_us
            + frames[0].draw_us;
        passes[0] += events;
        passes[1] += frames[0].sync_us;
        passes[2] += frames[0].build_layout_us;
        passes[3] += frames[0].apply_layout_us;
        passes[4] += frames[0].draw_us;
        // Wall time for the whole keypress. It counts the second frame the
        // live app does not run, so it is an upper bound — but unlike the
        // pass sum it also counts what happens *between* passes, which
        // includes the undo commit. The truth is between the two columns.
        passes[5] += wall;
        per_char.push(events + paint);
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
    let expected = paragraphs * PARAGRAPH.chars().count() + "warmup".len() + chars;
    assert!(
        total >= expected,
        "only {total} characters are in the document, expected at least \
         {expected}: the harness typed into something that did not keep the text",
    );

    let n = per_char.len().max(1) as u64;
    for p in &mut passes {
        *p /= n;
    }
    (per_char, passes)
}

/// Print the distribution of `per_char`, plus how the cost moves from the start
/// of the run to the end — a document-size effect shows up there before it
/// shows up in the percentiles.
fn report(label: &str, per_char: &[u64]) {
    let mut sorted = per_char.to_vec();
    sorted.sort_unstable();
    let ms = |us: u64| us as f64 / 1000.0;
    let mean = per_char.iter().sum::<u64>() / per_char.len().max(1) as u64;

    let quarter = per_char.len() / 4;
    let first = &per_char[..quarter.max(1)];
    let last = &per_char[per_char.len() - quarter.max(1)..];
    let avg = |v: &[u64]| v.iter().sum::<u64>() / v.len().max(1) as u64;

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
        avg(last) as f64 / avg(first).max(1) as f64,
        over,
        over as f64 * 100.0 / per_char.len() as f64,
    );

    // Where the slow characters fall matters as much as how slow they are. A
    // stall that recurs on a fixed stride is periodic work on the typing path;
    // one that creeps upward is cost that grows with the document. The
    // percentiles cannot tell those two apart, and they need different fixes.
    let p50 = percentile(&sorted, 50.0).max(1);
    let spikes: Vec<usize> = per_char
        .iter()
        .enumerate()
        .filter(|&(_, &us)| us > p50 * 4)
        .map(|(i, _)| i)
        .collect();
    if !spikes.is_empty() {
        let gaps: Vec<usize> = spikes.windows(2).map(|w| w[1] - w[0]).collect();
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
            &gaps[..gaps.len().min(11)],
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
fn paced_burst_profile(paragraphs: usize, chars: usize, per_second: f64) -> Vec<usize> {
    let path = document_with(paragraphs);
    let (state, wp, _fmt) = open_document(&path).expect("the document opens");
    let mut driver = Driver::new(state, Box::new(wp), (1200, 800));
    driver.type_text("warmup");

    let interval = std::time::Duration::from_secs_f64(1.0 / per_second);
    let text: Vec<char> = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .collect();

    let start = Instant::now();
    let mut done_at = Vec::with_capacity(chars);
    for (i, ch) in text.into_iter().enumerate() {
        // The key is due at `i · interval`. If the editor is already past that,
        // the key is waiting and goes in immediately.
        let due = interval.mul_f64(i as f64);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        driver.send(press(ch));
        done_at.push(start.elapsed());
    }

    let frames = (done_at.last().copied().unwrap_or_default().as_micros() / 16_667) as usize + 1;
    let mut per_frame = vec![0usize; frames];
    for t in done_at {
        let f = ((t.as_micros() / 16_667) as usize).min(frames - 1);
        per_frame[f] += 1;
    }
    per_frame
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
        clumped as f64 * 100.0 / typed.max(1) as f64,
    );
    // The shape itself, so a burst is legible rather than summarised: one
    // column per display frame, capped at 9 so a clump reads at a glance.
    let shape: String = per_frame
        .iter()
        .map(|&n| match n {
            0 => '.',
            n if n < 10 => char::from_digit(n as u32, 10).unwrap(),
            _ => '#',
        })
        .collect();
    for line in shape.as_bytes().chunks(120) {
        println!("      {}", String::from_utf8_lossy(line));
    }
}

fn main() {
    ui::i18n::I18nManager::init("en-US");

    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
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
        let (per_char, passes) = type_and_time(paragraphs, chars);
        report(&format!("{paragraphs} paragraph(s) of body"), &per_char);
        let ms = |us: u64| us as f64 / 1000.0;
        println!(
            "{:>28}   mean per character: handle_event {:.2}  sync {:.2}  \
             build {:.2}  apply {:.2}  draw {:.2}  | whole keypress, wall {:.2}  (ms)",
            "",
            ms(passes[0]),
            ms(passes[1]),
            ms(passes[2]),
            ms(passes[3]),
            ms(passes[4]),
            ms(passes[5]),
        );
    }

    println!("\ntyping at a person's pace: what reaches the screen, frame by frame\n");
    for paragraphs in [0usize, 4, 16] {
        let per_frame = paced_burst_profile(paragraphs, chars.min(120), rate);
        report_bursts(
            &format!("{paragraphs} paragraph(s) of body"),
            &per_frame,
            rate,
        );
    }
}
