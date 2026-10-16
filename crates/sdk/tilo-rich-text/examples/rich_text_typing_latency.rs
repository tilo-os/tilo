//! What one keystroke costs in the SDK rich-text editor, on a long note and
//! a long chat, against the toolkit's block editor editing locally and
//! handing its owner semantic block operations each frame (the path notes and
//! the calendar type through). Both run in the same framework host, so the
//! difference is the editor's own.
//!
//! A keystroke is both of its events plus one frame's sync, layout and draw,
//! from the harness's own per-pass profiler — what the live app spends
//! before the glyph is on screen.
//!
//! ```text
//! cargo run -p tilo-rich-text --release --example rich_text_typing_latency
//! cargo run -p tilo-rich-text --release --example rich_text_typing_latency -- --chars 400
//! ```
//!
//! Release only: a debug build makes every reading meaningless.

use std::time::Instant;
use ui::widget::{AppModel, ModelPath};

use tilo_document::{RichText, RichTextAction};
use tilo_rich_text::RichTextViews;
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::headless::FrameHarness;
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource};
use ui::widget::{BlockEditor, Decorator, Element, Widget};

/// A paragraph of ordinary prose: what a long note is made of.
const NOTE_PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much.";
/// A chat line: what a long conversation is made of.
const CHAT_LINE: &str = "sure, send it over and I'll take a look after lunch";

struct Document;

impl ResourceApp for Document {
    type Resource = RichText;
    const ID: &'static str = "so.tilo.rich-text-latency";
    const TITLE: &'static str = "Rich text";
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 800)
    }

    fn view(cx: &mut ViewCx<RichText>) -> Element {
        cx.rich_text()
    }
}

/// The document the block editor starts from, set before each run.
static SEED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A record holding nothing the framework reads: the block editor keeps its
/// blocks locally.
struct Blocks;

impl Resource for Blocks {
    const KIND: &'static str = "example.blocks";
    const LABEL: &'static str = "Blocks";
    type State = ();
    type Action = RichTextAction;

    fn read(_doc: &Doc) -> Result<(), ModelError> {
        Ok(())
    }
}

/// The toolkit's block editor as an action owner hosts it: edits stay local
/// and the owner drains them as semantic operations every frame.
struct Direct {
    editor: BlockEditor,
}

impl Decorator for Direct {
    type Inner = BlockEditor;

    fn inner(&self) -> &BlockEditor {
        &self.editor
    }

    fn inner_mut(&mut self) -> &mut BlockEditor {
        &mut self.editor
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // An owner would submit these as actions; draining them keeps the
        // editor's own diffing cost in the reading.
        std::hint::black_box(self.editor.take_edit_ops());
        self.editor.sync(state, path);
    }
}

struct DirectApp;

impl ResourceApp for DirectApp {
    type Resource = Blocks;
    const ID: &'static str = "so.tilo.block-editor-latency";
    const TITLE: &'static str = "Blocks";
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 800)
    }

    fn view(_cx: &mut ViewCx<Blocks>) -> Element {
        let seed = SEED.lock().map(|seed| seed.clone()).unwrap_or_default();
        Box::new(Direct {
            editor: BlockEditor::from_markdown(&seed).with_wrap_lines(true),
        })
    }
}

fn body(paragraph: &str, count: usize) -> String {
    let mut out = String::new();
    for _ in 0..count {
        out.push_str(paragraph);
        out.push_str("\n\n");
    }
    out
}

/// Microseconds as `f64` milliseconds for display; timings never approach
/// 2^53 µs.
#[allow(
    clippy::cast_precision_loss,
    reason = "microsecond timings never approach 2^53"
)]
fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    let rank = sorted
        .len()
        .saturating_mul(percent)
        .div_ceil(100)
        .saturating_sub(1);
    sorted.get(rank).copied().unwrap_or(0)
}

/// Mean microseconds per keystroke spent in each pass.
#[derive(Default)]
struct Passes {
    handle_event: u64,
    sync: u64,
    layout: u64,
    draw: u64,
}

/// What typing cost: every keystroke's total, and the mean per pass.
struct Typed {
    costs: Vec<u64>,
    passes: Passes,
}

/// Type `chars` characters into `harness`, timing each.
fn type_and_time(harness: &mut FrameHarness, chars: usize) -> Typed {
    harness.type_text("warmup ");
    harness.begin_profiling();
    let text: Vec<char> = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .collect();
    let mut costs = Vec::with_capacity(chars);
    let mut passes = Passes::default();
    for ch in text {
        harness.clear_profile_samples();
        let started = Instant::now();
        harness.key(&ch.to_string());
        let wall = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let frames = harness.take_profile_samples();
        let events: u64 = frames.iter().map(|frame| frame.handle_event_us).sum();
        if let Some(frame) = frames.first() {
            passes.handle_event = passes.handle_event.saturating_add(events);
            passes.sync = passes.sync.saturating_add(frame.sync_us);
            passes.layout = passes
                .layout
                .saturating_add(frame.build_layout_us.saturating_add(frame.apply_layout_us));
            passes.draw = passes.draw.saturating_add(frame.draw_us);
        }
        let paint = frames.first().map_or(0, |frame| {
            frame
                .sync_us
                .saturating_add(frame.build_layout_us)
                .saturating_add(frame.apply_layout_us)
                .saturating_add(frame.draw_us)
        });
        let measured = events.saturating_add(paint);
        costs.push(if measured == 0 { wall } else { measured });
    }
    let typed = u64::try_from(costs.len()).unwrap_or(u64::MAX).max(1);
    let mean = |total: u64| total.checked_div(typed).unwrap_or(0);
    Typed {
        passes: Passes {
            handle_event: mean(passes.handle_event),
            sync: mean(passes.sync),
            layout: mean(passes.layout),
            draw: mean(passes.draw),
        },
        costs,
    }
}

fn click_into(harness: &mut FrameHarness) {
    if let Some(rect) = harness.targets().get("block_editor").copied() {
        harness.click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    }
}

fn rich_text(paragraph: &str, count: usize, chars: usize) -> Result<Typed, String> {
    let mut app = TestApp::<Document>::new().map_err(|error| error.to_string())?;
    let seed = Invoke::<RichText>::apply(&RichTextAction::ReplaceBody {
        markdown: body(paragraph, count),
    })
    .map_err(|error| error.to_string())?;
    app.invoke(seed).map_err(|error| error.to_string())?;
    click_into(app.harness());
    Ok(type_and_time(app.harness(), chars))
}

fn direct(paragraph: &str, count: usize, chars: usize) -> Result<Typed, String> {
    if let Ok(mut seed) = SEED.lock() {
        *seed = body(paragraph, count);
    }
    let mut app = TestApp::<DirectApp>::new().map_err(|error| error.to_string())?;
    click_into(app.harness());
    Ok(type_and_time(app.harness(), chars))
}

fn report(name: &str, typed: Typed) {
    let Typed { mut costs, passes } = typed;
    costs.sort_unstable();
    println!(
        "{name:<34} p50 {:>7.3} ms  p95 {:>7.3} ms  max {:>7.3} ms  \
         (mean: event {:.3}, sync {:.3}, layout {:.3}, draw {:.3} ms)",
        ms(percentile(&costs, 50)),
        ms(percentile(&costs, 95)),
        ms(costs.last().copied().unwrap_or(0)),
        ms(passes.handle_event),
        ms(passes.sync),
        ms(passes.layout),
        ms(passes.draw),
    );
}

fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let chars = std::env::args()
        .skip_while(|arg| arg != "--chars")
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    for (label, paragraph, count) in [
        ("long note (400 paragraphs)", NOTE_PARAGRAPH, 400),
        ("long chat (2000 lines)", CHAT_LINE, 2000),
    ] {
        println!("{label}, {chars} characters typed:");
        report(
            "  block editor, semantic ops",
            direct(paragraph, count, chars)?,
        );
        report(
            "  SDK rich-text editor (tilo-doc)",
            rich_text(paragraph, count, chars)?,
        );
    }
    Ok(())
}
