//! Accessibility pane — the dedicated assistive-technology surface.
//!
//! This panel aggregates the OS accessibility controls the way a
//! control-center's "Universal Access" panel does. It edits two shared
//! sections of `settings.json`:
//!
//! - the **vision comforts** a low-vision user reaches for first — high
//!   contrast, reduced motion, and text size — which live in
//!   [`crate::prefs::AppearancePrefs`] (the single source of truth the whole
//!   system themes against, also editable from the Appearance pane); and
//! - the **pointer, typing, hearing, and screen-reader** assists in
//!   [`crate::prefs::AccessibilityPrefs`], read by the compositor's input +
//!   accessibility layer.
//!
//! Editing a vision comfort here writes the *same* appearance field the
//! Appearance pane writes, so the two panes never disagree — there is one
//! stored value, surfaced in both places (as macOS/GNOME do with text size).
//!
//! Each control is bound to a CRDT key; `sync` folds the change into the
//! loaded prefs and persists the shared store, preserving every other section.

use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::panes::appearance::{text_size_label, text_size_scale, TEXT_SIZES};
use crate::prefs::{AccessibilityPrefs, AppearancePrefs, SystemPrefs};
use crate::widgets::{card, caption, choice_row, rows, setting_row_described, El};

// Vision comforts (mirror the shared AppearancePrefs fields).
pub const HIGH_CONTRAST_KEY: &str = "a11y.high_contrast";
pub const REDUCE_MOTION_KEY: &str = "a11y.reduce_motion";
pub const TEXT_SIZE_KEY: &str = "a11y.text_size";
// Pointer.
pub const LARGE_CURSOR_KEY: &str = "a11y.large_cursor";
pub const CURSOR_SIZE_KEY: &str = "a11y.cursor_size";
// Typing.
pub const STICKY_KEYS_KEY: &str = "a11y.sticky_keys";
pub const SLOW_KEYS_KEY: &str = "a11y.slow_keys";
pub const OSK_KEY: &str = "a11y.on_screen_keyboard";
// Hearing + screen reader.
pub const VISUAL_ALERTS_KEY: &str = "a11y.visual_alerts";
pub const SCREEN_READER_KEY: &str = "a11y.screen_reader";

pub struct AccessibilityPane {
    appearance: AppearancePrefs,
    access: AccessibilityPrefs,
    needs_seed: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AccessibilityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessibilityPane {
    pub fn new() -> Self {
        let p = SystemPrefs::load();
        Self::with_prefs(p.appearance, p.accessibility)
    }

    /// Construct from explicit preferences — used by tests and the
    /// deterministic `--shots` render.
    pub fn with_prefs(appearance: AppearancePrefs, access: AccessibilityPrefs) -> Self {
        Self {
            appearance,
            access,
            needs_seed: true,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn access(&self) -> &AccessibilityPrefs {
        &self.access
    }

    pub fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }

    fn toggle(id: &str, key: &str) -> El {
        Box::new(Toggle::new(id, key).with_id(id))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // --- Vision: high contrast, reduce motion, text size.
        let size_opts: Vec<String> = TEXT_SIZES.iter().map(|(l, _)| l.to_string()).collect();
        let size = Dropdown::new(size_opts, TEXT_SIZE_KEY)
            .with_id("a11y.text_size")
            .with_placeholder(text_size_label(self.appearance.text_scale));
        let vision_card = card(
            "Vision",
            rows(vec![
                setting_row_described(
                    "High contrast",
                    "Maximise contrast for low-vision comfort",
                    Self::toggle("a11y.high_contrast", HIGH_CONTRAST_KEY),
                ),
                setting_row_described(
                    "Reduce motion",
                    "Skip animations and transitions",
                    Self::toggle("a11y.reduce_motion", REDUCE_MOTION_KEY),
                ),
                choice_row("Text size", Box::new(size)),
            ]),
        );

        // --- Pointer: large cursor + size.
        let cursor_opts: Vec<String> =
            AccessibilityPrefs::CURSOR_SIZES.iter().map(|(l, _)| l.to_string()).collect();
        let cursor = Dropdown::new(cursor_opts, CURSOR_SIZE_KEY)
            .with_id("a11y.cursor_size")
            .with_placeholder(AccessibilityPrefs::cursor_size_label(self.access.cursor_size));
        let pointer_card = card(
            "Pointer",
            rows(vec![
                setting_row_described(
                    "Large cursor",
                    "Enlarge the mouse pointer so it is easy to find",
                    Self::toggle("a11y.large_cursor", LARGE_CURSOR_KEY),
                ),
                choice_row("Cursor size", Box::new(cursor)),
            ]),
        );

        // --- Typing.
        let typing_card = card(
            "Typing",
            rows(vec![
                setting_row_described(
                    "Sticky keys",
                    "Type shortcuts one key at a time instead of holding a chord",
                    Self::toggle("a11y.sticky_keys", STICKY_KEYS_KEY),
                ),
                setting_row_described(
                    "Slow keys",
                    "Ignore brief, accidental key presses",
                    Self::toggle("a11y.slow_keys", SLOW_KEYS_KEY),
                ),
                setting_row_described(
                    "On-screen keyboard",
                    "Show a keyboard for typing without a physical one",
                    Self::toggle("a11y.osk", OSK_KEY),
                ),
            ]),
        );

        // --- Hearing + screen reader.
        let hearing_card = card(
            "Hearing & screen reader",
            rows(vec![
                setting_row_described(
                    "Visual alerts",
                    "Flash the screen instead of playing an alert sound",
                    Self::toggle("a11y.visual_alerts", VISUAL_ALERTS_KEY),
                ),
                setting_row_described(
                    "Screen reader",
                    "Read the interface aloud (AT-SPI)",
                    Self::toggle("a11y.screen_reader", SCREEN_READER_KEY),
                ),
            ]),
        );

        let explainer = card(
            "Shared with your apps",
            rows(vec![caption(
                "Vision comforts are the same settings the Appearance pane \
                 offers — changing them here or there updates one shared value. \
                 The pointer, typing, and hearing assists are read by the \
                 compositor's input and accessibility layer.",
            )]),
        );

        crate::widgets::cards(vec![
            vision_card,
            pointer_card,
            typing_card,
            hearing_card,
            explainer,
        ])
    }

    /// Persist both edited sections into the shared store, reading fresh so a
    /// change here never clobbers another pane's section.
    fn persist(&self) {
        let mut p = SystemPrefs::load();
        p.appearance = self.appearance.clone();
        p.accessibility = self.access;
        if let Err(e) = p.save() {
            warn!(target: "qos::settings", op = "accessibility_save", error = %e, "failed to persist accessibility prefs");
        }
    }

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

    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AccessibilityPane::new())
}

impl Widget for AccessibilityPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            let seed_bool = |state: &mut AutoCommit, key: &str, v: bool| {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(v));
            };
            seed_bool(state, HIGH_CONTRAST_KEY, self.appearance.high_contrast);
            seed_bool(state, REDUCE_MOTION_KEY, self.appearance.reduce_motion);
            let _ = state.put(root, TEXT_SIZE_KEY, text_size_label(self.appearance.text_scale));
            seed_bool(state, LARGE_CURSOR_KEY, self.access.large_cursor);
            let _ = state.put(
                root,
                CURSOR_SIZE_KEY,
                AccessibilityPrefs::cursor_size_label(self.access.cursor_size),
            );
            seed_bool(state, STICKY_KEYS_KEY, self.access.sticky_keys);
            seed_bool(state, SLOW_KEYS_KEY, self.access.slow_keys);
            seed_bool(state, OSK_KEY, self.access.on_screen_keyboard);
            seed_bool(state, VISUAL_ALERTS_KEY, self.access.visual_alerts);
            seed_bool(state, SCREEN_READER_KEY, self.access.screen_reader);
            self.needs_seed = false;
        }

        let mut changed = false;

        // Vision comforts fold into the shared appearance section.
        if let Some(b) = Self::read_bool(state, root, HIGH_CONTRAST_KEY) {
            if b != self.appearance.high_contrast {
                self.appearance.high_contrast = b;
                changed = true;
            }
        }
        if let Some(b) = Self::read_bool(state, root, REDUCE_MOTION_KEY) {
            if b != self.appearance.reduce_motion {
                self.appearance.reduce_motion = b;
                changed = true;
            }
        }
        if let Some(label) = Self::read_str(state, root, TEXT_SIZE_KEY) {
            if let Some(scale) = text_size_scale(&label) {
                if (scale - self.appearance.text_scale).abs() > f64::EPSILON {
                    self.appearance.text_scale = scale;
                    changed = true;
                }
            }
        }

        // Pointer / typing / hearing fold into the accessibility section.
        let mut adopt = |cur: &mut bool, v: Option<bool>| {
            if let Some(b) = v {
                if b != *cur {
                    *cur = b;
                    changed = true;
                }
            }
        };
        adopt(&mut self.access.large_cursor, Self::read_bool(state, root, LARGE_CURSOR_KEY));
        adopt(&mut self.access.sticky_keys, Self::read_bool(state, root, STICKY_KEYS_KEY));
        adopt(&mut self.access.slow_keys, Self::read_bool(state, root, SLOW_KEYS_KEY));
        adopt(
            &mut self.access.on_screen_keyboard,
            Self::read_bool(state, root, OSK_KEY),
        );
        adopt(&mut self.access.visual_alerts, Self::read_bool(state, root, VISUAL_ALERTS_KEY));
        adopt(&mut self.access.screen_reader, Self::read_bool(state, root, SCREEN_READER_KEY));

        if let Some(label) = Self::read_str(state, root, CURSOR_SIZE_KEY) {
            if let Some(px) = AccessibilityPrefs::cursor_size_px(&label) {
                if px != self.access.cursor_size {
                    self.access.cursor_size = px;
                    changed = true;
                }
            }
        }

        if changed {
            self.persist();
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
                        event, scale_factor, modifiers, state, path, handled, focus_target, cn,
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
        "AccessibilityPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_writes_current_prefs_into_crdt() {
        let mut pane = AccessibilityPane::with_prefs(
            AppearancePrefs {
                high_contrast: true,
                text_scale: 1.15,
                ..AppearancePrefs::default()
            },
            AccessibilityPrefs {
                large_cursor: true,
                cursor_size: 48,
                screen_reader: true,
                ..AccessibilityPrefs::default()
            },
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        assert_eq!(AccessibilityPane::read_bool(&doc, &root, HIGH_CONTRAST_KEY), Some(true));
        assert_eq!(
            AccessibilityPane::read_str(&doc, &root, TEXT_SIZE_KEY).as_deref(),
            Some("Large")
        );
        assert_eq!(AccessibilityPane::read_bool(&doc, &root, LARGE_CURSOR_KEY), Some(true));
        assert_eq!(
            AccessibilityPane::read_str(&doc, &root, CURSOR_SIZE_KEY).as_deref(),
            Some("Larger")
        );
        assert_eq!(AccessibilityPane::read_bool(&doc, &root, SCREEN_READER_KEY), Some(true));
    }

    #[test]
    fn vision_edit_folds_into_shared_appearance_and_persists() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("a11y_vision");
        let mut pane = AccessibilityPane::with_prefs(
            AppearancePrefs::default(),
            AccessibilityPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Turn on high contrast + bump the text size here in Accessibility.
        let _ = doc.put(&root, HIGH_CONTRAST_KEY, automerge::ScalarValue::Boolean(true));
        let _ = doc.put(&root, TEXT_SIZE_KEY, "Larger");
        pane.sync(&mut doc, &root);
        assert!(pane.appearance().high_contrast);
        assert_eq!(pane.appearance().text_scale, 1.30);
        // The shared appearance section is what got written — the Appearance
        // pane would load the same value.
        let loaded = SystemPrefs::load();
        assert!(loaded.appearance.high_contrast, "written to the shared section");
        assert_eq!(loaded.appearance.text_scale, 1.30);
    }

    #[test]
    fn pointer_and_typing_edits_persist_to_accessibility_section() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("a11y_pointer");
        let mut pane = AccessibilityPane::with_prefs(
            AppearancePrefs::default(),
            AccessibilityPrefs::default(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, LARGE_CURSOR_KEY, automerge::ScalarValue::Boolean(true));
        let _ = doc.put(&root, CURSOR_SIZE_KEY, "Largest");
        let _ = doc.put(&root, STICKY_KEYS_KEY, automerge::ScalarValue::Boolean(true));
        let _ = doc.put(&root, OSK_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert!(pane.access().large_cursor);
        assert_eq!(pane.access().cursor_size, 64);
        assert!(pane.access().sticky_keys);
        assert!(pane.access().on_screen_keyboard);
        let loaded = SystemPrefs::load().accessibility;
        assert_eq!(loaded.cursor_size, 64, "persisted to the accessibility section");
        assert!(loaded.on_screen_keyboard);
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
