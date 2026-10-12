//! What one keystroke costs in a multiline [`TextInput`] as the text grows.
//!
//! Scratchpad's whole UI and Mail's plain compose body are this widget: a
//! multiline input inside a vertical [`Scrollable`]. Two hosts type the same
//! text into notes of about 1, 10 and 100 KB of prose paragraphs:
//!
//! - **harness**: the headless [`FrameHarness`], which rebuilds its layout
//!   tree every frame (what `TestApp` and the SDK latency example measure);
//! - **retained**: the live host's lifecycle, which keeps one layout tree
//!   between keystrokes and re-solves it when a widget asks to be measured.
//!
//! Each row is the mean per keystroke of event handling, layout (build,
//! solve and apply) and draw, plus the p50 and p95 of their sum.
//!
//! ```text
//! cargo run -p ui-examples --release --example multiline-typing
//! cargo run -p ui-examples --release --example multiline-typing -- --chars 120 --sizes 10,100
//! ```
//!
//! Release only: a debug build makes every reading meaningless.

use std::collections::HashMap;
use tilo_ui::time::Instant;

use automerge::AutoCommit;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use tilo_ui::State;
use tilo_ui::bound_dispatch::BoundNodeMap;
use tilo_ui::headless::FrameHarness;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::widget::{Scrollable, TextInput, Widget};
use tilo_ui::{TextRenderer, TextRole, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};

/// A paragraph of ordinary prose: what a long note is made of.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much.";

const VIEWPORT: (u32, u32) = (1200, 800);

/// Prose paragraphs separated by blank lines, at least `kilobytes` KiB long.
fn note(kilobytes: usize) -> String {
    let mut out = String::new();
    while out.len() < kilobytes.saturating_mul(1024) {
        out.push_str(PARAGRAPH);
        out.push_str("\n\n");
    }
    out
}

/// The byte where typing starts: the end of the third paragraph, which is on
/// screen in a note of any size (a person types where they can see).
fn caret_byte(text: &str) -> usize {
    text.match_indices("\n\n")
        .nth(2)
        .map_or(text.len(), |(at, _)| at)
}

fn editor(text: &str) -> TextInput {
    TextInput::local()
        .with_initial_value(text)
        .with_plain_chrome()
        .with_text_role(TextRole::Reading)
        .multiline(true)
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

fn report(host: &str, kilobytes: usize, samples: &[Sample]) {
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
        "{host:<9} {kilobytes:>4} KB  p50 {:>8.3} ms  p95 {:>8.3} ms  \
         (mean: event {:.3}, layout {:.3}, draw {:.3} ms)",
        ms(percentile(&totals, 50)),
        ms(percentile(&totals, 95)),
        ms(mean(|sample| sample.event)),
        ms(mean(|sample| sample.layout)),
        ms(mean(|sample| sample.draw)),
    );
}

fn harness(kilobytes: usize, chars: usize) -> Vec<Sample> {
    let root = Scrollable::new_vertical()
        .fill_parent_height()
        .with_content_min_viewport_height()
        .with_child(Box::new(editor(&note(kilobytes))));
    let mut harness = FrameHarness::new(State::new(), Box::new(root), VIEWPORT);
    // Into the second paragraph, which is on screen at every size.
    harness.click_at(200.0, 60.0);
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

/// The live host's frame lifecycle: one retained tree, re-solved when the
/// input reports that its height may have changed.
struct Retained {
    root: Box<Scrollable>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    node: NodeId,
    /// The window-sized node the root is laid out in, as the hosts give it.
    window: NodeId,
    text: TextRenderer,
    theme: Theme,
    doc: AutoCommit,
}

impl Retained {
    fn new(kilobytes: usize) -> Result<Self, taffy::TaffyError> {
        let text = note(kilobytes);
        let mut input = editor(&text);
        input.set_focus(true);
        input.set_cursor_pos(caret_byte(&text));
        let root = Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_content_min_viewport_height()
                .with_child(Box::new(input)),
        );
        let mut fixture = Self {
            root,
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            bindings: BoundNodeMap::new(),
            node: NodeId::new(0),
            window: NodeId::new(0),
            text: TextRenderer::new(),
            theme: Theme::dark(),
            doc: AutoCommit::new(),
        };
        fixture.node =
            fixture
                .root
                .build_layout(&mut fixture.tree, &fixture.theme, &mut fixture.nodes);
        #[allow(
            clippy::cast_precision_loss,
            reason = "the viewport is a few thousand pixels"
        )]
        let window = Style {
            size: Size {
                width: Dimension::length(VIEWPORT.0 as f32),
                height: Dimension::length(VIEWPORT.1 as f32),
            },
            ..Style::default()
        };
        fixture.window = fixture.tree.new_with_children(window, &[fixture.node])?;
        // SAFETY: the boxed root owns every node and stays alive and in place.
        unsafe {
            fixture.bindings.rebuild(
                fixture.root.as_ref(),
                &fixture.tree,
                fixture.node,
                &fixture.nodes,
            );
        }
        fixture.solve();
        fixture.frame();
        Ok(fixture)
    }

    fn solve(&mut self) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "the viewport is a few thousand pixels"
        )]
        let available = Size {
            width: AvailableSpace::Definite(VIEWPORT.0 as f32),
            height: AvailableSpace::Definite(VIEWPORT.1 as f32),
        };
        // SAFETY: the tree and maps were built from this exact root.
        let solved = unsafe {
            tilo_ui::widget::compute_widget_layout(
                &mut self.tree,
                self.window,
                available,
                &self.nodes,
                &self.bindings,
                &mut self.text,
            )
        };
        if let Err(error) = solved {
            eprintln!("input layout did not settle: {error}");
        }
    }

    /// One frame; returns (layout, draw) microseconds.
    fn frame(&mut self) -> (u64, u64) {
        let started = tilo_ui::time::now();
        if self.root.layout_needs_measure() {
            self.solve();
        }
        self.root.apply_layout(&self.tree, self.node, Point::ZERO);
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
        self.root.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
        );
        self.root.mark_drawn();
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
            self.root.handle_event(
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
                    node_id: self.node,
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

fn retained(kilobytes: usize, chars: usize) -> Vec<Sample> {
    let mut fixture = match Retained::new(kilobytes) {
        Ok(fixture) => fixture,
        Err(error) => {
            eprintln!("cannot lay the window out: {error}");
            return Vec::new();
        }
    };
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
    let mut sizes = vec![1usize, 10, 100];
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
        for &kilobytes in &sizes {
            let samples = match host.as_str() {
                "harness" => harness(kilobytes, chars),
                "retained" => retained(kilobytes, chars),
                other => {
                    eprintln!("unknown host {other}");
                    continue;
                }
            };
            report(host, kilobytes, &samples);
        }
    }
}
