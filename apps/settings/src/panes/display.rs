//! Display pane — brightness slider + "Auto brightness" placeholder
//! toggle.
//!
//! Plumbs `os_actions::display::{get_brightness, set_brightness}`. The
//! initial value is sampled once on construction (cached in
//! [`DisplayPane::initial_brightness`]); subsequent drag-driven CRDT
//! updates are observed in `sync` and forwarded to
//! [`os_actions::Dispatcher::set_brightness`] only when the value
//! actually changes — that keeps `brightnessctl` from firing on every
//! frame just because the slider committed an identical f64.
//!
//! Auto-brightness has no real backing yet; the toggle is wired to a
//! CRDT key (`display.auto_brightness`) so the bit is captured, but no
//! dispatcher hook fires when it flips. Tracked in the pane docstring
//! so the omission is visible to anyone wiring real ambient-light
//! handling later.
//
// CRDT keys this pane reads / writes on `state.doc`:
//   `display.brightness`        — f64 in `[0.0, 1.0]`, slider value
//   `display.auto_brightness`   — bool, auto-brightness toggle stub

use std::collections::HashMap;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use os_actions::Dispatcher;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Column, Row, Slider, Spacer, Text, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;

/// CRDT key the brightness slider writes into.
pub const BRIGHTNESS_KEY: &str = "display.brightness";
/// CRDT key the auto-brightness toggle writes into.
pub const AUTO_BRIGHTNESS_KEY: &str = "display.auto_brightness";

/// Display pane widget.
///
/// Owns the dispatcher used to forward slider changes onto
/// `brightnessctl`. Tests inject a recording dispatcher via
/// [`DisplayPane::with_dispatcher`].
pub struct DisplayPane {
    /// Side-effect surface. Default is `RealDispatcher`; tests pass a
    /// recording impl.
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    /// Initial brightness, sampled once on construction. Pushed into
    /// the CRDT on the first `sync` so the slider rests at the right
    /// position on first frame.
    initial_brightness: f32,
    /// Last observed brightness on the CRDT, used to detect changes
    /// without re-dispatching every frame.
    last_brightness: f32,
    /// Same idea for the auto-brightness toggle.
    last_auto_brightness: bool,
    /// `true` while we still need to push `initial_brightness` into the
    /// CRDT. Cleared after the first successful `sync`.
    needs_seed: bool,
    /// Composed child tree, rebuilt each `build_layout` so a CRDT
    /// change immediately re-skins the widgets.
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for DisplayPane {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayPane {
    /// Construct a pane wired to the real dispatcher. Samples the
    /// current brightness once via [`os_actions::get_brightness`] —
    /// failures fall back to `0.5` so the slider still has a
    /// reasonable rest position on hosts without `brightnessctl`.
    pub fn new() -> Self {
        let initial = os_actions::get_brightness().unwrap_or(0.5);
        Self {
            dispatcher: dispatcher_or_real(None),
            initial_brightness: initial,
            last_brightness: initial,
            last_auto_brightness: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Construct a pane with a caller-supplied dispatcher (used by
    /// tests). The initial brightness is taken from
    /// `initial_brightness` so test fixtures don't depend on the host
    /// machine.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_brightness: f32,
    ) -> Self {
        Self {
            dispatcher,
            initial_brightness,
            last_brightness: initial_brightness,
            last_auto_brightness: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let row = Row::new()
            .push(Box::new(Text::new("Brightness").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Slider::new("settings.display.brightness", BRIGHTNESS_KEY)
                    .with_id("display.brightness"),
            ));
        let auto_row = Row::new()
            .push(Box::new(Text::new("Auto brightness").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Toggle::new("settings.display.auto_brightness", AUTO_BRIGHTNESS_KEY)
                    .with_id("display.auto_brightness"),
            ));
        Box::new(
            Column::new()
                .push(Box::new(row))
                .push(Box::new(auto_row))
                .push(Box::new(
                    Text::new(
                        "Drag the slider to change the backlight brightness. \
                         Auto-brightness is a placeholder — no ambient-light \
                         binding yet.",
                    )
                    .with_size(11.0),
                )),
        )
    }
}

/// Public entry point — the SettingsApp constructs this when the user
/// selects the Display category. Matches the historical `build()`
/// signature so the dispatch table in `settings_app::build_pane` keeps
/// compiling without changes.
pub fn build() -> Box<dyn Widget> {
    Box::new(DisplayPane::new())
}

impl Widget for DisplayPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Seed the CRDT with the initial brightness exactly once so the
        // slider rests where it should. The seed lives behind a
        // `needs_seed` flag rather than checking "is the key absent"
        // because the slider's own `sync` could fill it with the
        // default `0.5` before we run.
        if self.needs_seed {
            let _ = state.put(
                root,
                BRIGHTNESS_KEY,
                automerge::ScalarValue::F64(self.initial_brightness as f64),
            );
            let _ = state.put(
                root,
                AUTO_BRIGHTNESS_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            self.needs_seed = false;
        }

        // Read the CRDT and forward changes to the dispatcher. Cached
        // `last_brightness` guards against re-dispatching on identical
        // commits (the Slider widget already debounces by ε, but the
        // first `sync` of every frame still walks the path).
        if let Ok(Some((val, _))) = state.get(root, BRIGHTNESS_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::F64(f) = s.as_ref() {
                    let new = *f as f32;
                    if (new - self.last_brightness).abs() > 0.001 {
                        self.last_brightness = new;
                        if let Err(e) =
                            self.dispatcher.set_brightness((new.clamp(0.0, 1.0)) * 100.0)
                        {
                            warn!(target: "qos::settings", op = "set_brightness", error = %e, "display pane dispatch failed");
                        }
                    }
                }
            }
        }
        if let Ok(Some((val, _))) = state.get(root, AUTO_BRIGHTNESS_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    self.last_auto_brightness = *b;
                }
            }
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cn,
                        taffy,
                    );
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "DisplayPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    /// The pane forwards a slider-committed brightness onto the
    /// dispatcher. We bypass the Slider widget here (it commits via
    /// real winit events) and simulate the same CRDT mutation directly,
    /// then drive `sync` to provoke the dispatch.
    #[test]
    fn brightness_slider_invokes_set_brightness_on_drag() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        // First sync seeds the CRDT and observes the seed — should not
        // dispatch (last_brightness == initial).
        pane.sync(&mut doc, &root);
        assert!(
            rec.calls().is_empty(),
            "seed sync should not dispatch: {:?}",
            rec.calls()
        );

        // Simulate the slider committing a new value to the CRDT.
        let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
        pane.sync(&mut doc, &root);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1, "expected one dispatch, got {:?}", calls);
        assert!(
            calls[0].starts_with("set_brightness("),
            "expected set_brightness, got {:?}",
            calls[0]
        );
        // 0.75 * 100 → 75.00
        assert!(
            calls[0].contains("75."),
            "expected 75% in {:?}",
            calls[0]
        );
    }

    /// A second sync with the same CRDT value must not re-dispatch.
    /// Otherwise every frame would shell out to brightnessctl while the
    /// slider is at rest.
    #[test]
    fn brightness_unchanged_does_not_redispatch() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, BRIGHTNESS_KEY, automerge::ScalarValue::F64(0.75));
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls().len(), 1);
    }

    /// Seed sets `display.brightness` from the constructor argument on
    /// the first `sync`, so the Slider widget reads the right rest
    /// position.
    #[test]
    fn seed_writes_initial_brightness_into_crdt() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            DisplayPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.42);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let (val, _) = doc.get(&root, BRIGHTNESS_KEY).unwrap().unwrap();
        match val {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::F64(f) => {
                    assert!((f - 0.42).abs() < 1e-6, "got {f}");
                }
                other => panic!("expected F64 scalar, got {other:?}"),
            },
            other => panic!("expected scalar, got {other:?}"),
        }
    }
}
