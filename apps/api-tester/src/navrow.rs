//! A focusable, accessible sidebar list row.
//!
//! The collections tree, saved-request rows, and history rows used to be
//! painted straight into the sidebar and hit-tested from a private
//! `Vec<(Rect, …)>` table, so the keyboard could not reach them and a screen
//! reader could not see them. `NavRow` is the retained replacement: a real
//! [`Widget`] with a [`Role::Button`] accessibility node, keyboard/AT
//! activation, and a themed focus ring. The app hand-positions each row over
//! the sidebar it still paints, exactly as it does for the other shared
//! controls.
//!
//! It preserves the original per-row look — a disclosure caret + count for a
//! collection header, a coloured method tag for a saved request, a method
//! tag + status for a history entry — so the sidebar renders identically
//! while becoming fully navigable.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::color_compat::ColorCompat;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::model::HttpMethod;
use crate::palette::{method_color, method_short, status_class, status_color};

/// Row height in logical px — matches the sidebar's original `ROW_H`.
pub const ROW_H: f64 = 30.0;

/// Which of the three sidebar row shapes this row paints.
#[derive(Clone)]
pub enum NavKind {
    Disclosure {
        expanded: bool,
    },
    Divider,
    /// A foldable collection header: disclosure caret + name + request count.
    Collection {
        expanded: bool,
        count: usize,
    },
    /// A saved request: coloured method tag + name, highlighted when active.
    Request {
        method: HttpMethod,
        selected: bool,
    },
    /// A history entry: coloured method tag + path + coloured status code.
    History {
        method: HttpMethod,
        status: u16,
    },
}

/// A single focusable sidebar row.
pub struct NavRow {
    id: String,
    kind: NavKind,
    /// The visible label (collection name, request name, or history path).
    label: String,
    /// The already-composed screen-reader name (e.g. "GET List users").
    accessible_name: String,
    is_focused: bool,
    rect: Rect,
    /// Last cursor position (logical), tracked from `CursorMoved` so a
    /// left-press can self-gate to the row under the pointer — the container
    /// forwards the whole event stream to every row.
    last_cursor: Point,
    /// Fired on a completed activation — left-click, Enter/Space, or an AT
    /// `Activate` action. The app supplies a closure that pushes the row's
    /// command onto its sink.
    on_activate: Option<Box<dyn FnMut()>>,
    on_adjust: Option<Box<dyn FnMut(f64)>>,
}

impl NavRow {
    pub fn new(
        id: impl Into<String>,
        kind: NavKind,
        label: impl Into<String>,
        accessible_name: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            accessible_name: accessible_name.into(),
            is_focused: false,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            on_activate: None,
            on_adjust: None,
        }
    }

    /// Attach the activation handler (mirrors `Button::on_click`).
    pub fn on_activate<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    pub fn on_adjust(mut self, f: impl FnMut(f64) + 'static) -> Self {
        self.on_adjust = Some(Box::new(f));
        self
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if let NavKind::Disclosure { expanded: value } = &mut self.kind {
            *value = expanded;
        }
    }

    pub fn set_accessible_name(&mut self, name: String) {
        self.accessible_name = name;
    }

    /// Place the row at an absolute rect — the app positions every row over
    /// the sidebar it paints.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Whether keyboard or assistive navigation currently owns this row.
    /// The rail uses this to keep the focused resource inside its local
    /// scrolling viewport.
    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn activate(&mut self) {
        if let Some(cb) = self.on_activate.as_mut() {
            cb();
        }
    }
}

impl Widget for NavRow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A leaf node so the row is registered in `node_map` (for focus
        // order + the a11y tree); its taffy geometry is unused because the
        // app hand-positions it via `apply_layout_external`.
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see
        // `ui::widget::TaffyTreeExt`'s doc comment), and `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so there is
        // nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ROW_H as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout entry for our own node would be a taffy bug, not a
        // recoverable app condition — skip this frame's rect update rather
        // than panic; the next frame's layout pass will have one.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let r = self.rect;

        // Focus ring — a rounded outline that reads at a glance and is the
        // one visible cue a hand-painted row never had.
        if self.is_focused {
            let ring = RoundedRect::from_rect(
                Rect::new(r.x0, r.y0 + 1.0, r.x1, r.y1 - 1.0),
                f64::from(theme.tokens.radii.md),
            );
            scene.stroke(&Stroke::new(2.0), t, theme.border_focus, None, &ring);
        }

        match &self.kind {
            NavKind::Divider => {
                // The divider rules the raised rail it sits in; its colour is
                // derived against that surface rather than declared.
                let color = if self.is_focused {
                    theme.border_focus
                } else {
                    ui::surface::edge_on(theme.tokens.colors.surfaces.raised)
                };
                scene.fill(
                    Fill::NonZero,
                    t,
                    color,
                    None,
                    &Rect::new(r.x0, r.center().y, r.x1, r.center().y + 1.0),
                );
                scene.fill(
                    Fill::NonZero,
                    t,
                    color,
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(
                            r.center().x - 22.0,
                            r.center().y - 2.0,
                            r.center().x + 22.0,
                            r.center().y + 2.0,
                        ),
                        2.0,
                    ),
                );
            }
            NavKind::Disclosure { expanded } => {
                let label = format!("{} {}", if *expanded { "▾" } else { "▸" }, self.label);
                let style =
                    tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
                let (_, height) = tr.measure_style(&label, Some((r.width() - 12.0) as f32), &style);
                tr.draw_style(
                    scene,
                    &label,
                    theme.tokens.colors.text_primary,
                    t * Affine::translate((r.x0 + 6.0, r.center().y - f64::from(height) / 2.0)),
                    Some((r.width() - 12.0) as f32),
                    &style,
                );
            }
            NavKind::Collection { expanded, count } => {
                Self::draw_collection_row(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    r,
                    &self.label,
                    *expanded,
                    *count,
                );
            }
            NavKind::Request { method, selected } => {
                Self::draw_request_row(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    r,
                    &self.label,
                    *method,
                    *selected,
                );
            }
            NavKind::History { method, status } => {
                Self::draw_history_row(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    r,
                    &self.label,
                    *method,
                    *status,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                // `MouseInput` carries no position, so gate on the cursor we
                // tracked from `CursorMoved` — only the row under the pointer
                // activates.
                *ctx.focus_target = Some(ctx.node_id);
                self.activate();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                if matches!(self.kind, NavKind::Divider) {
                    let delta = match key {
                        Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => Some(24.0),
                        Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => Some(-24.0),
                        _ => None,
                    };
                    if let Some(delta) = delta
                        && let Some(adjust) = &mut self.on_adjust
                    {
                        adjust(delta);
                        *ctx.handled = true;
                        return;
                    }
                }
                let is_activate = matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
                    || matches!(key, Key::Character(s) if s.as_str() == " ");
                if is_activate {
                    self.activate();
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "NavRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(if matches!(self.kind, NavKind::Divider) {
                winit::window::CursorIcon::RowResize
            } else {
                winit::window::CursorIcon::Pointer
            })
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut state = AccessState::new().focusable(true).focused(self.is_focused);
        match &self.kind {
            NavKind::Collection { expanded, .. } => {
                state = state.expanded(*expanded);
            }
            NavKind::Request { selected, .. } => {
                state = state.selected(*selected);
            }
            NavKind::History { .. } | NavKind::Divider => {}
            NavKind::Disclosure { expanded } => state = state.expanded(*expanded),
        }
        Some(
            AccessNode::new(if matches!(self.kind, NavKind::Divider) {
                Role::Slider
            } else {
                Role::Button
            })
            .with_name(self.accessible_name.clone())
            .with_action_target(self.id.clone())
            .with_state(state),
        )
    }

    /// The row publishes its id as its semantic action target, so assistive
    /// technology and agents reach the same activation and adjustment a
    /// pointer or keyboard does.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        target == self.id && self.perform_action(action, state, path)
    }

    fn perform_action(
        &mut self,
        action: &Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if let Some(adjust) = &mut self.on_adjust {
            match action {
                Action::Increment => {
                    adjust(24.0);
                    return true;
                }
                Action::Decrement => {
                    adjust(-24.0);
                    return true;
                }
                _ => {}
            }
        }
        if matches!(action, Action::Activate) {
            self.activate();
            true
        } else {
            false
        }
    }
}

impl NavRow {
    /// Navigation stays one line: measure the exact elided label, then paint
    /// without a wrapping width. The retained accessibility name is untouched.
    #[expect(
        clippy::indexing_slicing,
        clippy::string_slice,
        reason = "the binary search stays within `boundaries`, whose entries are char boundaries of `label`"
    )]
    fn single_line_label(
        tr: &mut TextRenderer,
        label: &str,
        width: f64,
        style: &TextStyle,
    ) -> String {
        let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
        let fits = |tr: &mut TextRenderer, text: &str| {
            f64::from(tr.measure_style(text, None, style).0) <= width.max(0.0)
        };
        if fits(tr, &label) {
            return label;
        }
        if !fits(tr, "…") {
            return String::new();
        }
        let boundaries: Vec<_> = label
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(label.len()))
            .collect();
        let (mut low, mut high) = (0, boundaries.len() - 1);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if fits(tr, &format!("{}…", &label[..boundaries[mid]])) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        format!("{}…", &label[..boundaries[low]])
    }

    fn draw_label(ctx: &mut PaintCtx<'_>, r: Rect, label: &str, color: Color, style: &TextStyle) {
        let label = Self::single_line_label(ctx.text, label, r.width(), style);
        let (_, height) = ctx.text.measure_style(&label, None, style);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &r,
        );
        ctx.text(
            &label,
            style,
            color,
            Point::new(r.x0, r.center().y - f64::from(height) / 2.0),
            None,
        );
        ctx.scene.pop_layer();
    }

    fn draw_collection_row(
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        label: &str,
        expanded: bool,
        count: usize,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let caret = if expanded { "▾" } else { "▸" };
        let (_, caret_h) = ctx.text.measure(caret, 12.0, None);
        ctx.text(
            caret,
            &TextStyle::new(12.0),
            c.text_dim,
            Point::new(r.x0 + 6.0, r.center().y - f64::from(caret_h) / 2.0),
            None,
        );
        let count = count.to_string();
        let (cw, ch) = ctx.text.measure(&count, 11.0, None);
        let style = ctx.text.renderer_input_style(
            &TextStyle::new(theme.tokens.typography.scale.base).with_weight(TextWeight::Semibold),
        );
        Self::draw_label(
            ctx,
            Rect::new(r.x0 + 24.0, r.y0, r.x1 - f64::from(cw) - 14.0, r.y1),
            label,
            c.text_primary,
            &style,
        );
        ctx.text(
            &count,
            &TextStyle::new(11.0),
            c.text_dim,
            Point::new(
                r.x1 - f64::from(cw) - 6.0,
                r.center().y - f64::from(ch) / 2.0,
            ),
            None,
        );
    }

    fn draw_request_row(
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        label: &str,
        method: HttpMethod,
        selected: bool,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        if selected {
            ctx.fill_round(
                Rect::new(r.x0, r.y0 + 2.0, r.x1, r.y1 - 2.0),
                f64::from(theme.tokens.radii.md),
                theme.accent_primary.with_alpha_factor(0.18),
            );
        }
        let (mw, mh) =
            ctx.text
                .measure_weighted(method_short(method), 10.0, None, TextWeight::Bold);
        let label_x = r.x0 + 28.0 + f64::from(mw).max(24.0) + 12.0;
        ctx.text(
            method_short(method),
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            method_color(theme, method),
            Point::new(r.x0 + 28.0, r.center().y - f64::from(mh) / 2.0),
            None,
        );
        let style = ctx
            .text
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
        Self::draw_label(
            ctx,
            Rect::new(label_x, r.y0, r.x1 - 8.0, r.y1),
            label,
            if selected { c.text_primary } else { c.text_dim },
            &style,
        );
    }

    fn draw_history_row(
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        label: &str,
        method: HttpMethod,
        status: u16,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (mw, mh) =
            ctx.text
                .measure_weighted(method_short(method), 10.0, None, TextWeight::Bold);
        let status_text = status.to_string();
        let (sw, sh) = ctx
            .text
            .measure_weighted(&status_text, 11.0, None, TextWeight::Semibold);
        let label_x = r.x0 + 6.0 + f64::from(mw).max(28.0) + 12.0;
        ctx.text(
            method_short(method),
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            method_color(theme, method),
            Point::new(r.x0 + 6.0, r.center().y - f64::from(mh) / 2.0),
            None,
        );
        let style = ctx
            .text
            .renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
        Self::draw_label(
            ctx,
            Rect::new(label_x, r.y0, r.x1 - f64::from(sw) - 14.0, r.y1),
            label,
            c.text_dim,
            &style,
        );
        ctx.text(
            &status_text,
            &TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            status_color(theme, status_class(status)),
            Point::new(
                r.x1 - f64::from(sw) - 6.0,
                r.center().y - f64::from(sh) / 2.0,
            ),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_long_navigation_glyphs_fit_their_row() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let style = tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
            let height =
                (f64::from(tr.measure_style("Ag", None, &style).1).ceil() + 12.0).max(ROW_H);
            let rect = Rect::new(0.0, 0.0, 240.0, height);
            for kind in [
                NavKind::Request {
                    method: HttpMethod::Post,
                    selected: true,
                },
                NavKind::History {
                    method: HttpMethod::Get,
                    status: 200,
                },
            ] {
                let label = "a very long saved request or history path with Unicode café and many more words";
                let mut row = NavRow::new("long", kind, label, label);
                row.apply_layout_external(rect);
                let mut scene = Scene::new();
                row.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::default(),
                );
                assert_eq!(row.accessibility().unwrap().name.as_deref(), Some(label));
                for run in &scene.encoding().resources.glyph_runs {
                    for glyph in &scene.encoding().resources.glyphs[run.glyphs.clone()] {
                        let baseline = f64::from(run.transform.translation[1] + glyph.y);
                        assert!(
                            baseline < rect.y1,
                            "{scale}x row ends at {}, glyph baseline {baseline} escapes",
                            rect.y1
                        );
                    }
                }
            }
        }
    }
}
