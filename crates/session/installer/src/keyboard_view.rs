//! [`KeyboardView`] — a live visual preview of the selected keyboard
//! layout.
//!
//! It reads the chosen layout's *display name* from the CRDT (the same
//! `installer.keymap` path the layout [`ui::widget::Dropdown`] writes to),
//! resolves it against [`crate::layouts`], and draws a keyboard — the
//! number row, three letter rows and a spacebar row — as rounded key caps,
//! each showing the glyph that physical key produces under the layout. The
//! number keys also show their shifted glyph small in a corner.
//!
//! Because it pulls the selection on every [`Widget::sync`], it re-renders
//! to match the dropdown the moment the user changes it (the host App
//! re-syncs + redraws after each event).
//!
//! It is a *leaf* widget — no children, not focusable — so it trivially
//! mirrors the taffy tree for the App's focusable-order traversal and never
//! steals keyboard focus. All colours / sizing come from the [`Theme`];
//! nothing is hard-coded.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::layouts::{self, Layout};

/// Overall widget height (logical px): 5 rows + board padding.
const HEIGHT: f32 = 196.0;
/// Inner padding inside the board, all sides.
const PAD: f64 = 10.0;
/// Number of drawn rows (number + 3 letter rows + spacebar).
const ROWS: usize = 5;
/// Stagger of each letter row, in key-pitch units (mimics a real board).
const STAGGER: [f64; 4] = [0.0, 0.5, 0.75, 1.25];

pub struct KeyboardView {
    /// CRDT path holding the selected layout's display name.
    value_path: String,
    /// Resolved layout to render (defaults to English / US).
    layout: &'static Layout,
    rect: Rect,
}

impl KeyboardView {
    /// Build a preview reading the selected layout from `value_path`.
    pub fn new(value_path: &str) -> Self {
        Self {
            value_path: value_path.to_string(),
            layout: layouts::default_layout(),
            rect: Rect::ZERO,
        }
    }

    /// The layout currently being rendered (test/inspection helper).
    pub fn layout_id(&self) -> &str {
        self.layout.id
    }

    /// Read the display name stored at `value_path` (Text object or scalar
    /// string), mirroring the [`ui::widget::Dropdown`] storage convention.
    fn read_value(state: &AutoCommit, root: &ObjId, path: &str) -> Option<String> {
        match state.get(root, path) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => state.text(&obj).ok(),
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(t) => Some(t.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Draw one rounded key cap with `glyph` centred; if `shift` is set it
    /// is drawn small in the top-left corner (used for the number row).
    fn draw_key(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        key: Rect,
        glyph: &str,
        shift: Option<&str>,
    ) {
        let cap = RoundedRect::from_rect(key, (theme.radius_standard * 0.6).max(2.0));
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &cap);
        scene.stroke(&Stroke::new(1.0), transform, theme.border_standard, None, &cap);

        let font = ((key.height() * 0.42) as f32).clamp(8.0, 16.0);

        // Shifted glyph (corner), drawn first / dim so the base reads clear.
        if let Some(s) = shift {
            if !s.is_empty() && s != glyph {
                let sf = (font * 0.72).max(7.0);
                text_renderer.draw_text(
                    scene,
                    s,
                    sf,
                    theme.text_dim,
                    transform * Affine::translate((key.x0 + 3.0, key.y0 + 1.0)),
                    None,
                );
            }
        }

        if !glyph.is_empty() {
            let (gw, gh) = text_renderer.measure(glyph, font, None);
            let tx = key.center().x - gw as f64 / 2.0;
            let ty = key.center().y - gh as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                glyph,
                font,
                theme.text_primary,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }
}

impl Widget for KeyboardView {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Re-resolve every sync so the preview tracks the dropdown live.
        if let Some(name) = Self::read_value(state, root, self.value_path.as_str()) {
            if let Some(l) = layouts::by_display_name(&name) {
                self.layout = l;
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() < 8.0 || self.rect.height() < 8.0 {
            return;
        }

        // Board background — surfaces.base, a shade below the card so the
        // raised key caps pop.
        let board = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &board);
        scene.stroke(&Stroke::new(1.0), transform, theme.border_standard, None, &board);

        let inner_w = self.rect.width() - 2.0 * PAD;
        let inner_h = self.rect.height() - 2.0 * PAD;
        // 12 keys is the widest row; reserve up to ~1.25u of stagger room.
        let unit = inner_w / 13.5;
        let key_gap = unit * 0.12;
        let row_pitch = inner_h / ROWS as f64;
        let key_h = (row_pitch - key_gap).max(6.0);

        let x_base = self.rect.x0 + PAD;
        let y_base = self.rect.y0 + PAD;

        for (r, glyphs) in self.layout.rows.iter().enumerate() {
            let row_x0 = x_base + STAGGER[r] * unit;
            let row_y0 = y_base + r as f64 * row_pitch;
            for (i, glyph) in glyphs.iter().enumerate() {
                let kx = row_x0 + i as f64 * unit;
                let key = Rect::new(kx, row_y0, kx + (unit - key_gap), row_y0 + key_h);
                let shift = if r == 0 {
                    self.layout.number_shift.get(i).copied()
                } else {
                    None
                };
                self.draw_key(scene, transform, text_renderer, theme, key, glyph, shift);
            }
        }

        // Spacebar row — a single wide cap, centred.
        let sb_w = unit * 6.0;
        let sb_x0 = self.rect.center().x - sb_w / 2.0;
        let sb_y0 = y_base + 4.0 * row_pitch;
        let sb = Rect::new(sb_x0, sb_y0, sb_x0 + sb_w, sb_y0 + key_h);
        self.draw_key(scene, transform, text_renderer, theme, sb, "", None);
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "KeyboardView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("keyboard_view")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    fn doc_with_keymap(value: &str) -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let obj = doc.put_object(&root, "installer.keymap", ObjType::Text).unwrap();
        let _ = doc.splice_text(&obj, 0, 0, value);
        (doc, root)
    }

    #[test]
    fn defaults_to_us_before_any_sync() {
        let kv = KeyboardView::new("installer.keymap");
        assert_eq!(kv.layout_id(), "us");
    }

    #[test]
    fn sync_resolves_selected_layout() {
        let mut kv = KeyboardView::new("installer.keymap");
        let (mut doc, root) = doc_with_keymap("German (QWERTZ)");
        kv.sync(&mut doc, &root);
        assert_eq!(kv.layout_id(), "de");
    }

    #[test]
    fn sync_tracks_changes_live() {
        let mut kv = KeyboardView::new("installer.keymap");
        let (mut doc, root) = doc_with_keymap("French (AZERTY)");
        kv.sync(&mut doc, &root);
        assert_eq!(kv.layout_id(), "fr");

        // Change the stored selection, re-sync → preview follows.
        let (_, obj) = doc.get(&root, "installer.keymap").unwrap().unwrap();
        let cur = doc.text(&obj).unwrap();
        let _ = doc.splice_text(&obj, 0, cur.chars().count() as isize, "Dvorak");
        kv.sync(&mut doc, &root);
        assert_eq!(kv.layout_id(), "dvorak");
    }

    #[test]
    fn unknown_value_keeps_current_layout() {
        let mut kv = KeyboardView::new("installer.keymap");
        let (mut doc, root) = doc_with_keymap("Nonsense");
        kv.sync(&mut doc, &root);
        // Falls back to the default (no panic, no change to a bogus layout).
        assert_eq!(kv.layout_id(), "us");
    }

    #[test]
    fn is_a_non_focusable_leaf() {
        let kv = KeyboardView::new("installer.keymap");
        assert!(!kv.is_focusable());
        assert!(kv.children().is_empty());
    }
}
