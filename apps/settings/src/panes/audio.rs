//! Audio pane — volume slider, mute toggle, output device dropdown
//! placeholder.
//!
//! Plumbs `os_actions::audio::{get_volume, set_volume, set_mute}`. The
//! `Output device` row is a stub today: enumerating wpctl sinks
//! programmatically is future work; we render a static "Built-in audio"
//! row so the surface advertises where device selection will land.
//
// CRDT keys this pane reads / writes:
//   `audio.volume` — f64 in `[0.0, 1.0]`, slider value
//   `audio.muted`  — bool, mute toggle

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

pub const VOLUME_KEY: &str = "audio.volume";
pub const MUTED_KEY: &str = "audio.muted";

/// Audio pane widget.
pub struct AudioPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    initial_volume: f32,
    last_volume: f32,
    last_muted: bool,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AudioPane {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPane {
    pub fn new() -> Self {
        let initial = os_actions::get_volume().unwrap_or(0.5);
        Self {
            dispatcher: dispatcher_or_real(None),
            initial_volume: initial,
            last_volume: initial,
            last_muted: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_volume: f32,
    ) -> Self {
        Self {
            dispatcher,
            initial_volume,
            last_volume: initial_volume,
            last_muted: false,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let row = Row::new()
            .push(Box::new(Text::new("Volume").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Slider::new("settings.audio.volume", VOLUME_KEY).with_id("audio.volume"),
            ));
        let mute_row = Row::new()
            .push(Box::new(Text::new("Mute").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Toggle::new("settings.audio.mute", MUTED_KEY).with_id("audio.mute"),
            ));
        // Output device — single static row for now. Real enumeration is
        // a wpctl-status parsing job we haven't wired yet; surfacing the
        // row keeps the layout honest about where the picker will land.
        let device_row = Row::new()
            .push(Box::new(Text::new("Output device").with_size(12.0)))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Text::new("Built-in audio").with_size(12.0)));
        Box::new(
            Column::new()
                .push(Box::new(row))
                .push(Box::new(mute_row))
                .push(Box::new(device_row))
                .push(Box::new(
                    Text::new(
                        "Volume drives the default PipeWire sink; device \
                         enumeration is forthcoming.",
                    )
                    .with_size(11.0),
                )),
        )
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AudioPane::new())
}

impl Widget for AudioPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(
                root,
                VOLUME_KEY,
                automerge::ScalarValue::F64(self.initial_volume as f64),
            );
            let _ = state.put(root, MUTED_KEY, automerge::ScalarValue::Boolean(false));
            self.needs_seed = false;
        }
        if let Ok(Some((val, _))) = state.get(root, VOLUME_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::F64(f) = s.as_ref() {
                    let new = *f as f32;
                    if (new - self.last_volume).abs() > 0.001 {
                        self.last_volume = new;
                        if let Err(e) =
                            self.dispatcher.set_volume((new.clamp(0.0, 1.0)) * 100.0)
                        {
                            warn!(target: "qos::settings", op = "set_volume", error = %e, "audio pane dispatch failed");
                        }
                    }
                }
            }
        }
        if let Ok(Some((val, _))) = state.get(root, MUTED_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    if *b != self.last_muted {
                        self.last_muted = *b;
                        if let Err(e) = self.dispatcher.toggle_mute() {
                            warn!(target: "qos::settings", op = "toggle_mute", error = %e, "audio pane dispatch failed");
                        }
                    }
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
        "AudioPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    #[test]
    fn volume_slider_invokes_set_volume_on_drag() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, VOLUME_KEY, automerge::ScalarValue::F64(0.30));
        pane.sync(&mut doc, &root);
        let calls = rec.calls();
        assert_eq!(calls.len(), 1, "expected one dispatch, got {:?}", calls);
        assert!(calls[0].starts_with("set_volume("));
        assert!(calls[0].contains("30."), "expected 30 in {:?}", calls[0]);
    }

    #[test]
    fn mute_toggle_invokes_toggle_mute() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane =
            AudioPane::with_dispatcher(rec.clone() as Arc<dyn Dispatcher + Send + Sync>, 0.5);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Seed wrote `false`; simulating user flipping toggle to `true`.
        let _ = doc.put(&root, MUTED_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        let calls = rec.calls();
        assert_eq!(calls, vec!["toggle_mute()"]);
    }
}
