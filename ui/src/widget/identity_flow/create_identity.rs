//! Screen 1 — first-run identity creation.
//!
//! Layout (top-to-bottom inside an 800×600 centered card):
//!
//! ```text
//! +----------------------------------------+
//! | qOS — create your identity             |
//! |                                        |
//! | [explanation paragraph]                |
//! |                                        |
//! | Display name                           |
//! | [text input          ]                 |
//! |                                        |
//! | Accent colour                          |
//! | [ # # # # # # # # # # ]                |
//! |                                        |
//! |                       [Create →]       |
//! +----------------------------------------+
//! ```
//!
//! Submit is disabled while the display name is empty (whitespace only
//! counts as empty). On click: mints a fresh identity, wraps it in an
//! `IdentityRecord`, saves it via the `IdentityStore`, then advances to
//! screen 2 with the new record cached for the recovery-share screen.
//!
//! The form validation + submit pipeline is exposed as plain methods so
//! tests can drive the screen without rendering; `submit_to_store` is the
//! load-bearing path, also used by `handle_event_screen` on a real click.

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use rand_core::OsRng;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use threshold_identity::ThresholdIdentity;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{
    IdentityFlowError, IdentityRecord, IdentityStore, ScreenOutcome,
};

/// Default accent-tile palette. Picked so each tile reads as a
/// distinct hue at a small swatch size. Stored as `[r, g, b]`.
pub const ACCENT_PALETTE: [[u8; 3]; 8] = [
    [248, 113, 113], // rose
    [251, 146, 60],  // amber
    [250, 204, 21],  // gold
    [134, 239, 172], // mint
    [56, 189, 248],  // sky
    [129, 140, 248], // indigo
    [192, 132, 252], // violet
    [244, 114, 182], // pink
];

pub struct CreateIdentityScreen {
    rect: Rect,
    /// Single-line editor backing the display-name field. Use
    /// `display_name()` / `set_display_name()` to read/write.
    editor: crate::widget::identity_flow::LineEditState,
    selected_accent: usize,
    submit_hovered: bool,
    submit_rect: Rect,
    /// Accent-tile rects, populated by `apply_layout`.
    tile_rects: Vec<Rect>,
    /// Text-input field rect — populated by `apply_layout`.
    input_rect: Rect,
    /// Error from the last submit attempt, if any.
    submit_error: Option<String>,
    /// The most recent record produced by a successful submit.
    last_record: Option<IdentityRecord>,
    /// Last cursor position (set by `CursorMoved`) so the next
    /// `MouseInput` can hit-test against rects.
    last_cursor: Point,
}

impl Default for CreateIdentityScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateIdentityScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            editor: crate::widget::identity_flow::LineEditState::new(),
            selected_accent: 0,
            submit_hovered: false,
            submit_rect: Rect::ZERO,
            tile_rects: Vec::new(),
            input_rect: Rect::ZERO,
            submit_error: None,
            last_record: None,
            last_cursor: Point::ZERO,
        }
    }

    /// Live display-name input — useful for tests.
    pub fn display_name(&self) -> &str {
        &self.editor.text
    }

    pub fn set_display_name(&mut self, name: impl Into<String>) {
        self.editor = crate::widget::identity_flow::LineEditState::with_text(name);
        self.submit_error = None;
    }

    pub fn selected_accent_color(&self) -> [u8; 3] {
        ACCENT_PALETTE[self.selected_accent]
    }

    pub fn set_accent_index(&mut self, idx: usize) {
        if idx < ACCENT_PALETTE.len() {
            self.selected_accent = idx;
        }
    }

    pub fn is_submit_enabled(&self) -> bool {
        !self.editor.text.trim().is_empty()
    }

    pub fn last_record(&self) -> Option<&IdentityRecord> {
        self.last_record.as_ref()
    }

    pub fn submit_error(&self) -> Option<&str> {
        self.submit_error.as_deref()
    }

    /// On success the new `IdentityRecord` is cached and returned. On
    /// error `submit_error` is set so the UI can surface it inline.
    pub fn submit_to_store(
        &mut self,
        store: &dyn IdentityStore,
    ) -> Result<IdentityRecord, IdentityFlowError> {
        if !self.is_submit_enabled() {
            return Err(IdentityFlowError::DecodeFailed(
                "display name must not be empty".to_string(),
            ));
        }
        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        let record = IdentityRecord::from_threshold_identity(
            &id,
            self.editor.text.trim(),
            ACCENT_PALETTE[self.selected_accent],
            SystemTime::now(),
        )?;
        store.save(&record)?;
        self.last_record = Some(record.clone());
        self.submit_error = None;
        Ok(record)
    }

    /// Returns `ScreenOutcome::Advance` only on a successful submit.
    // Mirrors `Widget::handle_event`'s parameter list on purpose --
    // this is invoked from the trait impl, forwarding the same event,
    // scale, modifiers, CRDT handle, and out-params the screen host
    // received; bundling it would mean bundling the trait method too.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
        store: &dyn IdentityStore,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pos;
                let hovering = self.submit_rect.contains(pos) && self.is_submit_enabled();
                if hovering != self.submit_hovered {
                    self.submit_hovered = hovering;
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Accent tile?
                for (i, r) in self.tile_rects.iter().enumerate() {
                    if r.contains(self.last_cursor) {
                        self.selected_accent = i;
                        *handled = true;
                        return ScreenOutcome::Stay;
                    }
                }
                // Submit?
                if self.submit_rect.contains(self.last_cursor) && self.is_submit_enabled() {
                    *handled = true;
                    match self.submit_to_store(store) {
                        Ok(_rec) => return ScreenOutcome::Advance,
                        Err(e) => {
                            self.submit_error = Some(format!("{e}"));
                            return ScreenOutcome::Stay;
                        }
                    }
                }
                ScreenOutcome::Stay
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                // Enter submits; everything else delegates to the shared editor.
                if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                    if self.is_submit_enabled() {
                        *handled = true;
                        match self.submit_to_store(store) {
                            Ok(_) => return ScreenOutcome::Advance,
                            Err(e) => self.submit_error = Some(format!("{e}")),
                        }
                    }
                    return ScreenOutcome::Stay;
                }
                if self
                    .editor
                    .apply_key(logical_key, _modifiers, text.as_deref())
                {
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }
}

impl Widget for CreateIdentityScreen {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(800.0),
                height: Dimension::length(600.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let card_w = 520.0;
        let card_h = 440.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;
        self.input_rect = Rect::new(
            card.x0 + pad,
            card.y0 + 140.0,
            card.x1 - pad,
            card.y0 + 140.0 + 40.0,
        );
        self.tile_rects.clear();
        let tile_size = 32.0;
        let gap = 8.0;
        let tiles_total_width =
            ACCENT_PALETTE.len() as f64 * tile_size + (ACCENT_PALETTE.len() - 1) as f64 * gap;
        let tile_y = card.y0 + 232.0;
        let tile_x0 = card.x0 + (card_w - tiles_total_width) / 2.0;
        for i in 0..ACCENT_PALETTE.len() {
            let x = tile_x0 + i as f64 * (tile_size + gap);
            self.tile_rects
                .push(Rect::new(x, tile_y, x + tile_size, tile_y + tile_size));
        }
        let btn_w = 160.0;
        let btn_h = 44.0;
        self.submit_rect = Rect::new(
            card.x1 - pad - btn_w,
            card.y1 - pad - btn_h,
            card.x1 - pad,
            card.y1 - pad,
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
        let backdrop = Color::from_rgba8(0, 0, 0, 180);
        scene.fill(Fill::NonZero, transform, backdrop, None, &self.rect);
        let card_pad = 32.0;
        let card_w = 520.0;
        let card_h = 440.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let radius = 12.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(card, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(card, radius),
        );
        text_renderer.draw_text(
            scene,
            "Create your qOS identity",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + card_pad, card.y0 + card_pad)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Your identity signs every action you take and lets you join shared channels.",
            14.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + card_pad, card.y0 + card_pad + 36.0)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Display name",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + card_pad, card.y0 + 116.0)),
            None,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.input_rect, 6.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.input_rect, 6.0),
        );
        let name = &self.editor.text;
        let text_color = if name.is_empty() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        let displayed: String = if name.is_empty() {
            "Your name…".to_string()
        } else {
            name.clone()
        };
        let text_origin_x = self.input_rect.x0 + 12.0;
        let text_origin_y = self.input_rect.y0 + 12.0;
        if let Some((lo, hi)) = self.editor.selection_range() {
            let (pre_w, _) = text_renderer.measure(&name[..lo], 15.0, None);
            let (mid_w, _) = text_renderer.measure(&name[lo..hi], 15.0, None);
            let sel_color = theme.accent_primary.with_alpha_factor(0.30);
            scene.fill(
                Fill::NonZero,
                transform,
                sel_color,
                None,
                &Rect::new(
                    text_origin_x + f64::from(pre_w),
                    text_origin_y - 2.0,
                    text_origin_x + f64::from(pre_w) + f64::from(mid_w),
                    text_origin_y + 18.0,
                ),
            );
        }
        text_renderer.draw_text(
            scene,
            &displayed,
            15.0,
            text_color,
            transform * Affine::translate((text_origin_x, text_origin_y)),
            None,
        );
        // Drawn over the selection so the user always sees where they'd type.
        let cursor_byte = self.editor.cursor.min(name.len());
        let prefix = &name[..cursor_byte];
        let (prefix_w, _) = text_renderer.measure(prefix, 15.0, None);
        let caret_x = text_origin_x + f64::from(prefix_w);
        let caret_top = text_origin_y - 2.0;
        let caret_bottom = text_origin_y + 18.0;
        let caret_rect = Rect::new(caret_x, caret_top, caret_x + 1.5, caret_bottom);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
            None,
            &caret_rect,
        );
        text_renderer.draw_text(
            scene,
            "Accent colour",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + card_pad, card.y0 + 208.0)),
            None,
        );
        for (i, r) in self.tile_rects.iter().enumerate() {
            let [r_, g_, b_] = ACCENT_PALETTE[i];
            let fill = Color::from_rgb8(r_, g_, b_);
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(*r, 6.0),
            );
            if i == self.selected_accent {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.text_primary,
                    None,
                    &RoundedRect::from_rect(r.inflate(2.0, 2.0), 8.0),
                );
            }
        }
        let btn_bg = if self.is_submit_enabled() {
            if self.submit_hovered {
                theme.bg_active
            } else {
                theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
            }
        } else {
            theme.tokens.colors.surfaces.base
        };
        let btn_text_color = if self.is_submit_enabled() {
            Color::from_rgb8(255, 255, 255)
        } else {
            theme.tokens.colors.text_dim
        };
        scene.fill(
            Fill::NonZero,
            transform,
            btn_bg,
            None,
            &RoundedRect::from_rect(self.submit_rect, 8.0),
        );
        let label = "Create my identity";
        let (label_w, _) = text_renderer.measure(label, 14.0, None);
        let label_x = self.submit_rect.x0 + (self.submit_rect.width() - f64::from(label_w)) / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            14.0,
            btn_text_color,
            transform
                * Affine::translate((
                    label_x,
                    self.submit_rect.y0 + (self.submit_rect.height() - 14.0) / 2.0,
                )),
            None,
        );
        if let Some(err) = &self.submit_error {
            text_renderer.draw_text(
                scene,
                err,
                12.0,
                Color::from_rgb8(220, 80, 80),
                transform * Affine::translate((card.x0 + card_pad, card.y1 - card_pad - 28.0)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Real input goes through `handle_event_screen`, which the
        // container calls with the IdentityStore; this path is unused.
    }

    fn name(&self) -> &'static str {
        "CreateIdentityScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_create")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::identity_flow::MemoryIdentityStore;

    #[test]
    fn submit_disabled_until_name_provided() {
        let s = CreateIdentityScreen::new();
        assert!(!s.is_submit_enabled(), "empty name must disable submit");
    }

    #[test]
    fn submit_disabled_for_whitespace_only_name() {
        let mut s = CreateIdentityScreen::new();
        s.set_display_name("   \t  ");
        assert!(
            !s.is_submit_enabled(),
            "whitespace must be treated as empty"
        );
    }

    #[test]
    fn submit_to_store_creates_record() {
        let mut s = CreateIdentityScreen::new();
        s.set_display_name("alice");
        s.set_accent_index(2);
        let store = MemoryIdentityStore::new();
        let rec = s.submit_to_store(&store).expect("submit must succeed");
        assert_eq!(rec.display_name, "alice");
        assert_eq!(rec.avatar_color, ACCENT_PALETTE[2]);
        // Persisted into the store.
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.display_name, "alice");
        // Cached on the screen for the container to pick up.
        assert!(s.last_record().is_some());
    }

    #[test]
    fn submit_with_empty_name_errors() {
        let mut s = CreateIdentityScreen::new();
        let store = MemoryIdentityStore::new();
        let err = s.submit_to_store(&store).unwrap_err();
        assert!(matches!(err, IdentityFlowError::DecodeFailed(_)));
        assert!(s.last_record().is_none());
    }

    #[test]
    fn accent_index_clamps_to_palette_bound() {
        let mut s = CreateIdentityScreen::new();
        s.set_accent_index(99);
        // Out-of-range index leaves the previous selection in place.
        assert_eq!(s.selected_accent, 0);
        s.set_accent_index(3);
        assert_eq!(s.selected_accent, 3);
    }
}
