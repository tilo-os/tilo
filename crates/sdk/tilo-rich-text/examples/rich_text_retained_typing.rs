//! What one keystroke costs in the SDK rich-text editor in the live host's
//! frame lifecycle: one retained layout tree, rebuilt only when the editor
//! reports a structural change. The toolkit's block editor on the same
//! document is the baseline.
//!
//! ```text
//! cargo run -p tilo-rich-text --release --example rich_text_retained_typing
//! cargo run -p tilo-rich-text --release --example rich_text_retained_typing -- --chars 400
//! ```
//!
//! Release only: a debug build makes every reading meaningless.

use std::collections::HashMap;
use std::time::Instant;

use tilo_document::{RichText, RichTextAction};
use tilo_rich_text::{DirectField, RichTextSurface};
use ui::automerge::{AutoCommit, ROOT};
use ui::bound_dispatch::BoundNodeMap;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::tilo_actions::Action;
use ui::vello::Scene;
use ui::widget::{BlockEditor, CursorPlacement, Widget};
use ui::winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
use ui::winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
use ui::{TextRenderer, Theme};

/// A paragraph of ordinary prose: what a long note is made of.
const PARAGRAPH: &str = "The rehearsal ran long and the doors opened late, so the \
first half went out to a room that was still finding its seats. Nobody minded much.";

const VIEWPORT: (u32, u32) = (1200, 800);

fn markdown(count: usize) -> String {
    let mut out = String::new();
    for _ in 0..count {
        out.push_str(PARAGRAPH);
        out.push_str("\n\n");
    }
    out
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

/// Microseconds per keystroke in each pass.
#[derive(Default, Clone, Copy)]
struct Sample {
    event: u64,
    sync: u64,
    layout: u64,
    draw: u64,
}

impl Sample {
    fn total(self) -> u64 {
        self.event
            .saturating_add(self.sync)
            .saturating_add(self.layout)
            .saturating_add(self.draw)
    }
}

/// The live host's frame lifecycle around one widget.
struct Retained {
    widget: Box<dyn Widget>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: NodeId,
    text: TextRenderer,
    theme: Theme,
    doc: AutoCommit,
}

impl Retained {
    fn new(widget: Box<dyn Widget>, doc: AutoCommit) -> Self {
        let mut host = Self {
            widget,
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            bindings: BoundNodeMap::new(),
            root: NodeId::new(0),
            text: TextRenderer::new(),
            theme: Theme::dark(),
            doc,
        };
        host.sync();
        host.rebuild();
        host.frame();
        host
    }

    fn rebuild(&mut self) {
        self.tree = TaffyTree::new();
        self.nodes.clear();
        self.root = self
            .widget
            .build_layout(&mut self.tree, &self.theme, &mut self.nodes);
        self.bindings = BoundNodeMap::new();
        // SAFETY: the boxed widget owns every node and stays alive and in place.
        unsafe {
            self.bindings
                .rebuild(self.widget.as_ref(), &self.tree, self.root, &self.nodes);
        }
    }

    fn sync(&mut self) -> u64 {
        let started = Instant::now();
        self.widget.sync(&mut self.doc, &ROOT);
        micros(started)
    }

    /// One frame; returns (layout, draw) microseconds.
    fn frame(&mut self) -> (u64, u64) {
        let started = Instant::now();
        if self.widget.take_structure_dirty() {
            self.rebuild();
        }
        if self.widget.layout_needs_measure() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "the viewport is a few thousand pixels"
            )]
            let available = Size {
                width: AvailableSpace::Definite(VIEWPORT.0 as f32),
                height: AvailableSpace::Definite(VIEWPORT.1 as f32),
            };
            // SAFETY: the tree and maps were built from this exact widget.
            let solved = unsafe {
                ui::widget::compute_widget_layout(
                    &mut self.tree,
                    self.root,
                    available,
                    &self.nodes,
                    &self.bindings,
                    &mut self.text,
                )
            };
            if let Err(error) = solved {
                eprintln!("layout did not settle: {error}");
            }
        }
        self.widget.apply_layout(&self.tree, self.root, Point::ZERO);
        let layout = micros(started);
        let started = Instant::now();
        let presence = PresenceCtx::empty();
        presence.push_viewport(Rect::new(
            0.0,
            0.0,
            f64::from(VIEWPORT.0),
            f64::from(VIEWPORT.1),
        ));
        self.widget.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &presence,
        );
        self.widget.mark_drawn();
        (layout, micros(started))
    }

    fn key(&mut self, character: &str) -> Sample {
        let key = Key::Character(character.into());
        let started = Instant::now();
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
            self.widget.handle_event(
                &WindowEvent::KeyboardInput {
                    device_id: DeviceId::dummy(),
                    event,
                    is_synthetic: true,
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &Modifiers::default(),
                    state: &mut self.doc,
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut None,
                    node_id: self.root,
                    taffy: &self.tree,
                },
            );
        }
        let event = micros(started);
        let sync = self.sync();
        let (layout, draw) = self.frame();
        Sample {
            event,
            sync,
            layout,
            draw,
        }
    }
}

fn typed(host: &mut Retained, chars: usize) -> Vec<Sample> {
    for character in "warmup ".chars() {
        host.key(&character.to_string());
    }
    "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(chars)
        .map(|character| host.key(&character.to_string()))
        .collect()
}

fn block_editor(paragraphs: usize) -> Retained {
    let mut editor = BlockEditor::from_markdown(&markdown(paragraphs)).with_wrap_lines(true);
    editor.set_focus(true);
    editor.focus_block(0, CursorPlacement::Start);
    Retained::new(Box::new(editor), AutoCommit::new())
}

fn rich_text(paragraphs: usize) -> Result<Retained, String> {
    let mut doc = AutoCommit::new();
    RichText::initialize_under(&mut doc, &ROOT).map_err(|error| error.to_string())?;
    RichTextAction::ReplaceBody {
        markdown: markdown(paragraphs),
    }
    .apply(&mut doc)
    .map_err(|error| error.to_string())?;
    doc.commit();
    // The editor writing to the document itself: what the editor costs,
    // without an app's action host around it.
    let mut surface = RichTextSurface::over(DirectField::new(|_| Some(ROOT)))
        .with_editor(BlockEditor::new().with_wrap_lines(true));
    // The first sync reads the document and shows it.
    Widget::sync(&mut surface, &mut doc, &ROOT);
    surface.set_focus(true);
    surface.editor_mut().focus_block(0, CursorPlacement::Start);
    Ok(Retained::new(Box::new(surface), doc))
}

fn report(name: &str, samples: &[Sample]) {
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
        "{name:<30} p50 {:>8.3} ms  p95 {:>8.3} ms  \
         (mean: event {:.3}, sync {:.3}, layout {:.3}, draw {:.3} ms)",
        ms(percentile(&totals, 50)),
        ms(percentile(&totals, 95)),
        ms(mean(|sample| sample.event)),
        ms(mean(|sample| sample.sync)),
        ms(mean(|sample| sample.layout)),
        ms(mean(|sample| sample.draw)),
    );
}

fn main() -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let chars = std::env::args()
        .skip_while(|arg| arg != "--chars")
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    for paragraphs in [400, 2000] {
        println!("{paragraphs} paragraphs, {chars} characters typed:");
        report(
            "  block editor",
            &typed(&mut block_editor(paragraphs), chars),
        );
        report(
            "  SDK rich-text editor",
            &typed(&mut rich_text(paragraphs)?, chars),
        );
    }
    Ok(())
}
