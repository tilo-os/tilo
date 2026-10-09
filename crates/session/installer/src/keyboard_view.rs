//! [`KeyboardView`] — a live visual preview of the selected keyboard
//! layout.
//!
//! It reads the chosen layout's *display name* from the wizard's view state
//! (the same [`ui::LocalState`] the layout [`ui::widget::Dropdown`] edits),
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
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

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
    /// The selected layout's display name.
    selection: ui::LocalState<String>,
    /// Resolved layout to render (defaults to English / US).
    layout: &'static Layout,
    rect: Rect,
}

impl KeyboardView {
    /// Build a preview of the layout named by `selection`.
    pub fn new(selection: &ui::LocalState<String>) -> Self {
        let mut view = Self {
            selection: selection.clone(),
            layout: layouts::default_layout(),
            rect: Rect::ZERO,
        };
        view.resolve();
        view
    }

    /// Follow the selection; an unknown name keeps the current layout.
    fn resolve(&mut self) {
        if let Some(layout) = self.selection.with(|name| layouts::by_display_name(name)) {
            self.layout = layout;
        }
    }

    /// The layout currently being rendered (test/inspection helper).
    pub fn layout_id(&self) -> &str {
        self.layout.id
    }

    /// Draw one rounded key cap with `glyph` centred; if `shift` is set it
    /// is drawn small in the top-left corner (used for the number row).
    fn draw_key(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        key: Rect,
        glyph: &str,
        shift: Option<&str>,
    ) {
        let cap = RoundedRect::from_rect(key, (theme.radius_standard * 0.6).max(2.0));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &cap,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &cap,
        );

        let font = ui::num::f32_of_f64(key.height() * 0.42).clamp(8.0, 16.0);

        // Shifted glyph (corner), drawn first / dim so the base reads clear.
        if let Some(s) = shift
            && !s.is_empty()
            && s != glyph
        {
            let sf = (font * 0.72).max(7.0);
            text_renderer.draw_text(
                scene,
                s,
                sf,
                theme.text_dim,
                transform.pre_translate(Vec2::new(key.x0 + 3.0, key.y0 + 1.0)),
                None,
            );
        }

        if !glyph.is_empty() {
            let (gw, gh) = text_renderer.measure(glyph, font, None);
            let tx = key.center().x - f64::from(gw) / 2.0;
            let ty = key.center().y - f64::from(gh) / 2.0;
            text_renderer.draw_text(
                scene,
                glyph,
                font,
                theme.text_primary,
                transform.pre_translate(Vec2::new(tx, ty)),
                None,
            );
        }
    }
}

impl Widget for KeyboardView {
    fn sync(&mut self, _state: &AppModel, _root: &ModelPath) {
        // Re-resolve every sync so the preview tracks the dropdown live.
        self.resolve();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only errors for a corrupted tree; `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so surfacing a
        // theoretical error here would mean threading a `Result` through the
        // whole toolkit for a branch that can't be taken.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh style cannot fail")]
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `layout` only errors for a `NodeId` from a different tree, which
        // never happens here since `node` is always the id this same widget
        // received from its own `build_layout` call.
        #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
        let l = taffy.layout(node).unwrap();
        self.rect = ui::widget::placed(l, origin);
    }

    #[allow(
        clippy::similar_names,
        reason = "row_x0/row_y0 and sb_x0/sb_y0 are the clearest short names for this row/spacebar-cap geometry"
    )]
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

        // Board background — the window ground, a shade below the raised
        // key caps so they pop.
        let board = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &board,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &board,
        );

        let inner_w = self.rect.width() - 2.0 * PAD;
        let inner_h = self.rect.height() - 2.0 * PAD;
        // 12 keys is the widest row; reserve up to ~1.25u of stagger room.
        let unit = inner_w / 13.5;
        let key_gap = unit * 0.12;
        let row_pitch = inner_h / ui::num::f64_of_usize(ROWS);
        let key_h = (row_pitch - key_gap).max(6.0);

        let x_base = self.rect.x0 + PAD;
        let y_base = self.rect.y0 + PAD;

        for (r, (glyphs, &stagger)) in self.layout.rows.iter().zip(STAGGER.iter()).enumerate() {
            let row_x0 = x_base + stagger * unit;
            let row_y0 = y_base + ui::num::f64_of_usize(r) * row_pitch;
            for (i, glyph) in glyphs.iter().enumerate() {
                let kx = row_x0 + ui::num::f64_of_usize(i) * unit;
                let key = Rect::new(kx, row_y0, kx + (unit - key_gap), row_y0 + key_h);
                let shift = if r == 0 {
                    self.layout.number_shift.get(i).copied()
                } else {
                    None
                };
                Self::draw_key(scene, transform, text_renderer, theme, key, glyph, shift);
            }
        }

        // Spacebar row — a single wide cap, centred.
        let sb_w = unit * 6.0;
        let sb_x0 = self.rect.center().x - sb_w / 2.0;
        let sb_y0 = y_base + 4.0 * row_pitch;
        let sb = Rect::new(sb_x0, sb_y0, sb_x0 + sb_w, sb_y0 + key_h);
        Self::draw_key(scene, transform, text_renderer, theme, sb, "", None);
    }

    fn name(&self) -> &'static str {
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

    fn sync(view: &mut KeyboardView) {
        let doc = automerge::AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        assert_eq!(automerge::ReadDoc::length(&doc, automerge::ROOT), 0);
    }

    #[test]
    fn keyboard_view_follows_the_selected_layout_as_a_non_focusable_leaf() {
        // Phase `defaults_to_us_without_a_known_selection`.
        {
            let kv = KeyboardView::new(&ui::LocalState::new(String::new()));
            assert_eq!(kv.layout_id(), "us");
        }
        // Phase `resolves_the_selected_layout`.
        {
            let kv = KeyboardView::new(&ui::LocalState::new("German (QWERTZ)".into()));
            assert_eq!(kv.layout_id(), "de");
        }
        // Phase `sync_tracks_changes_live`.
        {
            let selection = ui::LocalState::new("French (AZERTY)".to_string());
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "fr");

            // Change the selection, re-sync → preview follows.
            selection.set("Dvorak".to_string());
            sync(&mut kv);
            assert_eq!(kv.layout_id(), "dvorak");
        }
        // Phase `unknown_value_keeps_current_layout`.
        {
            let selection = ui::LocalState::new("Nonsense".to_string());
            let mut kv = KeyboardView::new(&selection);
            sync(&mut kv);
            // Falls back to the default (no panic, no change to a bogus layout).
            assert_eq!(kv.layout_id(), "us");
        }
        // Phase `is_a_non_focusable_leaf`.
        {
            let kv = KeyboardView::new(&ui::LocalState::new(String::new()));
            assert!(!kv.is_focusable());
            assert!(kv.children().is_empty());
        }
    }
}
