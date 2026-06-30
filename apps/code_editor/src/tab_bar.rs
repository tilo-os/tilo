//! Editor tabs: one tab per open buffer, with a language-tinted dot, a dirty
//! indicator, and a close affordance. Drawn directly into the scene so the
//! tabs read like an IDE rather than generic chips. Emits [`TabEvent`]s.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::model::Language;

pub const TAB_BAR_HEIGHT: f64 = 36.0;
const TAB_PAD: f64 = 14.0;
const CLOSE_BOX: f64 = 16.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabEvent {
    Select(usize),
    Close(usize),
}

/// One tab's display data, pushed in by the app each frame.
#[derive(Clone, Debug)]
pub struct TabItem {
    pub name: String,
    pub language: Language,
    pub dirty: bool,
}

pub struct TabBar {
    rect: Rect,
    tabs: Vec<TabItem>,
    active: Option<usize>,
    /// Per-tab computed x-extent, filled in `draw`, read by hit-testing.
    tab_rects: Vec<(f64, f64)>,
    hovered: Option<usize>,
    pending: Vec<TabEvent>,
    last_cursor: Point,
    dirty: bool,
}

impl TabBar {
    pub fn new() -> Self {
        TabBar {
            rect: Rect::ZERO,
            tabs: Vec::new(),
            active: None,
            tab_rects: Vec::new(),
            hovered: None,
            pending: Vec::new(),
            last_cursor: Point::ZERO,
            dirty: true,
        }
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabItem>, active: Option<usize>) {
        self.tabs = tabs;
        self.active = active;
        self.dirty = true;
    }

    pub fn take_events(&mut self) -> Vec<TabEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Shrink the bar's drawable region from the left by `px`, reserving room
    /// for the overlay-mode hamburger toggle the app draws over that corner.
    /// The bar both draws and hit-tests from `self.rect`, so moving its left
    /// edge shifts the tabs without touching the per-tab layout. Applied each
    /// frame after the taffy-derived rect is set (see `CodeApp::apply_layout`).
    pub fn inset_left(&mut self, px: f64) {
        if px > 0.0 && self.rect.width() > px {
            self.rect = Rect::new(self.rect.x0 + px, self.rect.y0, self.rect.x1, self.rect.y1);
        }
    }

    /// A language-distinct dot colour, derived from theme tokens.
    fn lang_color(language: Language, theme: &Theme) -> vello::peniko::Color {
        let c = &theme.tokens.colors;
        match language {
            Language::Rust => c.semantic.warning,
            Language::Python => c.semantic.info,
            Language::JavaScript | Language::TypeScript => c.semantic.warning,
            Language::Toml | Language::Yaml => c.accent[5],
            Language::Json => c.semantic.success,
            Language::Markdown => c.text_dim,
            Language::Shell => c.semantic.success,
            Language::Html | Language::Css => c.semantic.danger,
            Language::C | Language::Cpp | Language::Go => c.semantic.info,
            Language::PlainText => c.text_dim,
        }
    }
}

impl Widget for TabBar {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
                    height: Dimension::length(TAB_BAR_HEIGHT as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &self.rect);

        self.tab_rects.clear();
        let mut x = self.rect.x0;
        for (i, tab) in self.tabs.iter().enumerate() {
            let (label_w, _) = text_renderer.measure(&tab.name, 13.0, None);
            let tab_w = TAB_PAD + 14.0 + label_w as f64 + 8.0 + CLOSE_BOX + TAB_PAD;
            let x1 = x + tab_w;
            self.tab_rects.push((x, x1));

            let is_active = self.active == Some(i);
            let tab_rect = Rect::new(x, self.rect.y0, x1, self.rect.y1);
            if is_active {
                scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &tab_rect);
                // Top accent line.
                let top = Rect::new(x, self.rect.y0, x1, self.rect.y0 + 2.0);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &top);
            } else if self.hovered == Some(i) {
                scene.fill(Fill::NonZero, transform, c.surface_white_5, None, &tab_rect);
            }

            // Language dot.
            let dot_c = Self::lang_color(tab.language, theme);
            let dot = vello::kurbo::Circle::new(
                Point::new(x + TAB_PAD + 4.0, self.rect.y0 + self.rect.height() / 2.0),
                4.0,
            );
            scene.fill(Fill::NonZero, transform, dot_c, None, &dot);

            // Label.
            let label_x = x + TAB_PAD + 14.0;
            let label_color = if is_active { c.text_primary } else { c.neutral[7] };
            text_renderer.draw_text_weighted(
                scene,
                &tab.name,
                13.0,
                label_color,
                transform * Affine::translate((label_x, self.rect.y0 + 9.0)),
                None,
                if is_active { TextWeight::Semibold } else { TextWeight::Regular },
            );

            // Dirty dot OR close glyph.
            let close_cx = x1 - TAB_PAD - CLOSE_BOX / 2.0;
            let close_cy = self.rect.y0 + self.rect.height() / 2.0;
            if tab.dirty && self.hovered != Some(i) {
                let d = vello::kurbo::Circle::new(Point::new(close_cx, close_cy), 4.0);
                scene.fill(Fill::NonZero, transform, c.text_dim, None, &d);
            } else {
                // Close X.
                let s = 4.0;
                let col = if is_active { c.text_primary } else { c.text_dim };
                let stroke = Stroke::new(1.5);
                scene.stroke(
                    &stroke,
                    transform,
                    col,
                    None,
                    &Line::new(
                        Point::new(close_cx - s, close_cy - s),
                        Point::new(close_cx + s, close_cy + s),
                    ),
                );
                scene.stroke(
                    &stroke,
                    transform,
                    col,
                    None,
                    &Line::new(
                        Point::new(close_cx + s, close_cy - s),
                        Point::new(close_cx - s, close_cy + s),
                    ),
                );
            }

            // Right divider.
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                c.border_standard,
                None,
                &Line::new(Point::new(x1, self.rect.y0 + 6.0), Point::new(x1, self.rect.y1 - 6.0)),
            );

            x = x1;
        }

        // Bottom border under the whole bar.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y1),
                Point::new(self.rect.x1, self.rect.y1),
            ),
        );
    }

    fn handle_event(
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
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                let mut h = None;
                if self.rect.contains(self.last_cursor) {
                    for (i, (x0, x1)) in self.tab_rects.iter().enumerate() {
                        if self.last_cursor.x >= *x0 && self.last_cursor.x < *x1 {
                            h = Some(i);
                            break;
                        }
                    }
                }
                if h != self.hovered {
                    self.hovered = h;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.rect.contains(self.last_cursor) {
                    return;
                }
                for (i, (x0, x1)) in self.tab_rects.iter().enumerate() {
                    if self.last_cursor.x >= *x0 && self.last_cursor.x < *x1 {
                        // Close hit-box at the right end of the tab.
                        let close_x0 = x1 - TAB_PAD - CLOSE_BOX;
                        if self.last_cursor.x >= close_x0 {
                            self.pending.push(TabEvent::Close(i));
                        } else {
                            self.pending.push(TabEvent::Select(i));
                        }
                        self.dirty = true;
                        *handled = true;
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "TabBar"
    }

    fn test_id(&self) -> Option<&str> {
        Some("tab_bar")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}
