//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! Every control drives real, durable state:
//! - **Pointer speed / Natural scrolling / Tap to click / Key repeat** are
//!   persisted to the shared preferences store
//!   ([`crate::prefs::SystemPrefs`]); the compositor reads them when it
//!   configures libinput for the session.
//! - **Keyboard layout** additionally applies immediately via
//!   `localectl set-x11-keymap <code>` through [`LayoutSetter`] (privileged,
//!   so a denial is logged rather than surfaced as a crash), and the choice
//!   is persisted so it survives the session.
//
// CRDT keys this pane reads / writes:
//   `input.pointer_speed`    — f64 in `[0.0, 1.0]`
//   `input.natural_scroll`   — bool
//   `input.tap_to_click`     — bool
//   `input.key_repeat`       — f64 in `[0.0, 1.0]`
//   `input.keyboard_layout`  — string, a display name from [`LAYOUTS`]

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Slider, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::prefs::{InputPrefs, SystemPrefs};
use crate::widgets::{caption, card, choice_row, rows, setting_row, setting_row_described};

pub const POINTER_SPEED_KEY: &str = "input.pointer_speed";
pub const NATURAL_SCROLL_KEY: &str = "input.natural_scroll";
pub const TAP_CLICK_KEY: &str = "input.tap_to_click";
pub const KEY_REPEAT_KEY: &str = "input.key_repeat";
pub const LAYOUT_KEY: &str = "input.keyboard_layout";

/// The offered layouts as `(display name, xkb code)`. The display name is
/// what the dropdown shows and stores in the CRDT; the code is what
/// `localectl` and the prefs store use.
pub const LAYOUTS: &[(&str, &str)] = &[
    ("English (US)", "us"),
    ("English (UK)", "gb"),
    ("German", "de"),
    ("French", "fr"),
    ("Dutch", "nl"),
    ("Spanish", "es"),
];

/// Map an xkb code back to its display name (falls back to English (US)).
fn layout_label(code: &str) -> &'static str {
    LAYOUTS
        .iter()
        .find(|(_, c)| *c == code)
        .map_or("English (US)", |(label, _)| *label)
}

/// Map a display name to its xkb code, if recognised.
fn layout_code(label: &str) -> Option<&'static str> {
    LAYOUTS
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, code)| *code)
}

/// Side-effect interface for applying the keyboard layout. Production shells
/// out to `localectl`; tests inject a recorder.
pub trait LayoutSetter: Send + Sync {
    fn set_layout(&self, xkb_code: &str) -> Result<(), String>;
}

/// Production impl: `localectl set-x11-keymap <code>`.
pub struct RealLayout;

impl LayoutSetter for RealLayout {
    fn set_layout(&self, xkb_code: &str) -> Result<(), String> {
        let out = Command::new("localectl")
            .args(["set-x11-keymap", xkb_code])
            .output()
            .map_err(|e| format!("localectl not available: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

pub struct MouseKeyboardPane {
    layout_setter: Arc<dyn LayoutSetter>,
    initial: InputPrefs,
    last: InputPrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for MouseKeyboardPane {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseKeyboardPane {
    pub fn new() -> Self {
        let prefs = SystemPrefs::load().input;
        Self {
            layout_setter: Arc::new(RealLayout),
            initial: prefs.clone(),
            last: prefs,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor: inject the layout setter and starting preferences.
    pub fn with_setter(layout_setter: Arc<dyn LayoutSetter>, initial: InputPrefs) -> Self {
        Self {
            layout_setter,
            initial: initial.clone(),
            last: initial,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mouse = card(
            "Pointer",
            rows(vec![
                setting_row(
                    "Pointer speed",
                    Box::new(
                        Slider::new("settings.input.pointer_speed", POINTER_SPEED_KEY)
                            .with_id("input.pointer_speed"),
                    ),
                ),
                setting_row_described(
                    "Natural scrolling",
                    "Content tracks finger movement",
                    Box::new(
                        Toggle::new("settings.input.natural_scroll", NATURAL_SCROLL_KEY)
                            .with_id("input.natural_scroll"),
                    ),
                ),
                setting_row_described(
                    "Tap to click",
                    "Tap the touchpad to register a click",
                    Box::new(
                        Toggle::new("settings.input.tap_click", TAP_CLICK_KEY)
                            .with_id("input.tap_click"),
                    ),
                ),
            ]),
        );

        let layouts: Vec<String> = LAYOUTS.iter().map(|(l, _)| l.to_string()).collect();
        let keyboard = card(
            "Keyboard",
            rows(vec![
                choice_row(
                    "Layout",
                    Box::new(
                        Dropdown::new(layouts, LAYOUT_KEY)
                            .with_id("input.layout")
                            .with_placeholder(layout_label(&self.initial.keyboard_layout)),
                    ),
                ),
                setting_row(
                    "Key repeat speed",
                    Box::new(
                        Slider::new("settings.input.key_repeat", KEY_REPEAT_KEY)
                            .with_id("input.key_repeat"),
                    ),
                ),
                caption(
                    "The layout applies to the current session; pointer and \
                     repeat preferences are applied by the compositor.",
                ),
            ]),
        );

        crate::widgets::cards(vec![mouse, keyboard])
    }

    /// Load prefs fresh, apply `f` to the input section, persist.
    fn persist_input(f: impl FnOnce(&mut InputPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.input);
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "input_prefs_save", error = %e, "failed to persist input prefs");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(MouseKeyboardPane::new())
}

impl Widget for MouseKeyboardPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let _ = state.put(
                root,
                POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(self.initial.pointer_speed),
            );
            let _ = state.put(
                root,
                NATURAL_SCROLL_KEY,
                automerge::ScalarValue::Boolean(self.initial.natural_scroll),
            );
            let _ = state.put(
                root,
                TAP_CLICK_KEY,
                automerge::ScalarValue::Boolean(self.initial.tap_to_click),
            );
            let _ = state.put(
                root,
                KEY_REPEAT_KEY,
                automerge::ScalarValue::F64(self.initial.key_repeat),
            );
            let _ = state.put(
                root,
                LAYOUT_KEY,
                layout_label(&self.initial.keyboard_layout),
            );
            self.last = self.initial.clone();
            self.needs_seed = false;
        }

        if let Some(v) = read_f64(state, root, POINTER_SPEED_KEY)
            && (v - self.last.pointer_speed).abs() > 0.001
        {
            self.last.pointer_speed = v;
            Self::persist_input(|i| i.pointer_speed = v);
        }
        if let Some(v) = read_f64(state, root, KEY_REPEAT_KEY)
            && (v - self.last.key_repeat).abs() > 0.001
        {
            self.last.key_repeat = v;
            Self::persist_input(|i| i.key_repeat = v);
        }
        if let Some(b) = read_bool(state, root, NATURAL_SCROLL_KEY)
            && b != self.last.natural_scroll
        {
            self.last.natural_scroll = b;
            Self::persist_input(|i| i.natural_scroll = b);
        }
        if let Some(b) = read_bool(state, root, TAP_CLICK_KEY)
            && b != self.last.tap_to_click
        {
            self.last.tap_to_click = b;
            Self::persist_input(|i| i.tap_to_click = b);
        }
        // Keyboard layout → apply via localectl AND persist the choice.
        if let Some(label) = read_str(state, root, LAYOUT_KEY)
            && let Some(code) = layout_code(&label)
            && code != self.last.keyboard_layout.as_str()
        {
            self.last.keyboard_layout = code.to_string();
            if let Err(e) = self.layout_setter.set_layout(code) {
                warn!(target: "qos::settings", op = "set_layout", error = %e, "keyboard layout apply failed");
            }
            let owned = code.to_string();
            Self::persist_input(|i| i.keyboard_layout = owned);
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
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
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
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
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

    fn name(&self) -> &'static str {
        "MouseKeyboardPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn read_f64(state: &AutoCommit, root: &ObjId, key: &str) -> Option<f64> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::F64(f) => Some(*f),
            _ => None,
        },
        _ => None,
    }
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

/// Read a string — dropdowns write into an Automerge `Text` object, so
/// handle both a plain scalar and a `Text`.
fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingLayout {
        calls: Mutex<Vec<String>>,
    }
    impl LayoutSetter for RecordingLayout {
        fn set_layout(&self, code: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(code.to_string());
            Ok(())
        }
    }

    #[test]
    fn layout_label_and_code_round_trip() {
        assert_eq!(layout_label("de"), "German");
        assert_eq!(layout_code("German"), Some("de"));
        assert_eq!(layout_label("zz"), "English (US)");
        assert_eq!(layout_code("Klingon"), None);
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    /// Selecting a layout applies it via the setter exactly once, with the
    /// right xkb code, and not again while it rests.
    #[test]
    fn selecting_layout_invokes_setter_with_xkb_code() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("mk_layout");
        let setter = Arc::new(RecordingLayout::default());
        let mut pane = MouseKeyboardPane::with_setter(setter.clone(), InputPrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert!(
            setter.calls.lock().unwrap().is_empty(),
            "seed must not apply"
        );
        // Dropdown writes the display name into a Text object; emulate that.
        let _ = doc.put(&root, LAYOUT_KEY, "German");
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        assert_eq!(setter.calls.lock().unwrap().as_slice(), ["de"]);
    }

    /// The pointer-speed slider is consumed by `sync` (a live control, not a
    /// static mock): a changed value is adopted into the pane's tracked
    /// state so it isn't re-processed.
    #[test]
    fn pointer_speed_change_is_consumed() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("mk_pointer");
        let setter = Arc::new(RecordingLayout::default());
        let mut pane = MouseKeyboardPane::with_setter(setter, InputPrefs::default());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, POINTER_SPEED_KEY, automerge::ScalarValue::F64(0.9));
        pane.sync(&mut doc, &root);
        assert!((pane.last.pointer_speed - 0.9).abs() < 1e-6);
    }
}
