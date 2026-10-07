//! What one keystroke costs in [`BlockEditor`] as the document grows.
//!
//! Two hosts type the same text into documents of 200, 2000 and 10000
//! paragraphs:
//!
//! - **harness**: the headless [`FrameHarness`], which rebuilds its layout
//!   tree every frame (what `TestApp` and the SDK latency example measure);
//! - **retained**: the live host's lifecycle, which keeps one layout tree
//!   between keystrokes and rebuilds it only on a structural change.
//!
//! Each row is the mean per keystroke of event handling, layout (build,
//! solve and apply) and draw, plus the p50 and p95 of their sum.
//!
//! ```text
//! cargo run -p tilo-ui --release --example block-editor-typing
//! cargo run -p tilo-ui --release --example block-editor-typing -- --chars 120 --sizes 200,2000
//! ```
//!
//! Release only: a debug build makes every reading meaningless.

use std::collections::HashMap;
use tilo_ui::time::Instant;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::State;
use tilo_ui::bound_dispatch::BoundNodeMap;
use tilo_ui::headless::FrameHarness;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::widget::{BlockEditor, CursorPlacement, Widget};
use tilo_ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};

/// A paragraph of ordinary prose: what a long note is made of.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much.";

const VIEWPORT: (u32, u32) = (1200, 800);

fn document(count: usize) -> String {
    let mut out = String::new();
    for _ in 0..count {
        out.push_str(PARAGRAPH);
        out.push_str("\n\n");
    }
    out
}

fn typed_text(chars: usize) -> Vec<String> {
    "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .map(String::from)
        .collect()
}

/// Microseconds per keystroke in each pass.
#[derive(Default, Clone, Copy)]
struct Sample {
    event: u64,
    layout: u64,
    draw: u64,
}

impl Sample {
    fn total(self) -> u64 {
        self.event
            .saturating_add(self.layout)
            .saturating_add(self.draw)
    }
}

fn micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Microseconds as `f64` milliseconds for display.
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

fn report(host: &str, paragraphs: usize, samples: &[Sample]) {
    let count = u64::try_from(samples.len()).unwrap_or(u64::MAX).max(1);
    let mean = |pick: fn(&Sample) -> u64| {
        samples
            .iter()
            .map(pick)
            .fold(0u64, u64::saturating_add)
            .checked_div(count)
            .unwrap_or(0)
    };
    let mut totals: Vec<u64> = samples.iter().map(|sample| sample.total()).collect();
    totals.sort_unstable();
    println!(
        "{host:<9} {paragraphs:>6} paragraphs  p50 {:>8.3} ms  p95 {:>8.3} ms  \
         (mean: event {:.3}, layout {:.3}, draw {:.3} ms)",
        ms(percentile(&totals, 50)),
        ms(percentile(&totals, 95)),
        ms(mean(|sample| sample.event)),
        ms(mean(|sample| sample.layout)),
        ms(mean(|sample| sample.draw)),
    );
}

fn harness(paragraphs: usize, chars: usize) -> Vec<Sample> {
    let editor = BlockEditor::from_markdown(&document(paragraphs)).with_wrap_lines(true);
    let mut harness = FrameHarness::new(State::new(), Box::new(editor), VIEWPORT);
    harness.click_at(40.0, 10.0);
    harness.type_text("warmup ");
    harness.begin_profiling();
    let mut samples = Vec::with_capacity(chars);
    for character in typed_text(chars) {
        harness.clear_profile_samples();
        harness.key(&character);
        let frames = harness.take_profile_samples();
        let mut sample = Sample {
            event: frames.iter().map(|frame| frame.handle_event_us).sum(),
            ..Sample::default()
        };
        if let Some(frame) = frames.first() {
            sample.layout = frame.build_layout_us.saturating_add(frame.apply_layout_us);
            sample.draw = frame.draw_us;
        }
        samples.push(sample);
    }
    samples
}

/// The live host's frame lifecycle: one retained tree, rebuilt only when the
/// editor reports a structural change.
struct Retained {
    editor: Box<BlockEditor>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: NodeId,
    text: TextRenderer,
    theme: Theme,
    doc: AutoCommit,
}

impl Retained {
    fn new(paragraphs: usize) -> Self {
        let mut editor =
            Box::new(BlockEditor::from_markdown(&document(paragraphs)).with_wrap_lines(true));
        editor.focus_block(0, CursorPlacement::Start);
        let mut fixture = Self {
            editor,
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            bindings: BoundNodeMap::new(),
            root: NodeId::new(0),
            text: TextRenderer::new(),
            theme: Theme::dark(),
            doc: AutoCommit::new(),
        };
        fixture.rebuild();
        fixture.frame();
        fixture
    }

    fn rebuild(&mut self) {
        self.tree = TaffyTree::new();
        self.nodes.clear();
        self.root = self
            .editor
            .build_layout(&mut self.tree, &self.theme, &mut self.nodes);
        self.bindings = BoundNodeMap::new();
        // SAFETY: the boxed editor owns every node and stays alive and in place.
        unsafe {
            self.bindings
                .rebuild(self.editor.as_ref(), &self.tree, self.root, &self.nodes);
        }
    }

    /// One frame; returns (layout, draw) microseconds.
    fn frame(&mut self) -> (u64, u64) {
        let started = tilo_ui::time::now();
        if self.editor.take_structure_dirty() {
            self.rebuild();
        }
        if self.editor.layout_needs_measure() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "the viewport is a few thousand pixels"
            )]
            let available = Size {
                width: AvailableSpace::Definite(VIEWPORT.0 as f32),
                height: AvailableSpace::Definite(VIEWPORT.1 as f32),
            };
            // SAFETY: the tree and maps were built from this exact editor.
            let solved = unsafe {
                tilo_ui::widget::compute_widget_layout(
                    &mut self.tree,
                    self.root,
                    available,
                    &self.nodes,
                    &self.bindings,
                    &mut self.text,
                )
            };
            if let Err(error) = solved {
                eprintln!("editor layout did not settle: {error}");
            }
        }
        self.editor.apply_layout(&self.tree, self.root, Point::ZERO);
        let layout = micros(started);
        let started = tilo_ui::time::now();
        // The live host culls against the window, as this does.
        let presence = PresenceCtx::empty();
        presence.push_viewport(Rect::new(
            0.0,
            0.0,
            f64::from(VIEWPORT.0),
            f64::from(VIEWPORT.1),
        ));
        self.editor.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
        );
        self.editor.mark_drawn();
        (layout, micros(started))
    }

    fn key(&mut self, character: &str) -> Sample {
        let key = Key::Character(character.into());
        let started = tilo_ui::time::now();
        for state in [ElementState::Pressed, ElementState::Released] {
            let text = (state == ElementState::Pressed).then(|| character.into());
            let event = KeyEvent::synthetic(
                RawKeyEvent {
                    physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    state,
                },
                key.clone(),
                text.clone(),
                KeyLocation::Standard,
                false,
                text,
                key.clone(),
            );
            let mut handled = false;
            self.editor.handle_event(
                &WindowEvent::KeyboardInput {
                    device_id: DeviceId::dummy(),
                    event,
                    is_synthetic: true,
                },
                &mut tilo_ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &Modifiers::default(),
                    state: tilo_ui::widget::Edits::lend(&mut self.doc),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut None,
                    node_id: self.root,
                    taffy: &self.tree,
                },
            );
        }
        let event = micros(started);
        let (layout, draw) = self.frame();
        Sample {
            event,
            layout,
            draw,
        }
    }
}

fn retained(paragraphs: usize, chars: usize) -> Vec<Sample> {
    let mut fixture = Retained::new(paragraphs);
    for character in typed_text(7) {
        fixture.key(&character);
    }
    typed_text(chars)
        .iter()
        .map(|character| fixture.key(character))
        .collect()
}

fn main() {
    let mut chars = 60usize;
    let mut sizes = vec![200usize, 2000, 10000];
    let mut hosts = vec!["retained".to_owned(), "harness".to_owned()];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--chars" => {
                chars = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(chars);
            }
            "--sizes" => {
                sizes = args
                    .next()
                    .map(|value| value.split(',').filter_map(|n| n.parse().ok()).collect())
                    .unwrap_or(sizes);
            }
            "--hosts" => {
                hosts = args
                    .next()
                    .map(|value| value.split(',').map(str::to_owned).collect())
                    .unwrap_or(hosts);
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    for host in &hosts {
        for &paragraphs in &sizes {
            let samples = match host.as_str() {
                "harness" => harness(paragraphs, chars),
                "retained" => retained(paragraphs, chars),
                other => {
                    eprintln!("unknown host {other}");
                    continue;
                }
            };
            report(host, paragraphs, &samples);
        }
    }
}
